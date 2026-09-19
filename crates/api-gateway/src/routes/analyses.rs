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
use denial_common::error::AppError;
use denial_db::pgjson::row_to_json;
use serde::Deserialize;
use sqlx::{QueryBuilder, Row};
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
}

#[derive(Clone, Deserialize)]
pub struct GenerateAnalysisRequest {
    pub denial_id: String,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
}

fn default_temperature() -> f32 {
    0.3
}

/// Safe, explainable fallback when an AI provider is unavailable. This keeps
/// the denial workflow moving and deliberately prefers reviewable work over a
/// speculative appeal or write-off.
fn deterministic_recommendation(cagc: &str, carc: &str, description: &str) -> serde_json::Value {
    let (category, action, step) = if cagc == "PR" {
        (
            "patient_responsibility",
            "bill_patient",
            "Move the payer-assigned balance to the patient statement and verify the EOB amount.",
        )
    } else if matches!(carc, "16" | "17" | "18" | "50" | "96") {
        ("missing_info", "clinical_documentation", "Review the remittance advice and submit the requested clinical or claim documentation.")
    } else if matches!(carc, "197" | "198" | "204") {
        ("lack_of_preauth", "clinical_documentation", "Verify authorization requirements and gather authorization or medical-necessity support before resubmission.")
    } else {
        ("other", "coding_correction", "Review the claim, remittance advice, coding, modifiers, and payer edits before corrected resubmission.")
    };
    serde_json::json!({
        "explanation": format!("Deterministic guidance based on {} {}.", carc, description),
        "denial_category": category,
        "required_action": action,
        "root_cause_summary": "AI provider unavailable; generated from the adjustment group and reason code.",
        "action_plan": {"type": action, "requires": ["remittance advice review"]},
        "steps": [{"step": 1, "action": step}],
        "needs_appeal": false,
        "draft_appeal_letter": "",
        "confidence_score": 0.45,
        "provider": "deterministic_rules"
    })
}

/// The full ai_analyses column list, with `confidence_score` cast off `numeric`
/// so it comes back as a JSON number rather than dropping out.
const ANALYSIS_COLUMNS: &str =
    "aa.id, aa.denial_id, aa.claim_id, aa.model_name, aa.provider_name, \
    aa.provider_version, aa.prompt_template_version, \
    aa.prompt_tokens, aa.completion_tokens, aa.total_tokens, aa.system_prompt_template, \
    aa.raw_prompt, aa.raw_response, aa.explanation, aa.denial_category, aa.root_cause_summary, \
    aa.required_action, aa.action_plan, aa.steps, aa.needs_appeal, aa.draft_appeal_letter, \
    aa.confidence_score::float8 AS confidence_score, aa.created_at, aa.updated_at";

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
             needs_appeal, draft_appeal_letter, confidence_score) \
          VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18::jsonb, $19::jsonb, \
                  $20, $21, $22) \
          RETURNING id, denial_id, claim_id, model_name, provider_name, provider_version, prompt_template_version, \
              prompt_tokens, completion_tokens, \
              total_tokens, system_prompt_template, raw_prompt, raw_response, explanation, \
              denial_category, root_cause_summary, required_action, action_plan, steps, \
              needs_appeal, draft_appeal_letter, confidence_score::float8 AS confidence_score, \
              created_at, updated_at",
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
    .bind(needs_appeal)
    .bind(draft_appeal_letter)
    .bind(confidence_score)
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
                c.claim_number, c.patient_name, c.payer_name, c.icd_10_codes, \
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
    let icd_codes: Vec<String> = denial
        .try_get::<Option<Vec<String>>, _>("icd_10_codes")
        .ok()
        .flatten()
        .unwrap_or_default();

    // All of them, not just the first: a medical-necessity denial usually
    // turns on the secondary diagnosis. Capped so a long list cannot drown
    // out the CPT and CARC terms.
    let icd = icd_codes
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let cpt = cpt_code.as_deref().unwrap_or("");
    let carc = carc_code.as_deref().unwrap_or("");
    let search_query = format!("{payer_name} {cpt} {icd} {carc}");

    let policy_texts: Vec<String> = match state
        .rag
        .search(&search_query, 5, serde_json::json!({ "payer": payer_name }))
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
            Vec::new()
        }
    };

    let prompt = state
        .rag
        .build_prompt(&serde_json::json!({
            "claim_id": claim_number,
            "payer_name": payer_name,
            "cpt_code": cpt,
            "icd10_code": icd,
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

    let llm_result = state
        .llm
        .analyze_denial(&serde_json::json!({
            "denial_id": request.denial_id,
            // ai_analyses.claim_id is a UUID FK to claims(id); the human-
            // readable claim number is already baked into the prompt text.
            "claim_id": claim_db_id.to_string(),
            "prompt": prompt,
            "allowed_evidence_ids": allowed_evidence_ids,
            "temperature": request.temperature,
        }))
        .await;

    let llm_result = match llm_result {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!("AI provider unavailable; using deterministic recommendation: {error}");
            let mut parsed = deterministic_recommendation(
                cagc.as_deref().unwrap_or(""),
                carc,
                carc_description
                    .as_deref()
                    .unwrap_or("the payer's adjustment reason"),
            );
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

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_analyses))
        .route("/store", post(store_analysis))
        .route("/generate", post(generate_analysis))
        .route("/generate-jobs", post(enqueue_generation))
        .route("/generate-jobs/{job_id}", get(get_generation_job))
}
