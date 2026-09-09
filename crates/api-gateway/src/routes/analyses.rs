//! AI analyses routes.
//!
//! Ported from `api-gateway/routes/analyses.py`. `generate` is the full
//! pipeline: pull the denial and its code definitions, retrieve matching payer
//! policy with the RAG engine, build the prompt, and hand it to the LLM
//! service (which stores the parsed result itself).

use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use denial_common::error::AppError;
use denial_common::pgjson::row_to_json;
use denial_common::rbac::{Principal, PrincipalKind};
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

#[derive(Deserialize)]
pub struct StoreAnalysis {
    pub denial_id: String,
    pub claim_id: String,
    pub model_name: String,
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

#[derive(Deserialize)]
pub struct GenerateAnalysisRequest {
    pub denial_id: String,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
}

fn default_temperature() -> f32 {
    0.3
}

/// The full ai_analyses column list, with `confidence_score` cast off `numeric`
/// so it comes back as a JSON number rather than dropping out.
const ANALYSIS_COLUMNS: &str = "aa.id, aa.denial_id, aa.claim_id, aa.model_name, \
    aa.prompt_tokens, aa.completion_tokens, aa.total_tokens, aa.system_prompt_template, \
    aa.raw_prompt, aa.raw_response, aa.explanation, aa.denial_category, aa.root_cause_summary, \
    aa.required_action, aa.action_plan, aa.steps, aa.needs_appeal, aa.draft_appeal_letter, \
    aa.confidence_score::float8 AS confidence_score, aa.created_at, aa.updated_at";


pub async fn list_analyses(
    State(state): State<AppState>,
    Query(params): Query<ListAnalysesQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let mut qb = QueryBuilder::<sqlx::Postgres>::new(format!(
        "SELECT {ANALYSIS_COLUMNS}, d.cpt_code, d.carc_code, \
         c.claim_number, c.patient_name, c.payer_name \
         FROM ai_analyses aa \
         JOIN denials d ON d.id = aa.denial_id \
         JOIN claims c ON c.id = aa.claim_id"
    ));

    let mut need_where = true;
    if let Some(denial_id) = params.denial_id {
        qb.push(" WHERE aa.denial_id = ").push_bind(denial_id);
        need_where = false;
    }
    if let Some(claim_id) = params.claim_id {
        qb.push(if need_where { " WHERE " } else { " AND " });
        qb.push("aa.claim_id = ").push_bind(claim_id);
    }

    qb.push(" ORDER BY aa.created_at DESC LIMIT ")
        .push_bind(params.limit.clamp(1, 500));

    let rows = qb.build().fetch_all(&state.pool).await.map_err(AppError::Db)?;
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
    let action_plan = serde_json::to_string(pr.get("action_plan").unwrap_or(&serde_json::Value::Null))
        .unwrap_or_else(|_| "null".into());
    let steps = serde_json::to_string(pr.get("steps").unwrap_or(&serde_json::Value::Null))
        .unwrap_or_else(|_| "null".into());
    let needs_appeal = pr.get("needs_appeal").and_then(|v| v.as_bool()).unwrap_or(false);
    let draft_appeal_letter = pr
        .get("draft_appeal_letter")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let confidence_score = pr
        .get("confidence_score")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    let row = sqlx::query(
        "INSERT INTO ai_analyses \
            (denial_id, claim_id, model_name, prompt_tokens, completion_tokens, total_tokens, \
             system_prompt_template, raw_prompt, raw_response, \
             explanation, denial_category, required_action, root_cause_summary, action_plan, steps, \
             needs_appeal, draft_appeal_letter, confidence_score) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14::jsonb, $15::jsonb, \
                 $16, $17, $18) \
         RETURNING id, denial_id, claim_id, model_name, prompt_tokens, completion_tokens, \
             total_tokens, system_prompt_template, raw_prompt, raw_response, explanation, \
             denial_category, root_cause_summary, required_action, action_plan, steps, \
             needs_appeal, draft_appeal_letter, confidence_score::float8 AS confidence_score, \
             created_at, updated_at",
    )
    .bind(denial_id)
    .bind(claim_id)
    .bind(&a.model_name)
    .bind(a.prompt_tokens)
    .bind(a.completion_tokens)
    .bind(a.total_tokens)
    .bind("denial_analysis_v1")
    .bind(&a.raw_prompt)
    .bind(&a.raw_response)
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
         WHERE d.id = $1",
    )
    .bind(denial_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;

    let claim_db_id: Uuid = denial.try_get("claim_id").map_err(|e| AppError::Internal(e.to_string()))?;
    let claim_number: String = denial.try_get("claim_number").unwrap_or_default();
    let patient_name: Option<String> = denial.try_get("patient_name").ok().flatten();
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
    let icd = icd_codes.iter().take(5).cloned().collect::<Vec<_>>().join(" ");
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
                    .map(|r| r.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string())
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
        }))
        .await?;

    let llm_result = state
        .llm
        .analyze_denial(&serde_json::json!({
            "denial_id": request.denial_id,
            // ai_analyses.claim_id is a UUID FK to claims(id); the human-
            // readable claim number is already baked into the prompt text.
            "claim_id": claim_db_id.to_string(),
            "prompt": prompt,
            "temperature": request.temperature,
        }))
        .await?;

    // The denial id arrives in the body, so the access log sees no record in
    // the path to name. This entry says whose claim was analysed.
    denial_common::audit::record(
        &state.pool,
        "generate_analysis",
        "denial",
        Some(&request.denial_id),
        principal.user_id.as_deref(),
        &serde_json::json!({
            "username": principal.username,
            "claim_number": claim_number,
            "patient_name": patient_name,
            "carc_code": carc_code,
            "cpt_code": cpt_code,
            "policies_retrieved": policy_texts.len(),
        }),
        principal.ip.as_deref(),
        None,
    )
    .await;

    Ok(Json(serde_json::json!({
        "denial_id": request.denial_id,
        "claim_id": claim_number,
        "model": llm_result.get("model"),
        "parsed_json": llm_result.get("parsed_json"),
        "stored": llm_result.get("stored"),
        "policy_documents_retrieved": policy_texts.len(),
    })))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_analyses))
        .route("/store", post(store_analysis))
        .route("/generate", post(generate_analysis))
}
