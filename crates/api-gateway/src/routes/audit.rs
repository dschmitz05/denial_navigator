//! Audit log routes.
//!
//! Ported from `api-gateway/routes/audit.py`. Read-only, manager-and-up (the
//! access-control middleware enforces `audit -> (MANAGER_UP, NOBODY)`), so
//! these handlers do no role checks of their own.

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use serde::Deserialize;
use sqlx::{QueryBuilder, Row};
use uuid::Uuid;

use crate::state::AppState;

/// Who performed an entry. Three kinds of actor end up in this table and only
/// the first has a `users` row: a person (`user_id` joins to `users`), a
/// sibling service (`user_id` NULL, `service:*` in details), and an
/// unauthenticated caller (`user_id` NULL, `anonymous` in details).
const ACTOR_SQL: &str = "COALESCE(u.username, al.details->>'username', 'system')";

#[derive(Deserialize)]
pub struct AuditQuery {
    pub action: Option<String>,
    pub resource_type: Option<String>,
    pub user_id: Option<String>,
    /// Actor name, including `service:*` and `anonymous`.
    pub username: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    200
}

fn organization_id(principal: &Principal) -> Result<Uuid, AppError> {
    principal
        .organization_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(AppError::Forbidden)
}

fn opt_string(row: &sqlx::postgres::PgRow, col: &str) -> Option<String> {
    row.try_get::<Option<String>, _>(col).ok().flatten()
}

fn opt_uuid_string(row: &sqlx::postgres::PgRow, col: &str) -> Option<String> {
    row.try_get::<Option<Uuid>, _>(col)
        .ok()
        .flatten()
        .map(|u| u.to_string())
}

fn opt_ts(row: &sqlx::postgres::PgRow, col: &str) -> Option<String> {
    row.try_get::<Option<DateTime<Utc>>, _>(col)
        .ok()
        .flatten()
        .map(|t| t.to_rfc3339())
}

/// Make one audit row JSON-safe. `ip_address` arrives already rendered to text
/// by `host()`, and `details` is decoded straight from `jsonb`.
fn serialize_row(row: &sqlx::postgres::PgRow) -> serde_json::Value {
    serde_json::json!({
        "id": opt_uuid_string(row, "id"),
        "user_id": opt_uuid_string(row, "user_id"),
        "username": opt_string(row, "username"),
        "action": opt_string(row, "action"),
        "resource_type": opt_string(row, "resource_type"),
        "resource_id": opt_uuid_string(row, "resource_id"),
        "details": row
            .try_get::<Option<serde_json::Value>, _>("details")
            .ok()
            .flatten()
            .unwrap_or(serde_json::Value::Null),
        "ip_address": opt_string(row, "ip_address"),
        "user_agent": opt_string(row, "user_agent"),
        "created_at": opt_ts(row, "created_at"),
        "actor": opt_string(row, "actor"),
    })
}

pub async fn list_audit_log(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<AuditQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let organization_id = organization_id(&principal)?;
    let limit = params.limit.clamp(1, 1000);
    let offset = params.offset.max(0);

    let mut qb = QueryBuilder::<sqlx::Postgres>::new(
        "SELECT al.id, al.user_id, u.username, al.action, \
         al.resource_type, al.resource_id, al.details, \
         host(al.ip_address) AS ip_address, al.user_agent, al.created_at, ",
    );
    qb.push(ACTOR_SQL);
    qb.push(" AS actor FROM audit_log al LEFT JOIN users u ON u.id = al.user_id");

    qb.push(" WHERE al.organization_id = ")
        .push_bind(organization_id);
    let mut need_where = false;
    let prefix = |qb: &mut QueryBuilder<sqlx::Postgres>, need_where: &mut bool| {
        qb.push(if *need_where { " WHERE " } else { " AND " });
        *need_where = false;
    };

    if let Some(ref v) = params.action {
        prefix(&mut qb, &mut need_where);
        qb.push("al.action = ").push_bind(v);
    }
    if let Some(ref v) = params.resource_type {
        prefix(&mut qb, &mut need_where);
        qb.push("al.resource_type = ").push_bind(v);
    }
    if let Some(ref v) = params.user_id {
        let uid = Uuid::parse_str(v.trim())
            .map_err(|_| AppError::BadRequest("user_id must be a UUID".into()))?;
        prefix(&mut qb, &mut need_where);
        qb.push("al.user_id = ").push_bind(uid);
    }
    if let Some(ref v) = params.username {
        // Filter on the same expression the /audit/actors dropdown is built
        // from, so a name it offers always returns its rows.
        prefix(&mut qb, &mut need_where);
        qb.push(ACTOR_SQL).push(" = ").push_bind(v);
    }
    if let Some(ref v) = params.start_date {
        prefix(&mut qb, &mut need_where);
        qb.push("al.created_at >= ")
            .push_bind(v)
            .push("::timestamptz");
    }
    if let Some(ref v) = params.end_date {
        prefix(&mut qb, &mut need_where);
        qb.push("al.created_at <= ")
            .push_bind(v)
            .push("::timestamptz");
    }

    qb.push(" ORDER BY al.created_at DESC LIMIT ")
        .push_bind(limit)
        .push(" OFFSET ")
        .push_bind(offset);

    let rows = qb
        .build()
        .fetch_all(&state.pool)
        .await
        .map_err(AppError::Db)?;
    Ok(Json(rows.iter().map(serialize_row).collect()))
}

pub async fn list_audit_actors(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let organization_id = organization_id(&principal)?;
    let sql = format!(
        "SELECT {ACTOR_SQL} AS actor, COUNT(*) AS entry_count, MAX(al.created_at) AS last_seen \
         FROM audit_log al LEFT JOIN users u ON u.id = al.user_id \
         WHERE al.organization_id = $1 \
         GROUP BY {ACTOR_SQL} ORDER BY COUNT(*) DESC, actor"
    );
    let rows = sqlx::query(&sql)
        .bind(organization_id)
        .fetch_all(&state.pool)
        .await
        .map_err(AppError::Db)?;
    let out = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "actor": opt_string(r, "actor"),
                "entry_count": r.try_get::<i64, _>("entry_count").unwrap_or(0),
                "last_seen": opt_ts(r, "last_seen"),
            })
        })
        .collect();
    Ok(Json(out))
}

pub async fn audit_stats(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<serde_json::Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let actions = sqlx::query(
        "SELECT action, COUNT(*) AS cnt FROM audit_log WHERE organization_id = $1 GROUP BY action ORDER BY cnt DESC",
    )
    .bind(organization_id)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let recent_24h: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM audit_log WHERE organization_id = $1 AND created_at >= NOW() - INTERVAL '24 hours'",
    )
    .bind(organization_id)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let logins = sqlx::query(
        "SELECT u.username, u.role, COUNT(*) AS login_count \
         FROM audit_log al JOIN users u ON u.id = al.user_id \
         WHERE al.organization_id = $1 AND al.action = 'login' AND al.created_at >= NOW() - INTERVAL '7 days' \
         GROUP BY u.username, u.role ORDER BY login_count DESC LIMIT 10",
    )
    .bind(organization_id)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let total: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM audit_log WHERE organization_id = $1")
        .bind(organization_id)
        .fetch_one(&state.pool)
        .await
        .map_err(AppError::Db)?;

    let actions_breakdown: Vec<serde_json::Value> = actions
        .iter()
        .map(|r| {
            serde_json::json!({
                "action": opt_string(r, "action"),
                "cnt": r.try_get::<i64, _>("cnt").unwrap_or(0),
            })
        })
        .collect();

    let recent_logins: Vec<serde_json::Value> = logins
        .iter()
        .map(|r| {
            serde_json::json!({
                "username": opt_string(r, "username"),
                "role": opt_string(r, "role"),
                "login_count": r.try_get::<i64, _>("login_count").unwrap_or(0),
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "total_entries": total.0,
        "recent_24h": recent_24h.0,
        "actions_breakdown": actions_breakdown,
        "recent_logins": recent_logins,
    })))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_audit_log))
        .route("/actors", get(list_audit_actors))
        .route("/stats", get(audit_stats))
}
