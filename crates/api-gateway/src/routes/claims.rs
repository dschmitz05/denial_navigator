use axum::extract::{Query, State};
use axum::routing::{get, patch, post};
use axum::{Extension, Json, Router};
use chrono::DateTime;
use denial_common::error::AppError;
use denial_common::rbac::Principal;
use serde::{Deserialize, Serialize};
use sqlx::{Column, QueryBuilder, Row};
use uuid::Uuid;

use crate::state::AppState;

const TERMINAL_DENIAL_STATUSES: &[&str] = &["appealed", "overruled", "resolved", "written_off"];
const TERMINAL_OUTCOMES: &[&str] = &["approved", "overruled", "resolved", "denied_again", "cancelled"];
const APPEAL_RESOLUTION_TYPES: &[&str] = &["appeal_letter"];

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
    Query(params): Query<ListClaimsQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let mut qb = QueryBuilder::<sqlx::Postgres>::new(
        "SELECT c.*, COUNT(d.id) AS denial_count, \
         COUNT(d.id) FILTER (WHERE d.status <> ALL(ARRAY['appealed','overruled','resolved','written_off']::text[])) AS open_denial_count \
         FROM claims c LEFT JOIN denials d ON d.claim_id = c.id",
    );

    let q = params.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let mut need_where = true;
    if let Some(ref status) = params.status {
        if need_where {
            qb.push(" WHERE ");
            need_where = false;
        } else {
            qb.push(" AND ");
        }
        qb.push("c.status = ");
        qb.push_bind(status);
    }
    if let Some(q) = q {
        if need_where {
            qb.push(" WHERE ");
            need_where = false;
        } else {
            qb.push(" AND ");
        }
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

    let rows = qb.build().fetch_all(&state.pool).await.map_err(AppError::Db)?;
    let values: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| row_to_json(row))
        .collect();

    Ok(Json(values))
}

fn row_to_json(row: &sqlx::postgres::PgRow) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for col in row.columns().iter() {
        let name = col.name();
        let val = row
            .try_get::<Option<String>, _>(name)
            .map(|v| v.map(serde_json::Value::String).unwrap_or(serde_json::Value::Null))
            .or_else(|_| {
                row.try_get::<Option<i64>, _>(name)
                    .map(|v| v.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null))
            })
            .or_else(|_| {
                row.try_get::<Option<f64>, _>(name)
                    .map(|v| v.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null))
            })
            .or_else(|_| {
                row.try_get::<Option<bool>, _>(name)
                    .map(|v| v.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null))
            })
            .unwrap_or(serde_json::Value::Null);
        map.insert(name.to_string(), val);
    }
    serde_json::Value::Object(map)
}

pub async fn get_claim(
    State(state): State<AppState>,
    axum::extract::Path(claim_id): axum::extract::Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let row = sqlx::query("SELECT * FROM claims WHERE id = $1")
        .bind(claim_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::Db)?
        .ok_or(AppError::NotFound)?;

    let denials = sqlx::query(
        "SELECT * FROM denials WHERE claim_id = $1 ORDER BY service_line_number",
    )
    .bind(claim_id)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let analyses = sqlx::query(
        "SELECT * FROM ai_analyses WHERE claim_id = $1 ORDER BY created_at DESC",
    )
    .bind(claim_id)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let mut result = row_to_json(&row);
    result["denials"] = serde_json::to_value(
        denials.iter().map(row_to_json).collect::<Vec<_>>(),
    )
    .unwrap();
    result["analyses"] = serde_json::to_value(
        analyses.iter().map(row_to_json).collect::<Vec<_>>(),
    )
    .unwrap();

    Ok(Json(result))
}

pub async fn create_claim(
    State(state): State<AppState>,
    Json(body): Json<ClaimCreate>,
) -> Result<(axum::http::StatusCode, Json<serde_json::Value>), AppError> {
    let icd = serde_json::to_string(&body.icd_10_codes).map_err(|e| AppError::Internal(e.to_string()))?;
    let row = sqlx::query(
        "INSERT INTO claims (claim_number, patient_id, payer_name, total_charge, icd_10_codes, status) \
         VALUES ($1, $2, $3, $4, $5, 'ingested') RETURNING *",
    )
    .bind(&body.claim_number)
    .bind(&body.patient_id)
    .bind(&body.payer_name)
    .bind(body.total_charge)
    .bind(&icd)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    Ok((axum::http::StatusCode::CREATED, Json(row_to_json(&row))))
}

pub async fn update_claim(
    State(state): State<AppState>,
    axum::extract::Path(claim_id): axum::extract::Path<Uuid>,
    Json(body): Json<ClaimUpdate>,
) -> Result<Json<serde_json::Value>, AppError> {
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

    let mut qb = QueryBuilder::<sqlx::Postgres>::new(
        &format!("UPDATE claims SET {} WHERE id = ${} RETURNING *", sets.join(", "), claim_idx),
    );

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

    let row = query
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::Db)?
        .ok_or(AppError::NotFound)?;

    Ok(Json(row_to_json(&row)))
}

pub async fn dashboard_stats(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let total_claims: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM claims")
        .fetch_one(&state.pool)
        .await
        .map_err(AppError::Db)?;

    let denied_claims: (i64,) =
        sqlx::query_as("SELECT COUNT(DISTINCT claim_id) FROM denials")
            .fetch_one(&state.pool)
            .await
            .map_err(AppError::Db)?;

    let total_denials: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM denials WHERE status = 'open'")
            .fetch_one(&state.pool)
            .await
            .map_err(AppError::Db)?;

    let pending_appeals: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM appeals_queue \
         WHERE (outcome_status IS NULL OR outcome_status NOT IN ('approved','overruled','resolved','denied_again','cancelled')) \
           AND resolution_type = ANY(ARRAY['appeal_letter']::text[])",
    )
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let pending_worklist: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM appeals_queue \
         WHERE (outcome_status IS NULL OR outcome_status NOT IN ('approved','overruled','resolved','denied_again','cancelled')) \
           AND (resolution_type IS NULL OR NOT (resolution_type = ANY(ARRAY['appeal_letter']::text[])))",
    )
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let total_denied: (Option<f64>,) =
        sqlx::query_as("SELECT COALESCE(SUM(adjustment_amount), 0) FROM denials")
            .fetch_one(&state.pool)
            .await
            .map_err(AppError::Db)?;

    let open_denied: (Option<f64>,) = sqlx::query_as(
        "SELECT COALESCE(SUM(adjustment_amount), 0) FROM denials WHERE status = 'open'",
    )
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let financial = sqlx::query(
        "SELECT COALESCE(SUM(total_charge), 0) as total_charges, \
         COALESCE(SUM(total_paid), 0) as total_paid, \
         COALESCE(SUM(total_adjustment), 0) as total_adjustments \
         FROM claims WHERE id IN (SELECT DISTINCT claim_id FROM denials)",
    )
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let total_charges: Option<f64> = financial.try_get("total_charges").map_err(|e| AppError::Internal(e.to_string()))?;
    let total_paid: Option<f64> = financial.try_get("total_paid").map_err(|e| AppError::Internal(e.to_string()))?;
    let total_adjustments: Option<f64> = financial.try_get("total_adjustments").map_err(|e| AppError::Internal(e.to_string()))?;

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

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_claims).post(create_claim))
        .route("/dashboard/stats", get(dashboard_stats))
        .route("/{claim_id}", get(get_claim).patch(update_claim))
}
