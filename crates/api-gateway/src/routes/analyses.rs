//! AI analyses routes.
//!
//! Ported from `api-gateway/routes/analyses.py`. `generate` is the full
//! pipeline: pull the denial and its code definitions, retrieve matching payer
//! policy with the RAG engine, build the prompt, and hand it to the LLM
//! service (which stores the parsed result itself).

use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use denial_auth::rbac::{Principal, PrincipalKind};
use denial_common::clients::LLMServiceClient;
use denial_common::error::AppError;
use denial_db::pgjson::row_to_json;
use serde::Deserialize;
use sqlx::{QueryBuilder, Row};
use std::collections::HashSet;
use uuid::Uuid;

use crate::state::AppState;

/// Per-caller key for the in-process rate limiter, matching `ingestion`.
fn limit_key(principal: &Principal) -> String {
    if principal.kind == PrincipalKind::User {
        if let Some(ref uid) = principal.user_id {
            return format!("user:{uid}");
        }
    }
    if !principal.username.is_empty() && principal.username != "anonymous" {
        return format!("svc:{}", principal.username);
    }
    format!("ip:{}", principal.ip.as_deref().unwrap_or("unknown"))
}

#[derive(Deserialize)]
pub struct ListAnalysesQuery {
    pub denial_id: Option<Uuid>,
    pub claim_id: Option<Uuid>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    50
}

fn organization_id(principal: &Principal) -> Result<Uuid, AppError> {
    principal
        .organization_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(AppError::Forbidden)
}

#[derive(Deserialize)]
pub struct StoreAnalysis {
    pub denial_id: String,
    pub claim_id: String,
    pub model_name: String,
    #[serde(default)]
    pub provider_name: String,
    #[serde(default)]
    pub provider_version: String,
    #[serde(default)]
    pub prompt_template_version: String,
    pub raw_prompt: String,
    pub raw_response: String,
    pub parsed_result: serde_json::Value,
    #[serde(default)]
    pub prompt_tokens: i64,
    #[serde(default)]
    pub completion_tokens: i64,
    #[serde(default)]
    pub total_tokens: i64,
    #[serde(default)]
    pub allowed_evidence_ids: Vec<String>,
    /// Why the analysis is degraded, if it is; see FALLBACK_REASONS.
    #[serde(default)]
    pub fallback_reason: Option<String>,
}

/// Values of `ai_analyses.fallback_reason`. Anything else from a caller is
/// dropped rather than stored.
/// The description an organization has imported for a code, if any. CPT and
/// ICD-10 descriptions are licensed, so these tables are often empty and the
/// query then carries the bare code.
async fn code_description(pool: &sqlx::PgPool, table: &str, code: Option<&str>) -> Option<String> {
    let code = code?;
    let sql = format!("SELECT description FROM {table} WHERE code = $1 AND is_active");
    sqlx::query_scalar(&sql)
        .bind(code)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
}

async fn code_descriptions(
    pool: &sqlx::PgPool,
    table: &str,
    codes: &[String],
) -> Vec<Option<String>> {
    let mut out = Vec::with_capacity(codes.len());
    for code in codes {
        out.push(code_description(pool, table, Some(code)).await);
    }
    out
}

/// The retrieval query for a denial.
///
/// Bare codes retrieve badly: the embedding model has no idea that 80053 is a
/// metabolic panel, and against the test knowledge base a query of payer name
/// and codes put the right document first 57% of the time against 93% for this
/// form (`scripts/eval_retrieval.py`). Each code is labelled the way documents
/// write it ("CPT 80053"), which is also what the RAG engine anchors on, and
/// the CARC description supplies the words the denial is actually about. The
/// payer name is left out: retrieval is already scoped to the payer, and
/// repeating it pulled in that payer's unrelated documents.
fn build_search_query(
    cpt: Option<&str>,
    cpt_description: Option<&str>,
    icd: &[String],
    icd_descriptions: &[Option<String>],
    carc: Option<&str>,
    carc_description: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(code) = cpt.filter(|c| !c.is_empty()) {
        parts.push(match cpt_description {
            Some(d) if !d.is_empty() => format!("CPT {code} {d}"),
            _ => format!("CPT {code}"),
        });
    }
    for (i, code) in icd.iter().filter(|c| !c.is_empty()).enumerate() {
        parts.push(match icd_descriptions.get(i).and_then(|d| d.as_deref()) {
            Some(d) if !d.is_empty() => format!("ICD-10 {code} {d}"),
            _ => format!("ICD-10 {code}"),
        });
    }
    if let Some(code) = carc.filter(|c| !c.is_empty()) {
        parts.push(match carc_description {
            Some(d) if !d.is_empty() => format!("CARC {code}: {d}"),
            _ => format!("CARC {code}"),
        });
    }
    parts.join(" ")
}

pub const FALLBACK_REASONS: &[&str] = &["llm_error", "retrieval_error", "no_evidence"];

#[derive(Clone, Deserialize)]
pub struct GenerateAnalysisRequest {
    pub denial_id: String,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
}

fn default_temperature() -> f32 {
    0.3
}

/// Provider boundary for the primary LLM and deterministic fallback.
#[allow(async_fn_in_trait)]
trait RecommendationProvider: Send + Sync {
    async fn recommend(&self) -> Result<serde_json::Value, AppError>;
}

struct LlmRecommendationProvider<'a> {
    client: &'a LLMServiceClient,
    request: serde_json::Value,
}

impl RecommendationProvider for LlmRecommendationProvider<'_> {
    async fn recommend(&self) -> Result<serde_json::Value, AppError> {
        self.client.analyze_denial(&self.request).await
    }
}

/// Safe, explainable fallback when an AI provider is unavailable. This keeps
/// the denial workflow moving and deliberately prefers reviewable work over a
/// speculative appeal or write-off.
struct DeterministicRecommendationProvider<'a> {
    cagc: &'a str,
    carc: &'a str,
    description: &'a str,
    /// A payer that pays after this claim's payer, if the claim names one.
    next_payer: Option<&'a str>,
    ncci_evidence: Option<&'a str>,
}

impl RecommendationProvider for DeterministicRecommendationProvider<'_> {
    async fn recommend(&self) -> Result<serde_json::Value, AppError> {
        let secondary_step;
        let (category, action, step) = if let (true, Some(payer)) =
            (self.cagc == "PR", self.next_payer)
        {
            secondary_step = format!(
                "Send the balance to {payer} with this remittance before billing the patient; bill the patient only for what it leaves."
            );
            (
                "patient_responsibility",
                "bill_secondary",
                secondary_step.as_str(),
            )
        } else if self.cagc == "PR" {
            (
            "patient_responsibility",
            "bill_patient",
            "Move the payer-assigned balance to the patient statement and verify the EOB amount.",
        )
        } else if matches!(self.carc, "16" | "17" | "18" | "50" | "96") {
            ("missing_info", "clinical_documentation", "Review the remittance advice and submit the requested clinical or claim documentation.")
        } else if matches!(self.carc, "197" | "198" | "204") {
            ("lack_of_preauth", "clinical_documentation", "Verify authorization requirements and gather authorization or medical-necessity support before resubmission.")
        } else if self.carc == "97" {
            ("bundled_service", "coding_correction", self.ncci_evidence.unwrap_or("Review the billed service pair against current NCCI PTP edits and verify whether a distinct-service modifier is supported."))
        } else {
            ("other", "coding_correction", "Review the claim, remittance advice, coding, modifiers, and payer edits before corrected resubmission.")
        };
        Ok(serde_json::json!({
            "explanation": format!("Deterministic guidance based on {} {}.", self.carc, self.description),
            "denial_category": category,
            "required_action": action,
            "root_cause_summary": "AI provider unavailable; generated from the adjustment group and reason code.",
            "action_plan": {"type": action, "requires": ["remittance advice review"]},
            "steps": [{"step": 1, "action": step}],
            "needs_appeal": false,
            "draft_appeal_letter": "",
            "confidence_score": 0.45,
            "provider": "deterministic_rules"
        }))
    }
}

/// The full ai_analyses column list, with `confidence_score` cast off `numeric`
/// so it comes back as a JSON number rather than dropping out.
const ANALYSIS_COLUMNS: &str =
    "aa.id, aa.denial_id, aa.claim_id, aa.model_name, aa.provider_name, \
    aa.provider_version, aa.prompt_template_version, \
    aa.prompt_tokens, aa.completion_tokens, aa.total_tokens, aa.system_prompt_template, \
    aa.raw_prompt, aa.raw_response, aa.explanation, aa.denial_category, aa.root_cause_summary, \
    aa.required_action, aa.action_plan, aa.steps, aa.citations, aa.needs_appeal, aa.draft_appeal_letter, \
    aa.confidence_score::float8 AS confidence_score, aa.fallback_reason, aa.created_at, aa.updated_at";

/// Keep only unique IDs the model cited from the retrieved-evidence allowlist.
/// The subsequent database query scopes those IDs to the analyzed claim's
/// organization before any citation metadata is persisted.
fn cited_evidence_ids(
    parsed_result: &serde_json::Value,
    allowed: &[String],
) -> Result<Vec<Uuid>, AppError> {
    let allowed: HashSet<&str> = allowed.iter().map(String::as_str).collect();
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    let Some(cited) = parsed_result.get("evidence_ids") else {
        return Ok(ids);
    };
    let cited = cited.as_array().ok_or_else(|| {
        AppError::BadRequest("parsed_result evidence_ids must be an array of strings".into())
    })?;

    for evidence_id in cited {
        let evidence_id = evidence_id.as_str().ok_or_else(|| {
            AppError::BadRequest("parsed_result evidence_ids must be an array of strings".into())
        })?;
        if !allowed.contains(evidence_id) {
            return Err(AppError::BadRequest(
                "cited evidence was not retrieved for this analysis".into(),
            ));
        }
        let evidence_id = Uuid::parse_str(evidence_id)
            .map_err(|_| AppError::BadRequest("cited evidence_id must be a UUID".into()))?;
        if seen.insert(evidence_id) {
            ids.push(evidence_id);
        }
    }

    Ok(ids)
}

async fn resolve_citations(
    state: &AppState,
    claim_id: Uuid,
    cited_evidence_ids: &[Uuid],
) -> Result<Vec<serde_json::Value>, AppError> {
    if cited_evidence_ids.is_empty() {
        return Ok(Vec::new());
    }

    let rows = sqlx::query(
        "SELECT kc.id AS evidence_id, kc.knowledge_document_id AS document_id, \
         kd.source_type, kc.chunk_index \
         FROM knowledge_chunks kc \
         JOIN knowledge_documents kd ON kd.id = kc.knowledge_document_id \
         JOIN claims c ON c.id = $1 \
         WHERE kc.id = ANY($2) AND kd.organization_id = c.organization_id \
         ORDER BY array_position($2::uuid[], kc.id)",
    )
    .bind(claim_id)
    .bind(cited_evidence_ids)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;

    if rows.len() != cited_evidence_ids.len() {
        return Err(AppError::BadRequest(
            "cited evidence must belong to the claim organization".into(),
        ));
    }

    Ok(rows
        .iter()
        .map(|row| {
            resolve_citation_metadata(
                row.try_get("evidence_id").unwrap_or_default(),
                row.try_get("document_id").unwrap_or_default(),
                &row.try_get::<String, _>("source_type").unwrap_or_default(),
                row.try_get("chunk_index").unwrap_or_default(),
            )
        })
        .collect())
}

fn resolve_citation_metadata(
    evidence_id: Uuid,
    document_id: Uuid,
    source_type: &str,
    chunk_index: i32,
) -> serde_json::Value {
    serde_json::json!({
        "evidence_id": evidence_id,
        "document_id": document_id,
        "source_type": source_type,
        "chunk_index": chunk_index,
    })
}

pub async fn list_analyses(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<ListAnalysesQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let organization_id = organization_id(&principal)?;
    let mut qb = QueryBuilder::<sqlx::Postgres>::new(format!(
        "SELECT {ANALYSIS_COLUMNS}, d.cpt_code, d.carc_code, \
         c.claim_number, c.patient_name, c.payer_name \
         FROM ai_analyses aa \
         JOIN denials d ON d.id = aa.denial_id \
         JOIN claims c ON c.id = aa.claim_id"
    ));

    qb.push(" WHERE c.organization_id = ")
        .push_bind(organization_id);
    if let Some(denial_id) = params.denial_id {
        qb.push(" AND aa.denial_id = ").push_bind(denial_id);
    }
    if let Some(claim_id) = params.claim_id {
        qb.push(" AND aa.claim_id = ").push_bind(claim_id);
    }

    qb.push(" ORDER BY aa.created_at DESC LIMIT ")
        .push_bind(params.limit.clamp(1, 500));

    let rows = qb
        .build()
        .fetch_all(&state.pool)
        .await
        .map_err(AppError::Db)?;
    Ok(Json(rows.iter().map(row_to_json).collect()))
}

pub async fn store_analysis(
    State(state): State<AppState>,
    Json(a): Json<StoreAnalysis>,
) -> Result<Json<serde_json::Value>, AppError> {
    let denial_id = Uuid::parse_str(a.denial_id.trim())
        .map_err(|_| AppError::BadRequest("denial_id must be a UUID".into()))?;
    let claim_id = Uuid::parse_str(a.claim_id.trim())
        .map_err(|_| AppError::BadRequest("claim_id must be a UUID".into()))?;

    let pr = &a.parsed_result;
    let get_str = |k: &str| pr.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let action_plan =
        serde_json::to_string(pr.get("action_plan").unwrap_or(&serde_json::Value::Null))
            .unwrap_or_else(|_| "null".into());
    let steps = serde_json::to_string(pr.get("steps").unwrap_or(&serde_json::Value::Null))
        .unwrap_or_else(|_| "null".into());
    let needs_appeal = pr
        .get("needs_appeal")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let draft_appeal_letter = pr
        .get("draft_appeal_letter")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let confidence_score = pr
        .get("confidence_score")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let playbook_id = pr
        .get("playbook")
        .and_then(|p| p.get("id"))
        .and_then(|id| id.as_str())
        .and_then(|id| Uuid::parse_str(id).ok());
    let cited_evidence_ids = cited_evidence_ids(&a.parsed_result, &a.allowed_evidence_ids)?;
    let citations = resolve_citations(&state, claim_id, &cited_evidence_ids).await?;

    // `parsed_result` is provider-controlled data. A referenced playbook must
    // belong to the same organization as the claim being analyzed, even for a
    // trusted internal caller, so an arbitrary UUID cannot create a cross-
    // tenant relationship in `ai_analyses`.
    if let Some(playbook_id) = playbook_id {
        let belongs: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM institutional_playbooks p \
             JOIN claims c ON c.id = $2 \
             WHERE p.id = $1 AND p.organization_id = c.organization_id)",
        )
        .bind(playbook_id)
        .bind(claim_id)
        .fetch_one(&state.pool)
        .await
        .map_err(AppError::Db)?;
        if !belongs {
            return Err(AppError::BadRequest(
                "playbook must belong to the claim organization".into(),
            ));
        }
    }

    // Raw prompt/response text can carry PHI (a claim reference, payer policy
    // excerpts). Persist it only when explicitly enabled; otherwise store NULL
    // and keep just the structured, redacted result (plan §17.3, §16.4).
    let store_raw = state.config.store_raw_ai_artifacts;
    let row = sqlx::query(
        "INSERT INTO ai_analyses \
            (denial_id, claim_id, playbook_id, model_name, provider_name, provider_version, prompt_template_version, \
             prompt_tokens, completion_tokens, total_tokens, \
             system_prompt_template, raw_prompt, raw_response, \
              explanation, denial_category, required_action, root_cause_summary, action_plan, steps, \
              citations, needs_appeal, draft_appeal_letter, confidence_score, fallback_reason) \
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18::jsonb, $19::jsonb, \
                   $20::jsonb, $21, $22, $23, $24) \
           RETURNING id, denial_id, claim_id, model_name, provider_name, provider_version, prompt_template_version, \
               prompt_tokens, completion_tokens, \
               total_tokens, system_prompt_template, raw_prompt, raw_response, explanation, \
               denial_category, root_cause_summary, required_action, action_plan, steps, \
               citations, needs_appeal, draft_appeal_letter, confidence_score::float8 AS confidence_score, \
              fallback_reason, created_at, updated_at",
    )
    .bind(denial_id)
    .bind(claim_id)
    .bind(playbook_id)
    .bind(&a.model_name)
    .bind(&a.provider_name)
    .bind(&a.provider_version)
    .bind(&a.prompt_template_version)
    .bind(a.prompt_tokens)
    .bind(a.completion_tokens)
    .bind(a.total_tokens)
    .bind(&a.prompt_template_version)
    .bind(if store_raw { Some(&a.raw_prompt) } else { None })
    .bind(if store_raw { Some(&a.raw_response) } else { None })
    .bind(get_str("explanation"))
    .bind(get_str("denial_category"))
    .bind(get_str("required_action"))
    .bind(get_str("root_cause_summary"))
    .bind(action_plan)
    .bind(steps)
    .bind(serde_json::to_string(&citations).map_err(|e| AppError::Internal(e.to_string()))?)
    .bind(needs_appeal)
    .bind(draft_appeal_letter)
    .bind(confidence_score)
    .bind(
        a.fallback_reason
            .as_deref()
            .filter(|reason| FALLBACK_REASONS.contains(reason)),
    )
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    Ok(Json(row_to_json(&row)))
}

pub async fn generate_analysis(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<GenerateAnalysisRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let key = limit_key(&principal);
    if !state.analyses_limiter.allow(&key) {
        return Err(AppError::RateLimited {
            retry_after: state.analyses_limiter.retry_after(&key),
        });
    }

    Ok(Json(
        generate_analysis_for_request(state, request, Some(principal)).await?,
    ))
}

async fn generate_analysis_for_request(
    state: AppState,
    request: GenerateAnalysisRequest,
    principal: Option<Principal>,
) -> Result<serde_json::Value, AppError> {
    let organization_id = principal
        .as_ref()
        .map(organization_id)
        .transpose()?
        .ok_or(AppError::Forbidden)?;
    let denial_id = Uuid::parse_str(request.denial_id.trim())
        .map_err(|_| AppError::BadRequest("denial_id must be a UUID".into()))?;

    let denial = sqlx::query(
        "SELECT d.cagc, d.cpt_code, d.carc_code, d.rarc_code, d.claim_id, \
                c.claim_number, c.patient_name, c.payer_name, c.icd_10_codes, c.next_payer_name, \
                c.service_from, c.payer_id_number, \
                cc.description AS carc_description, \
                rc.description AS rarc_description \
         FROM denials d \
         JOIN claims c ON c.id = d.claim_id \
         LEFT JOIN carc_codes cc ON cc.code = d.carc_code \
         LEFT JOIN rarc_codes rc ON rc.code = d.rarc_code \
         WHERE d.id = $1 AND c.organization_id = $2",
    )
    .bind(denial_id)
    .bind(organization_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;

    let claim_db_id: Uuid = denial
        .try_get("claim_id")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let claim_number: String = denial.try_get("claim_number").unwrap_or_default();
    let payer_name: String = denial.try_get("payer_name").unwrap_or_default();
    let cpt_code: Option<String> = denial.try_get("cpt_code").ok().flatten();
    let carc_code: Option<String> = denial.try_get("carc_code").ok().flatten();
    let cagc: Option<String> = denial.try_get("cagc").ok().flatten();
    let rarc_code: Option<String> = denial.try_get("rarc_code").ok().flatten();
    let carc_description: Option<String> = denial.try_get("carc_description").ok().flatten();
    let rarc_description: Option<String> = denial.try_get("rarc_description").ok().flatten();
    let next_payer_name: Option<String> = denial.try_get("next_payer_name").ok().flatten();
    let service_from: Option<chrono::NaiveDate> = denial.try_get("service_from").ok().flatten();
    let payer_id_number: Option<String> = denial.try_get("payer_id_number").ok().flatten();
    let icd_codes: Vec<String> = denial
        .try_get::<Option<Vec<String>>, _>("icd_10_codes")
        .ok()
        .flatten()
        .unwrap_or_default();

    // All of them, not just the first: a medical-necessity denial usually
    // turns on the secondary diagnosis. Capped so a long list cannot drown
    // out the CPT and CARC terms.
    let icd: Vec<String> = icd_codes.iter().take(5).cloned().collect();
    let cpt = cpt_code.as_deref().unwrap_or("");
    let carc = carc_code.as_deref().unwrap_or("");
    let ncci_evidence: Option<String> = if carc == "97" && !cpt.is_empty() {
        sqlx::query("SELECT p.column_1_code, p.column_2_code, p.modifier_indicator FROM ncci_ptp_edits p JOIN denials other ON other.claim_id=$1 AND other.id<>$2 AND other.cpt_code IN (p.column_1_code,p.column_2_code) WHERE $3 IN (p.column_1_code,p.column_2_code) AND (p.termination_date IS NULL OR p.termination_date >= CURRENT_DATE) ORDER BY p.effective_date DESC NULLS LAST LIMIT 1")
            .bind(claim_db_id).bind(denial_id).bind(cpt).fetch_optional(&state.pool).await.map_err(AppError::Db)?
            .map(|row| format!("NCCI PTP edit: {} / {}; modifier indicator {}. Verify documented distinct-service circumstances before submitting a corrected claim.", row.try_get::<String,_>("column_1_code").unwrap_or_default(), row.try_get::<String,_>("column_2_code").unwrap_or_default(), row.try_get::<i16,_>("modifier_indicator").unwrap_or(9)))
    } else {
        None
    };
    let cpt_description = code_description(&state.pool, "cpt_codes", cpt_code.as_deref()).await;
    let icd_descriptions = code_descriptions(&state.pool, "icd10_codes", &icd).await;
    let search_query = build_search_query(
        cpt_code.as_deref(),
        cpt_description.as_deref(),
        &icd,
        &icd_descriptions,
        carc_code.as_deref(),
        carc_description.as_deref(),
    );

    let mut retrieval_failed = false;
    let policy_texts: Vec<String> = match state
        .rag
        // The RAG service refuses unscoped searches. Forward the organization
        // established by this route's authorization check so retrieval remains
        // tenant-isolated and can return the knowledge evidence for this claim.
        .search(
            &search_query,
            5,
            serde_json::json!({
                "payer": payer_name,
                "payer_id_number": payer_id_number,
                // Only policies in effect on the date of service count as
                // evidence for this claim.
                "effective_on": service_from.map(|d| d.to_string()),
                "organization_id": organization_id,
            }),
        )
        .await
    {
        Ok(v) => v
            .get("results")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|r| {
                        let content = r
                            .get("content")
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string();
                        let evidence_id = r.get("id").and_then(|id| id.as_str()).unwrap_or("");
                        format!("[evidence:{evidence_id}]\n{content}")
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Err(e) => {
            tracing::warn!("RAG search failed: {e}");
            retrieval_failed = true;
            Vec::new()
        }
    };
    let retrieval_fallback = if retrieval_failed {
        Some("retrieval_error")
    } else if policy_texts.is_empty() {
        Some("no_evidence")
    } else {
        None
    };

    let prompt = state
        .rag
        .build_prompt(&serde_json::json!({
            "claim_id": claim_number,
            "payer_name": payer_name,
            "next_payer_name": next_payer_name,
            "cpt_code": cpt,
            "icd10_code": icd.join(" "),
            "cagc": cagc.as_deref().unwrap_or(""),
            "carc_code": carc,
            "carc_definition": carc_description.as_deref().unwrap_or("Unknown"),
            "rarc_code": rarc_code.as_deref().unwrap_or(""),
            "rarc_definition": rarc_description.as_deref().unwrap_or("Unknown"),
            "retrieved_policies": policy_texts,
            "phi_disclosure_level": crate::routes::settings::current_level(&state.pool).await,
        }))
        .await?;
    let allowed_evidence_ids: Vec<String> = policy_texts
        .iter()
        .filter_map(|policy| policy.strip_prefix("[evidence:"))
        .filter_map(|value| value.split_once(']'))
        .map(|(id, _)| id.to_string())
        .collect();

    let llm_provider = LlmRecommendationProvider {
        client: &state.llm,
        request: serde_json::json!({
            "denial_id": request.denial_id,
            // ai_analyses.claim_id is a UUID FK to claims(id); the human-
            // readable claim number is already baked into the prompt text.
            "claim_id": claim_db_id.to_string(),
            "prompt": prompt,
            "allowed_evidence_ids": allowed_evidence_ids,
            "fallback_reason": retrieval_fallback,
            "temperature": request.temperature,
        }),
    };
    let llm_result = llm_provider.recommend().await;

    let llm_result = match llm_result {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!("AI provider unavailable; using deterministic recommendation: {error}");
            let mut parsed = DeterministicRecommendationProvider {
                cagc: cagc.as_deref().unwrap_or(""),
                carc,
                description: carc_description
                    .as_deref()
                    .unwrap_or("the payer's adjustment reason"),
                next_payer: next_payer_name.as_deref(),
                ncci_evidence: ncci_evidence.as_deref(),
            }
            .recommend()
            .await?;
            let playbook = sqlx::query(
                "SELECT id, name, version, recommendation FROM institutional_playbooks \
                 WHERE organization_id=$1 AND status='approved' \
                   AND (triggers->>'carc_code' IS NULL OR triggers->>'carc_code'=$2) \
                   AND (triggers->>'cagc' IS NULL OR triggers->>'cagc'=$3) \
                   AND (triggers->>'payer_name' IS NULL OR lower(triggers->>'payer_name')=lower($4)) \
                 ORDER BY updated_at DESC LIMIT 1",
            )
            .bind(organization_id)
            .bind(carc)
            .bind(cagc.as_deref())
            .bind(&payer_name)
            .fetch_optional(&state.pool)
            .await
            .map_err(AppError::Db)?;
            if let Some(playbook) = playbook {
                let recommendation: serde_json::Value = playbook
                    .try_get("recommendation")
                    .unwrap_or(serde_json::Value::Null);
                if let Some(obj) = recommendation.as_object() {
                    for (key, value) in obj {
                        parsed[key] = value.clone();
                    }
                }
                parsed["provider"] = serde_json::json!("approved_playbook");
                parsed["playbook"] = serde_json::json!({
                    "id": playbook.try_get::<Uuid, _>("id").map(|v| v.to_string()).unwrap_or_default(),
                    "name": playbook.try_get::<String, _>("name").unwrap_or_default(),
                    "version": playbook.try_get::<i32, _>("version").unwrap_or(1),
                });
            }
            let Json(stored) = store_analysis(
                State(state.clone()),
                Json(StoreAnalysis {
                    denial_id: request.denial_id.clone(),
                    claim_id: claim_db_id.to_string(),
                    model_name: "deterministic_rules_v1".into(),
                    provider_name: "deterministic_rules".into(),
                    provider_version: "v1".into(),
                    prompt_template_version: "deterministic_rules_v1".into(),
                    raw_prompt: "AI provider unavailable; deterministic rules applied.".into(),
                    raw_response: parsed.to_string(),
                    parsed_result: parsed.clone(),
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                    allowed_evidence_ids: Vec::new(),
                    fallback_reason: Some("llm_error".into()),
                }),
            )
            .await?;
            return Ok(serde_json::json!({
                "denial_id": request.denial_id,
                "claim_id": claim_number,
                "model": "deterministic_rules_v1",
                "parsed_json": parsed,
                "stored": true,
                "analysis_id": stored.get("id"),
                "provider": "deterministic_rules",
                "provider_notice": "AI provider unavailable; deterministic guidance was generated.",
                "policy_documents_retrieved": policy_texts.len(),
            }));
        }
    };

    // The denial id arrives in the body, so the access log sees no record in
    // the path to name. This entry says whose claim was analysed.
    if let Some(principal) = principal {
        denial_audit::record(
            &state.pool,
            "generate_analysis",
            "denial",
            Some(&request.denial_id),
            principal.user_id.as_deref(),
            // No patient name: an analysis audit entry records *which* claim
            // was analyzed without re-storing PHI (plan §8.10, §16.5).
            &serde_json::json!({
                "username": principal.username,
                "claim_number": claim_number,
                "carc_code": carc_code,
                "cpt_code": cpt_code,
                "policies_retrieved": policy_texts.len(),
            }),
            principal.ip.as_deref(),
            None,
            principal.organization_id.as_deref(),
        )
        .await;
    }

    Ok(serde_json::json!({
        "denial_id": request.denial_id,
        "claim_id": claim_number,
        "model": llm_result.get("model"),
        "parsed_json": llm_result.get("parsed_json"),
        "stored": llm_result.get("stored"),
        "policy_documents_retrieved": policy_texts.len(),
    }))
}

#[cfg(test)]
mod tests {
    use super::{
        cited_evidence_ids, resolve_citation_metadata, DeterministicRecommendationProvider,
        RecommendationProvider,
    };

    #[test]
    fn a_denial_query_labels_each_code_and_carries_the_carc_wording() {
        let q = super::build_search_query(
            Some("80053"),
            None,
            &["E11.65".to_string()],
            &[None],
            Some("50"),
            Some("These are non-covered services because this is not deemed a 'medical necessity'"),
        );
        assert_eq!(
            q,
            "CPT 80053 ICD-10 E11.65 CARC 50: These are non-covered services \
             because this is not deemed a 'medical necessity'"
                .replace("             ", "")
        );
    }

    #[test]
    fn descriptions_are_used_when_the_organization_has_imported_them() {
        let q = super::build_search_query(
            Some("20610"),
            Some("Arthrocentesis, major joint"),
            &["M17.11".to_string()],
            &[Some(
                "Unilateral primary osteoarthritis, right knee".to_string(),
            )],
            None,
            None,
        );
        assert_eq!(
            q,
            "CPT 20610 Arthrocentesis, major joint ICD-10 M17.11 Unilateral primary osteoarthritis, right knee"
        );
    }

    #[test]
    fn a_denial_with_no_codes_has_an_empty_query() {
        assert_eq!(
            super::build_search_query(None, None, &[], &[], Some(""), None),
            ""
        );
    }

    const FIRST: &str = "11111111-1111-1111-1111-111111111111";
    const SECOND: &str = "22222222-2222-2222-2222-222222222222";

    #[test]
    fn keeps_unique_cited_evidence_in_model_order() {
        let ids = cited_evidence_ids(
            &serde_json::json!({ "evidence_ids": [SECOND, FIRST, SECOND] }),
            &[FIRST.into(), SECOND.into()],
        )
        .unwrap();

        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0].to_string(), SECOND);
        assert_eq!(ids[1].to_string(), FIRST);
    }

    #[test]
    fn rejects_evidence_not_in_retrieval_allowlist() {
        assert!(cited_evidence_ids(
            &serde_json::json!({ "evidence_ids": [SECOND] }),
            &[FIRST.into()],
        )
        .is_err());
    }

    #[test]
    fn fallback_without_evidence_ids_has_no_citations() {
        assert!(cited_evidence_ids(&serde_json::json!({}), &[FIRST.into()])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn citation_metadata_excludes_document_title() {
        let citation = resolve_citation_metadata(
            uuid::uuid!("11111111-1111-1111-1111-111111111111"),
            uuid::uuid!("22222222-2222-2222-2222-222222222222"),
            "payer_policy",
            3,
        );

        assert_eq!(citation.as_object().unwrap().len(), 4);
        assert!(citation.get("document_title").is_none());
    }

    #[tokio::test]
    async fn deterministic_provider_selects_patient_responsibility_guidance() {
        let recommendation = DeterministicRecommendationProvider {
            cagc: "PR",
            carc: "1",
            description: "Deductible amount",
            next_payer: None,
            ncci_evidence: None,
        }
        .recommend()
        .await
        .unwrap();

        assert_eq!(recommendation["required_action"], "bill_patient");
        assert_eq!(recommendation["provider"], "deterministic_rules");
    }

    #[tokio::test]
    async fn a_patient_balance_goes_to_the_next_payer_first() {
        let recommendation = DeterministicRecommendationProvider {
            cagc: "PR",
            carc: "2",
            description: "Coinsurance amount",
            next_payer: Some("SYNTHETIC SECONDARY PLAN"),
            ncci_evidence: None,
        }
        .recommend()
        .await
        .unwrap();

        assert_eq!(recommendation["required_action"], "bill_secondary");
        assert!(recommendation["steps"][0]["action"]
            .as_str()
            .unwrap()
            .contains("SYNTHETIC SECONDARY PLAN"));
    }
}

/// Queue the same recommendation pipeline for callers that should not hold an
/// HTTP connection open while retrieval and model inference run.
pub async fn enqueue_generation(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<GenerateAnalysisRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let key = limit_key(&principal);
    if !state.analyses_limiter.allow(&key) {
        return Err(AppError::RateLimited {
            retry_after: state.analyses_limiter.retry_after(&key),
        });
    }
    let denial_id = Uuid::parse_str(request.denial_id.trim())
        .map_err(|_| AppError::BadRequest("denial_id must be a UUID".into()))?;
    let requested_by = principal
        .user_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok());
    let job_id: Uuid = sqlx::query_scalar(
        "INSERT INTO recommendation_jobs (denial_id, requested_by, temperature) \
         VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(denial_id)
    .bind(requested_by)
    .bind(request.temperature)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let worker_state = state.clone();
    let worker_principal = principal.clone();
    tokio::spawn(async move {
        if let Err(error) = sqlx::query(
            "UPDATE recommendation_jobs SET status = 'running', started_at = NOW(), attempts = attempts + 1 WHERE id = $1",
        )
        .bind(job_id)
        .execute(&worker_state.pool)
        .await
        {
            tracing::error!(%job_id, "could not start recommendation job: {error}");
            return;
        }
        match generate_analysis_for_request(worker_state.clone(), request, Some(worker_principal)).await {
            Ok(result) => {
                if let Err(error) = sqlx::query(
                    "UPDATE recommendation_jobs SET status = 'completed', result = $2::jsonb, completed_at = NOW() WHERE id = $1",
                )
                .bind(job_id)
                .bind(result.to_string())
                .execute(&worker_state.pool)
                .await
                {
                    tracing::error!(%job_id, "could not complete recommendation job: {error}");
                }
            }
            Err(error) => {
                tracing::warn!(%job_id, "recommendation job failed: {error}");
                let _ = sqlx::query(
                    "UPDATE recommendation_jobs SET status = 'failed', error_message = $2, completed_at = NOW() WHERE id = $1",
                )
                .bind(job_id)
                .bind(error.to_string())
                .execute(&worker_state.pool)
                .await;
            }
        }
    });

    Ok(Json(serde_json::json!({
        "id": job_id,
        "status": "pending",
        "denial_id": denial_id,
    })))
}

pub async fn get_generation_job(
    State(state): State<AppState>,
    axum::extract::Path(job_id): axum::extract::Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let row = sqlx::query(
        "SELECT id, denial_id, requested_by, temperature, status, attempts, result, error_message, created_at, started_at, completed_at \
         FROM recommendation_jobs WHERE id = $1",
    )
    .bind(job_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;
    Ok(Json(row_to_json(&row)))
}

/// Analyses needed before a fallback share is judged; one failure out of two
/// is noise, not an outage.
const DEGRADED_MIN_ANALYSES: i64 = 3;

/// Whether AI analyses are degraded: either the organization's most recent
/// `DEGRADED_MIN_ANALYSES` analyses all fell back or ran without evidence (an
/// outage happening now), or that share over the last 24 hours reaches
/// `AI_DEGRADED_THRESHOLD` (default 0.2). The 24-hour share alone reacts too
/// slowly: after a busy day, several failures in a row are still a small share.
pub async fn ai_status(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<serde_json::Value>, AppError> {
    Ok(Json(
        ai_fallback_summary(&state.pool, organization_id(&principal)?).await?,
    ))
}

/// The body of [`ai_status`], shared with system health.
pub async fn ai_fallback_summary(
    pool: &sqlx::PgPool,
    organization_id: Uuid,
) -> Result<serde_json::Value, AppError> {
    let threshold = std::env::var("AI_DEGRADED_THRESHOLD")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| (0.0..=1.0).contains(v))
        .unwrap_or(0.2);
    let row = sqlx::query(
        "SELECT COUNT(*) AS total, \
                COUNT(*) FILTER (WHERE aa.fallback_reason IS NOT NULL) AS degraded, \
                COUNT(*) FILTER (WHERE aa.fallback_reason = 'llm_error') AS llm_error, \
                COUNT(*) FILTER (WHERE aa.fallback_reason = 'retrieval_error') AS retrieval_error, \
                COUNT(*) FILTER (WHERE aa.fallback_reason = 'no_evidence') AS no_evidence \
         FROM ai_analyses aa JOIN claims c ON c.id = aa.claim_id \
         WHERE c.organization_id = $1 AND aa.created_at > NOW() - INTERVAL '24 hours'",
    )
    .bind(organization_id)
    .fetch_one(pool)
    .await
    .map_err(AppError::Db)?;
    let recent: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT aa.fallback_reason FROM ai_analyses aa JOIN claims c ON c.id = aa.claim_id \
         WHERE c.organization_id = $1 ORDER BY aa.created_at DESC LIMIT $2",
    )
    .bind(organization_id)
    .bind(DEGRADED_MIN_ANALYSES)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)?;
    let recent_all_degraded =
        recent.len() as i64 == DEGRADED_MIN_ANALYSES && recent.iter().all(Option::is_some);
    let total: i64 = row.get("total");
    let degraded_count: i64 = row.get("degraded");
    let share = if total == 0 {
        0.0
    } else {
        degraded_count as f64 / total as f64
    };
    Ok(serde_json::json!({
        "degraded": recent_all_degraded
            || (total >= DEGRADED_MIN_ANALYSES && share >= threshold),
        "recent_all_degraded": recent_all_degraded,
        "window_hours": 24,
        "analyses": total,
        "fallback_share": share,
        "threshold": threshold,
        "reasons": {
            "llm_error": row.get::<i64, _>("llm_error"),
            "retrieval_error": row.get::<i64, _>("retrieval_error"),
            "no_evidence": row.get::<i64, _>("no_evidence"),
        },
    }))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_analyses))
        .route("/status", get(ai_status))
        .route("/store", post(store_analysis))
        .route("/generate", post(generate_analysis))
        .route("/generate-jobs", post(enqueue_generation))
        .route("/generate-jobs/{job_id}", get(get_generation_job))
}
