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

use crate::state::AppState;

fn require_admin(principal: &Principal) -> Result<(), AppError> {
    if principal.role.as_deref() == Some("admin") {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
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

    let retention_days = state.default_retention_days as i64;
    let sql = format!(
        "SELECT COUNT(*) AS total, \
                MIN(created_at) AS oldest, \
                MAX(created_at) AS newest, \
                COUNT(*) FILTER (WHERE created_at < NOW() - INTERVAL '{retention_days} days') \
                    AS beyond_retention, \
                pg_size_pretty(pg_total_relation_size('audit_log')) AS on_disk \
         FROM audit_log"
    );
    let row = sqlx::query(&sql)
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
         FROM audit_log WHERE created_at < NOW() - INTERVAL '{older_than_days} days'"
    ))
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
        "DELETE FROM audit_log WHERE created_at < NOW() - INTERVAL '{older_than_days} days'"
    ))
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

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/audit", get(audit_retention_status))
        .route("/audit/prune", post(prune_audit_log))
}
