//! Manager-curated deterministic denial-resolution playbooks.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use denial_db::pgjson::row_to_json;
use serde::Deserialize;
use sqlx::QueryBuilder;
use uuid::Uuid;

use crate::state::AppState;

#[derive(Deserialize)]
pub struct PlaybookInput {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub triggers: serde_json::Value,
    #[serde(default)]
    pub recommendation: serde_json::Value,
}
#[derive(Deserialize)]
pub struct ListQuery {
    pub status: Option<String>,
}
#[derive(Deserialize)]
pub struct TestRequest {
    pub carc_code: Option<String>,
    pub cagc: Option<String>,
    pub payer_name: Option<String>,
}

fn manager(principal: &Principal) -> Result<(), AppError> {
    match principal.role.as_deref() {
        Some("revenue_cycle_manager" | "system_admin") => Ok(()),
        _ => Err(AppError::Forbidden),
    }
}
fn organization_id(principal: &Principal) -> Result<Uuid, AppError> {
    principal
        .organization_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(AppError::Forbidden)
}
fn validate(input: &PlaybookInput) -> Result<(), AppError> {
    if input.name.trim().is_empty() || input.name.len() > 200 {
        return Err(AppError::BadRequest(
            "name is required and must be at most 200 characters".into(),
        ));
    }
    if !input.triggers.is_object() || !input.recommendation.is_object() {
        return Err(AppError::BadRequest(
            "triggers and recommendation must be JSON objects".into(),
        ));
    }
    Ok(())
}

pub async fn list(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let organization_id = organization_id(&principal)?;
    let mut qb = QueryBuilder::<sqlx::Postgres>::new("SELECT p.*, u.username AS creator_name, a.username AS approver_name FROM institutional_playbooks p LEFT JOIN users u ON u.id=p.created_by LEFT JOIN users a ON a.id=p.approved_by WHERE p.organization_id = ");
    qb.push_bind(organization_id);
    if let Some(status) = q.status {
        qb.push(" AND p.status = ").push_bind(status);
    }
    qb.push(" ORDER BY p.updated_at DESC");
    let rows = qb
        .build()
        .fetch_all(&state.pool)
        .await
        .map_err(AppError::Db)?;
    Ok(Json(rows.iter().map(row_to_json).collect()))
}
pub async fn create(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(input): Json<PlaybookInput>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    manager(&principal)?;
    validate(&input)?;
    let organization_id = organization_id(&principal)?;
    let user_id = principal
        .user_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok());
    let row = sqlx::query("INSERT INTO institutional_playbooks (organization_id,name,description,triggers,recommendation,created_by) VALUES ($1,$2,$3,$4::jsonb,$5::jsonb,$6) RETURNING *")
        .bind(organization_id).bind(input.name.trim()).bind(input.description).bind(input.triggers.to_string()).bind(input.recommendation.to_string()).bind(user_id).fetch_one(&state.pool).await.map_err(AppError::Db)?;
    let out = row_to_json(&row);
    denial_audit::record(
        &state.pool,
        "playbook_created",
        "playbook",
        out.get("id").and_then(|v| v.as_str()),
        principal.user_id.as_deref(),
        &serde_json::json!({"username":principal.username,"name":input.name}),
        principal.ip.as_deref(),
        None,
    )
    .await;
    Ok((StatusCode::CREATED, Json(out)))
}
pub async fn update(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<Uuid>,
    Json(input): Json<PlaybookInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    manager(&principal)?;
    validate(&input)?;
    let organization_id = organization_id(&principal)?;
    let row = sqlx::query("UPDATE institutional_playbooks SET name=$1,description=$2,triggers=$3::jsonb,recommendation=$4::jsonb,status='draft',approved_by=NULL,approved_at=NULL,version=version+1,updated_at=NOW() WHERE id=$5 AND organization_id=$6 AND status <> 'archived' RETURNING *")
        .bind(input.name.trim()).bind(input.description).bind(input.triggers.to_string()).bind(input.recommendation.to_string()).bind(id).bind(organization_id).fetch_optional(&state.pool).await.map_err(AppError::Db)?.ok_or(AppError::NotFound)?;
    denial_audit::record(
        &state.pool,
        "playbook_updated",
        "playbook",
        Some(&id.to_string()),
        principal.user_id.as_deref(),
        &serde_json::json!({"username":principal.username}),
        principal.ip.as_deref(),
        None,
    )
    .await;
    Ok(Json(row_to_json(&row)))
}
pub async fn approve(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    manager(&principal)?;
    let organization_id = organization_id(&principal)?;
    let user_id = principal
        .user_id
        .as_deref()
        .and_then(|v| Uuid::parse_str(v).ok());
    let row=sqlx::query("UPDATE institutional_playbooks SET status='approved',approved_by=$1,approved_at=NOW(),updated_at=NOW() WHERE id=$2 AND organization_id=$3 AND status='draft' RETURNING *").bind(user_id).bind(id).bind(organization_id).fetch_optional(&state.pool).await.map_err(AppError::Db)?.ok_or(AppError::NotFound)?;
    denial_audit::record(
        &state.pool,
        "playbook_approved",
        "playbook",
        Some(&id.to_string()),
        principal.user_id.as_deref(),
        &serde_json::json!({"username":principal.username}),
        principal.ip.as_deref(),
        None,
    )
    .await;
    Ok(Json(row_to_json(&row)))
}
pub async fn archive(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    manager(&principal)?;
    let organization_id = organization_id(&principal)?;
    let row=sqlx::query("UPDATE institutional_playbooks SET status='archived',updated_at=NOW() WHERE id=$1 AND organization_id=$2 RETURNING *").bind(id).bind(organization_id).fetch_optional(&state.pool).await.map_err(AppError::Db)?.ok_or(AppError::NotFound)?;
    denial_audit::record(
        &state.pool,
        "playbook_archived",
        "playbook",
        Some(&id.to_string()),
        principal.user_id.as_deref(),
        &serde_json::json!({"username":principal.username}),
        principal.ip.as_deref(),
        None,
    )
    .await;
    Ok(Json(row_to_json(&row)))
}
pub async fn test(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<TestRequest>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let organization_id = organization_id(&principal)?;
    let rows=sqlx::query("SELECT * FROM institutional_playbooks WHERE organization_id=$1 AND status='approved' AND (triggers->>'carc_code' IS NULL OR triggers->>'carc_code'=$2) AND (triggers->>'cagc' IS NULL OR triggers->>'cagc'=$3) AND (triggers->>'payer_name' IS NULL OR lower(triggers->>'payer_name')=lower($4)) ORDER BY updated_at DESC").bind(organization_id).bind(req.carc_code).bind(req.cagc).bind(req.payer_name).fetch_all(&state.pool).await.map_err(AppError::Db)?;
    Ok(Json(rows.iter().map(row_to_json).collect()))
}
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/test", post(test))
        .route("/{id}", post(update))
        .route("/{id}/approve", post(approve))
        .route("/{id}/archive", post(archive))
}
