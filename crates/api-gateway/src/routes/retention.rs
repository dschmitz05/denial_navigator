//! Audit log retention.
//!
//! Ported from `api-gateway/routes/retention.py`. Pruning an audit trail is a
//! sensitive act, so it is deliberate rather than automatic: admin-only (the
//! access-control middleware enforces `retention -> (ADMIN_ONLY, ADMIN_ONLY)`,
//! and the handler double-checks), it refuses a window shorter than a year,
//! and it records what it removed - including the date range - as an audit
//! entry of its own, written after the delete so it cannot be caught by the
//! same statement.

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use serde::Deserialize;
use sqlx::Row;
use uuid::Uuid;

use crate::state::AppState;

fn require_admin(principal: &Principal) -> Result<(), AppError> {
    if matches!(
        principal.role.as_deref(),
        Some("system_admin" | "security_admin")
    ) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn organization_id(principal: &Principal) -> Result<Uuid, AppError> {
    principal
        .organization_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(AppError::Forbidden)
}

#[derive(Deserialize)]
pub struct PruneRequest {
    pub older_than_days: Option<i64>,
    #[serde(default)]
    pub confirm: bool,
}

pub async fn audit_retention_status(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&principal)?;
    let organization_id = organization_id(&principal)?;

    let retention_days = state.default_retention_days as i64;
    let sql = format!(
        "SELECT COUNT(*) AS total, \
                MIN(created_at) AS oldest, \
                MAX(created_at) AS newest, \
                COUNT(*) FILTER (WHERE created_at < NOW() - INTERVAL '{retention_days} days') \
                    AS beyond_retention, \
                NULL::text AS on_disk \
         FROM audit_log WHERE organization_id = $1"
    );
    let row = sqlx::query(&sql)
        .bind(organization_id)
        .fetch_one(&state.pool)
        .await
        .map_err(AppError::Db)?;

    let total: i64 = row.try_get("total").unwrap_or(0);
    let oldest: Option<DateTime<Utc>> = row.try_get("oldest").ok().flatten();
    let newest: Option<DateTime<Utc>> = row.try_get("newest").ok().flatten();
    let beyond_retention: i64 = row.try_get("beyond_retention").unwrap_or(0);
    let on_disk: Option<String> = row.try_get("on_disk").ok().flatten();

    Ok(Json(serde_json::json!({
        "retention_days": retention_days,
        "retention_years": ((retention_days as f64 / 365.0) * 10.0).round() / 10.0,
        "minimum_allowed_days": state.min_retention_days,
        "total_entries": total,
        "oldest_entry": oldest.map(|t| t.to_rfc3339()),
        "newest_entry": newest.map(|t| t.to_rfc3339()),
        "entries_beyond_retention": beyond_retention,
        "on_disk": on_disk,
        "note": "Nothing is deleted automatically. Prune deliberately, and keep an \
                 exported copy if your policy requires the history beyond this window.",
    })))
}

pub async fn prune_audit_log(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(body): Json<PruneRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&principal)?;
    let organization_id = organization_id(&principal)?;

    let older_than_days = body
        .older_than_days
        .unwrap_or(state.default_retention_days as i64);
    if older_than_days < state.min_retention_days as i64 {
        return Err(AppError::Unprocessable(format!(
            "older_than_days must be at least {}",
            state.min_retention_days
        )));
    }
    if !body.confirm {
        return Err(AppError::BadRequest(
            "Send confirm=true. This permanently deletes audit history.".into(),
        ));
    }

    let preview = sqlx::query(&format!(
        "SELECT COUNT(*) AS n, MIN(created_at) AS from_date, MAX(created_at) AS to_date \
         FROM audit_log WHERE organization_id = $1 AND created_at < NOW() - INTERVAL '{older_than_days} days'"
    ))
    .bind(organization_id)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let n: i64 = preview.try_get("n").unwrap_or(0);
    if n == 0 {
        return Ok(Json(serde_json::json!({
            "status": "nothing_to_prune",
            "older_than_days": older_than_days,
            "deleted": 0,
        })));
    }

    let from_date: Option<DateTime<Utc>> = preview.try_get("from_date").ok().flatten();
    let to_date: Option<DateTime<Utc>> = preview.try_get("to_date").ok().flatten();

    sqlx::query(&format!(
        "DELETE FROM audit_log WHERE organization_id = $1 AND created_at < NOW() - INTERVAL '{older_than_days} days'"
    ))
    .bind(organization_id)
    .execute(&state.pool)
    .await
    .map_err(AppError::Db)?;

    // Written after the delete so it cannot itself be removed by the same
    // statement. This entry is the only remaining record that the prune ran.
    denial_audit::record(
        &state.pool,
        "audit_pruned",
        "audit_log",
        None,
        principal.user_id.as_deref(),
        &serde_json::json!({
            "username": principal.username,
            "older_than_days": older_than_days,
            "deleted": n,
            "covered_from": from_date.map(|t| t.to_rfc3339()),
            "covered_to": to_date.map(|t| t.to_rfc3339()),
        }),
        principal.ip.as_deref(),
        None,
    )
    .await;

    tracing::warn!(
        "audit log pruned by {}: {n} entries removed",
        principal.username
    );

    Ok(Json(serde_json::json!({
        "status": "pruned",
        "older_than_days": older_than_days,
        "deleted": n,
        "covered_from": from_date.map(|t| t.to_rfc3339()),
        "covered_to": to_date.map(|t| t.to_rfc3339()),
    })))
}

/// AI-record retention status, scoped to the caller's organization via the
/// claim it belongs to (`ai_analyses` has no `organization_id` of its own).
pub async fn ai_analyses_retention_status(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&principal)?;
    let organization_id = organization_id(&principal)?;

    let retention_days = state.default_retention_days as i64;
    let sql = format!(
        "SELECT COUNT(*) AS total, \
                MIN(aa.created_at) AS oldest, \
                MAX(aa.created_at) AS newest, \
                COUNT(*) FILTER (WHERE aa.created_at < NOW() - INTERVAL '{retention_days} days') \
                    AS beyond_retention \
         FROM ai_analyses aa \
         JOIN claims c ON c.id = aa.claim_id \
         WHERE c.organization_id = $1"
    );
    let row = sqlx::query(&sql)
        .bind(organization_id)
        .fetch_one(&state.pool)
        .await
        .map_err(AppError::Db)?;

    let total: i64 = row.try_get("total").unwrap_or(0);
    let oldest: Option<DateTime<Utc>> = row.try_get("oldest").ok().flatten();
    let newest: Option<DateTime<Utc>> = row.try_get("newest").ok().flatten();
    let beyond_retention: i64 = row.try_get("beyond_retention").unwrap_or(0);

    Ok(Json(serde_json::json!({
        "retention_days": retention_days,
        "minimum_allowed_days": state.min_retention_days,
        "total_entries": total,
        "oldest_entry": oldest.map(|t| t.to_rfc3339()),
        "newest_entry": newest.map(|t| t.to_rfc3339()),
        "entries_beyond_retention": beyond_retention,
        "note": "Raw prompt/response text is stored only when AI_STORE_RAW_ARTIFACTS=true. \
                 Prune deliberately; nothing is deleted automatically.",
    })))
}

/// Deliberate, admin-only, org-scoped prune of AI analysis history. Mirrors
/// the audit-log prune: refuses a window under the floor, requires
/// `confirm=true`, and records what it removed as its own audit entry.
pub async fn prune_ai_analyses(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(body): Json<PruneRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&principal)?;
    let organization_id = organization_id(&principal)?;

    let older_than_days = body
        .older_than_days
        .unwrap_or(state.default_retention_days as i64);
    if older_than_days < state.min_retention_days as i64 {
        return Err(AppError::Unprocessable(format!(
            "older_than_days must be at least {}",
            state.min_retention_days
        )));
    }
    if !body.confirm {
        return Err(AppError::BadRequest(
            "Send confirm=true. This permanently deletes AI analysis history.".into(),
        ));
    }

    let preview = sqlx::query(&format!(
        "SELECT COUNT(*) AS n, MIN(aa.created_at) AS from_date, MAX(aa.created_at) AS to_date \
         FROM ai_analyses aa \
         JOIN claims c ON c.id = aa.claim_id \
         WHERE c.organization_id = $1 AND aa.created_at < NOW() - INTERVAL '{older_than_days} days'"
    ))
    .bind(organization_id)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let n: i64 = preview.try_get("n").unwrap_or(0);
    if n == 0 {
        return Ok(Json(serde_json::json!({
            "status": "nothing_to_prune",
            "older_than_days": older_than_days,
            "deleted": 0,
        })));
    }

    let from_date: Option<DateTime<Utc>> = preview.try_get("from_date").ok().flatten();
    let to_date: Option<DateTime<Utc>> = preview.try_get("to_date").ok().flatten();

    sqlx::query(&format!(
        "DELETE FROM ai_analyses aa \
         USING claims c \
         WHERE c.id = aa.claim_id AND c.organization_id = $1 \
           AND aa.created_at < NOW() - INTERVAL '{older_than_days} days'"
    ))
    .bind(organization_id)
    .execute(&state.pool)
    .await
    .map_err(AppError::Db)?;

    // Written after the delete so it cannot itself be removed by the same
    // statement.
    denial_audit::record(
        &state.pool,
        "ai_analyses_pruned",
        "analysis",
        None,
        principal.user_id.as_deref(),
        &serde_json::json!({
            "username": principal.username,
            "older_than_days": older_than_days,
            "deleted": n,
            "covered_from": from_date.map(|t| t.to_rfc3339()),
            "covered_to": to_date.map(|t| t.to_rfc3339()),
        }),
        principal.ip.as_deref(),
        None,
    )
    .await;

    tracing::warn!(
        "ai_analyses pruned by {}: {n} entries removed",
        principal.username
    );

    Ok(Json(serde_json::json!({
        "status": "pruned",
        "older_than_days": older_than_days,
        "deleted": n,
        "covered_from": from_date.map(|t| t.to_rfc3339()),
        "covered_to": to_date.map(|t| t.to_rfc3339()),
    })))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/audit", get(audit_retention_status))
        .route("/audit/prune", post(prune_audit_log))
        .route("/ai", get(ai_analyses_retention_status))
        .route("/ai/prune", post(prune_ai_analyses))
}
