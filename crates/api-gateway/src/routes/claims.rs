use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use denial_db::pgjson::row_to_json;
use serde::Deserialize;
use sqlx::{QueryBuilder, Row};
use uuid::Uuid;

use super::scope::organization_id;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct ClaimCreate {
    pub claim_number: String,
    pub patient_id: String,
    pub payer_name: String,
    pub total_charge: f64,
    #[serde(default)]
    pub icd_10_codes: Vec<String>,
}

#[derive(Deserialize)]
pub struct ClaimUpdate {
    pub status: Option<String>,
    pub total_paid: Option<f64>,
    pub total_adjustment: Option<f64>,
}

#[derive(Deserialize)]
pub struct ListClaimsQuery {
    pub status: Option<String>,
    pub q: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    50
}

pub async fn list_claims(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<ListClaimsQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let organization_id = organization_id(&principal)?;
    let mut qb = QueryBuilder::<sqlx::Postgres>::new(
        "SELECT c.*, COUNT(d.id) AS denial_count, \
         COUNT(d.id) FILTER (WHERE d.status <> ALL(ARRAY['appealed','overruled','resolved','written_off']::text[])) AS open_denial_count \
         FROM claims c LEFT JOIN denials d ON d.claim_id = c.id",
    );

    let q = params.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
    qb.push(" WHERE c.organization_id = ")
        .push_bind(organization_id);
    if let Some(ref status) = params.status {
        qb.push(" AND ");
        qb.push("c.status = ");
        qb.push_bind(status);
    }
    if let Some(q) = q {
        qb.push(" AND ");
        let pattern = format!("%{q}%");
        qb.push("(c.claim_number ILIKE ");
        qb.push_bind(pattern.clone());
        qb.push(" OR c.patient_name ILIKE ");
        qb.push_bind(pattern.clone());
        qb.push(" OR c.patient_id ILIKE ");
        qb.push_bind(pattern);
        qb.push(")");
    }

    qb.push(" GROUP BY c.id ORDER BY c.created_at DESC");
    let limit = params.limit.clamp(1, 500);
    qb.push(" LIMIT ").push_bind(limit);
    qb.push(" OFFSET ").push_bind(params.offset.max(0));

    let rows = qb
        .build()
        .fetch_all(&state.pool)
        .await
        .map_err(AppError::Db)?;
    let values: Vec<serde_json::Value> = rows.iter().map(row_to_json).collect();

    Ok(Json(values))
}

pub async fn get_claim(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    axum::extract::Path(claim_id): axum::extract::Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let row = sqlx::query("SELECT * FROM claims WHERE id = $1 AND organization_id = $2")
        .bind(claim_id)
        .bind(organization_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::Db)?
        .ok_or(AppError::NotFound)?;

    let denials =
        sqlx::query("SELECT * FROM denials WHERE claim_id = $1 ORDER BY service_line_number")
            .bind(claim_id)
            .fetch_all(&state.pool)
            .await
            .map_err(AppError::Db)?;

    let analyses =
        sqlx::query("SELECT * FROM ai_analyses WHERE claim_id = $1 ORDER BY created_at DESC")
            .bind(claim_id)
            .fetch_all(&state.pool)
            .await
            .map_err(AppError::Db)?;

    let mut result = row_to_json(&row);
    result["denials"] =
        serde_json::to_value(denials.iter().map(row_to_json).collect::<Vec<_>>()).unwrap();
    result["analyses"] =
        serde_json::to_value(analyses.iter().map(row_to_json).collect::<Vec<_>>()).unwrap();

    // X12 835 carries the patient portion as PR adjustments. Keep it separate
    // from payer `total_paid` so staff can see the full payment composition in
    // the claim detail instead of treating a patient payment as a write-off.
    let patient_paid: (Option<f64>,) = sqlx::query_as(
        "SELECT COALESCE(SUM(adjustment_amount) FILTER (WHERE cagc = 'PR'), 0)::float8 \
         FROM denials WHERE claim_id = $1",
    )
    .bind(claim_id)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;
    result["patient_paid"] = serde_json::json!(patient_paid.0.unwrap_or(0.0));
    result["provider_adjustments"] = serde_json::json!(
        crate::routes::provider_adjustments::for_claim(&state.pool, organization_id, claim_id)
            .await?
    );

    Ok(Json(result))
}

pub async fn create_claim(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(body): Json<ClaimCreate>,
) -> Result<(axum::http::StatusCode, Json<serde_json::Value>), AppError> {
    let organization_id = organization_id(&principal)?;
    let row = sqlx::query(
        "INSERT INTO claims (organization_id, claim_number, patient_id, payer_name, total_charge, icd_10_codes, status) \
         VALUES ($1, $2, $3, $4, $5, $6, 'ingested') RETURNING *",
    )
    .bind(organization_id)
    .bind(&body.claim_number)
    .bind(&body.patient_id)
    .bind(&body.payer_name)
    .bind(body.total_charge)
    // icd_10_codes is a Postgres text[]; bind the Vec directly. (It was being
    // JSON-encoded to a string, which the text[] column rejected.)
    .bind(&body.icd_10_codes)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    Ok((axum::http::StatusCode::CREATED, Json(row_to_json(&row))))
}

pub async fn update_claim(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    axum::extract::Path(claim_id): axum::extract::Path<Uuid>,
    Json(body): Json<ClaimUpdate>,
) -> Result<Json<serde_json::Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let mut sets = Vec::new();
    let mut binds: Vec<Box<dyn sqlx::Encode<'static, sqlx::Postgres> + Send>> = Vec::new();

    if let Some(ref status) = body.status {
        sets.push(format!("status = ${}", sets.len() + 1));
        binds.push(Box::new(status.clone()));
    }
    if let Some(v) = body.total_paid {
        sets.push(format!("total_paid = ${}", sets.len() + 1));
        binds.push(Box::new(v));
    }
    if let Some(v) = body.total_adjustment {
        sets.push(format!("total_adjustment = ${}", sets.len() + 1));
        binds.push(Box::new(v));
    }

    if sets.is_empty() {
        return Err(AppError::BadRequest("No updates provided".into()));
    }

    sets.push("updated_at = NOW()".to_string());
    let claim_idx = sets.len() + 1;

    let mut qb = QueryBuilder::<sqlx::Postgres>::new(&format!(
        "UPDATE claims SET {} WHERE id = ${} AND organization_id = ${} RETURNING *",
        sets.join(", "),
        claim_idx,
        claim_idx + 1
    ));

    let mut query = qb.build();
    if let Some(ref s) = body.status {
        query = query.bind(s);
    }
    if let Some(v) = body.total_paid {
        query = query.bind(v);
    }
    if let Some(v) = body.total_adjustment {
        query = query.bind(v);
    }
    query = query.bind(claim_id);
    query = query.bind(organization_id);

    let row = query
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::Db)?
        .ok_or(AppError::NotFound)?;

    Ok(Json(row_to_json(&row)))
}

pub async fn dashboard_stats(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<serde_json::Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let total_claims: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM claims WHERE organization_id = $1")
            .bind(organization_id)
            .fetch_one(&state.pool)
            .await
            .map_err(AppError::Db)?;

    let denied_claims: (i64,) = sqlx::query_as(
        "SELECT COUNT(DISTINCT d.claim_id) FROM denials d JOIN claims c ON c.id = d.claim_id WHERE c.organization_id = $1",
    )
        .bind(organization_id)
        .fetch_one(&state.pool)
        .await
        .map_err(AppError::Db)?;

    let total_denials: (i64,) =
        sqlx::query_as(
            "SELECT COUNT(*) FROM denials d JOIN claims c ON c.id = d.claim_id WHERE d.status = 'open' AND c.organization_id = $1",
        )
            .bind(organization_id)
            .fetch_one(&state.pool)
            .await
            .map_err(AppError::Db)?;

    let pending_appeals: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM appeals_queue aq JOIN claims c ON c.id = aq.claim_id \
         WHERE (outcome_status IS NULL OR outcome_status NOT IN ('approved','overruled','resolved','denied_again','cancelled')) \
           AND resolution_type = ANY(ARRAY['appeal_letter']::text[]) AND c.organization_id = $1",
    )
    .bind(organization_id)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let pending_worklist: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM appeals_queue aq JOIN claims c ON c.id = aq.claim_id \
         WHERE (outcome_status IS NULL OR outcome_status NOT IN ('approved','overruled','resolved','denied_again','cancelled')) \
           AND (resolution_type IS NULL OR NOT (resolution_type = ANY(ARRAY['appeal_letter']::text[]))) \
           AND c.organization_id = $1",
    )
    .bind(organization_id)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let total_denied: (Option<f64>,) =
        sqlx::query_as(
            "SELECT COALESCE(SUM(d.adjustment_amount), 0)::float8 FROM denials d JOIN claims c ON c.id = d.claim_id WHERE c.organization_id = $1",
        )
            .bind(organization_id)
            .fetch_one(&state.pool)
            .await
            .map_err(AppError::Db)?;

    let open_denied: (Option<f64>,) = sqlx::query_as(
        "SELECT COALESCE(SUM(d.adjustment_amount), 0)::float8 FROM denials d JOIN claims c ON c.id = d.claim_id WHERE d.status = 'open' AND c.organization_id = $1",
    )
    .bind(organization_id)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let financial = sqlx::query(
        "SELECT COALESCE(SUM(total_charge), 0)::float8 as total_charges, \
         COALESCE(SUM(total_paid), 0)::float8 as total_paid, \
         COALESCE(SUM(total_adjustment), 0)::float8 as total_adjustments \
         FROM claims WHERE organization_id = $1 AND id IN (SELECT DISTINCT claim_id FROM denials)",
    )
    .bind(organization_id)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let total_charges: Option<f64> = financial
        .try_get("total_charges")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let total_paid: Option<f64> = financial
        .try_get("total_paid")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let total_adjustments: Option<f64> = financial
        .try_get("total_adjustments")
        .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(Json(serde_json::json!({
        "total_claims": total_claims.0,
        "denied_claims": denied_claims.0,
        "pending_denials": total_denials.0,
        "pending_appeals": pending_appeals.0,
        "pending_worklist": pending_worklist.0,
        "total_denied": total_denied.0.unwrap_or(0.0),
        "open_denied": open_denied.0.unwrap_or(0.0),
        "total_charges": total_charges.unwrap_or(0.0),
        "total_paid": total_paid.unwrap_or(0.0),
        "total_adjustments": total_adjustments.unwrap_or(0.0),
    })))
}

/// A deliberately narrow, authenticated export. It contains operational
/// claim fields only, is bounded, and is audited just like a screen view.
pub async fn export_claims_csv(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Response, AppError> {
    if !matches!(
        principal.role.as_deref(),
        Some("revenue_cycle_manager" | "system_admin")
    ) {
        return Err(AppError::Forbidden);
    }
    let organization_id = principal
        .organization_id
        .as_deref()
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .ok_or(AppError::Forbidden)?;
    let rows = sqlx::query(
        "SELECT c.claim_number, c.payer_name, c.status, c.total_charge, c.total_paid, \
                c.total_adjustment, COUNT(d.id) AS denial_count, \
                COALESCE(SUM(d.adjustment_amount), 0)::float8 AS denied_amount \
         FROM claims c LEFT JOIN denials d ON d.claim_id=c.id \
         WHERE c.organization_id = $1 \
         GROUP BY c.id ORDER BY c.created_at DESC LIMIT 10000",
    )
    .bind(organization_id)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer
        .write_record([
            "claim_number",
            "payer",
            "status",
            "total_charge",
            "payer_paid",
            "adjustment",
            "denial_count",
            "denied_amount",
        ])
        .map_err(|e| AppError::Internal(e.to_string()))?;
    for r in rows {
        // Spreadsheet applications interpret leading formula characters even
        // when the source value was merely an external claim/payer string.
        let csv_text = |value: String| match value.as_bytes().first() {
            Some(b'=' | b'+' | b'-' | b'@') => format!("'{value}"),
            _ => value,
        };
        writer
            .write_record([
                csv_text(r.try_get::<String, _>("claim_number").unwrap_or_default()),
                csv_text(r.try_get::<String, _>("payer_name").unwrap_or_default()),
                r.try_get::<String, _>("status").unwrap_or_default(),
                r.try_get::<Option<f64>, _>("total_charge")
                    .ok()
                    .flatten()
                    .unwrap_or(0.0)
                    .to_string(),
                r.try_get::<Option<f64>, _>("total_paid")
                    .ok()
                    .flatten()
                    .unwrap_or(0.0)
                    .to_string(),
                r.try_get::<Option<f64>, _>("total_adjustment")
                    .ok()
                    .flatten()
                    .unwrap_or(0.0)
                    .to_string(),
                r.try_get::<i64, _>("denial_count").unwrap_or(0).to_string(),
                r.try_get::<Option<f64>, _>("denied_amount")
                    .ok()
                    .flatten()
                    .unwrap_or(0.0)
                    .to_string(),
            ])
            .map_err(|e| AppError::Internal(e.to_string()))?;
    }
    let bytes = writer
        .into_inner()
        .map_err(|e| AppError::Internal(e.to_string()))?;
    denial_audit::record(&state.pool, "export_claims_csv", "claim_export", None, principal.user_id.as_deref(), &serde_json::json!({"username": principal.username, "rows": bytes.iter().filter(|&&b| b == b'\n').count().saturating_sub(1)}), principal.ip.as_deref(), None, principal.organization_id.as_deref()).await;
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=claims-export.csv",
            ),
        ],
        bytes,
    )
        .into_response())
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/export.csv", get(export_claims_csv))
        .route("/", get(list_claims).post(create_claim))
        .route("/dashboard/stats", get(dashboard_stats))
        .route("/unanswered", get(crate::routes::unanswered::list))
        .route(
            "/{claim_id}/followups",
            axum::routing::post(crate::routes::unanswered::record_followup),
        )
        .route("/{claim_id}", get(get_claim).patch(update_claim))
}
