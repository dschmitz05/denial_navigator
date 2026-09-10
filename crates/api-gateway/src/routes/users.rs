//! User management routes (admin only, plus the assignable list for managers).

use axum::extract::{Extension, Path, Query, State};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use chrono::{DateTime, NaiveDate, Utc};
use denial_auth::auth::hash_password;
use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use serde::Deserialize;
use sqlx::{Column, QueryBuilder, Row};
use uuid::Uuid;

use crate::state::AppState;

const VALID_ROLES: &[&str] = &[
    "billing_specialist",
    "billing_manager",
    "rcm_director",
    "admin",
];
const MANAGER_UP: &[&str] = &["billing_manager", "rcm_director", "admin"];

// Outcomes that mean a queue item is finished. Assignment on a CLOSED item is
// the record of who did the work, so it is left alone; only live work moves.
const OPEN_QUEUE_CLAUSE: &str = "(outcome_status IS NULL \
     OR outcome_status NOT IN ('approved', 'overruled', 'resolved', 'denied_again', 'cancelled'))";

#[derive(Deserialize)]
struct UserUpdate {
    email: Option<String>,
    full_name: Option<String>,
    role: Option<String>,
    is_active: Option<bool>,
}

#[derive(Deserialize)]
struct UserPasswordUpdate {
    password: String,
}

#[derive(Deserialize)]
struct TotpPolicy {
    required: bool,
}

#[derive(Deserialize)]
struct ListUsersQuery {
    role: Option<String>,
    is_active: Option<bool>,
    search: Option<String>,
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
}

fn default_limit() -> i64 {
    100
}

#[derive(Deserialize)]
struct DeleteUserQuery {
    #[serde(default)]
    purge: bool,
    #[serde(default)]
    confirm_username: Option<String>,
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn require_roles(principal: &Principal, roles: &[&str]) -> Result<(), AppError> {
    match principal.role.as_deref() {
        Some(role) if roles.contains(&role) => Ok(()),
        _ => Err(AppError::Forbidden),
    }
}

fn is_self(principal: &Principal, user_id: &Uuid) -> bool {
    principal.user_id.as_deref() == Some(user_id.to_string().as_str())
}

fn row_to_json(row: &sqlx::postgres::PgRow) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for col in row.columns().iter() {
        let name = col.name();
        let val = row
            .try_get::<Option<String>, _>(name)
            .map(|v| {
                v.map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null)
            })
            .or_else(|_| {
                row.try_get::<Option<i64>, _>(name).map(|v| {
                    v.map(serde_json::Value::from)
                        .unwrap_or(serde_json::Value::Null)
                })
            })
            .or_else(|_| {
                row.try_get::<Option<f64>, _>(name).map(|v| {
                    v.map(serde_json::Value::from)
                        .unwrap_or(serde_json::Value::Null)
                })
            })
            .or_else(|_| {
                row.try_get::<Option<bool>, _>(name).map(|v| {
                    v.map(serde_json::Value::Bool)
                        .unwrap_or(serde_json::Value::Null)
                })
            })
            .or_else(|_| {
                row.try_get::<Option<Uuid>, _>(name).map(|v| {
                    v.map(|v| serde_json::Value::String(v.to_string()))
                        .unwrap_or(serde_json::Value::Null)
                })
            })
            .or_else(|_| {
                row.try_get::<Option<NaiveDate>, _>(name).map(|v| {
                    v.map(|v| serde_json::Value::String(v.to_string()))
                        .unwrap_or(serde_json::Value::Null)
                })
            })
            .or_else(|_| {
                row.try_get::<Option<DateTime<Utc>>, _>(name).map(|v| {
                    v.map(|v| serde_json::Value::String(v.to_rfc3339()))
                        .unwrap_or(serde_json::Value::Null)
                })
            })
            .unwrap_or(serde_json::Value::Null);
        map.insert(name.to_string(), val);
    }
    serde_json::Value::Object(map)
}

/// Return a deactivated user's live queue items to the shared pool.
///
/// Unassigning is better than refusing to deactivate the user (offboarding
/// should not be blocked by a queue) and better than deleting the items,
/// which are real outstanding money.
async fn release_queue_items(pool: &sqlx::PgPool, user_id: Uuid) -> Result<u64, AppError> {
    let sql = format!(
        "UPDATE appeals_queue SET assigned_user_id = NULL, updated_at = NOW() \
         WHERE assigned_user_id = $1 AND {OPEN_QUEUE_CLAUSE} RETURNING id"
    );
    let rows = sqlx::query(&sql)
        .bind(user_id)
        .fetch_all(pool)
        .await
        .map_err(AppError::Db)?;
    Ok(rows.len() as u64)
}

async fn audit_insert(
    pool: &sqlx::PgPool,
    principal: &Principal,
    action: &str,
    resource_type: &str,
    resource_id: &Uuid,
    details: &serde_json::Value,
) {
    let result = sqlx::query(
        "INSERT INTO audit_log (user_id, action, resource_type, resource_id, details, ip_address) \
         VALUES ($1::uuid, $2, $3, $4::uuid, $5::jsonb, $6::inet)",
    )
    .bind(principal.user_id.as_deref())
    .bind(action)
    .bind(resource_type)
    .bind(resource_id)
    .bind(details)
    .bind(principal.ip.as_deref())
    .execute(pool)
    .await;
    if let Err(e) = result {
        tracing::warn!("audit write failed for {action}: {e}");
    }
}

// ── Handlers ────────────────────────────────────────────────────────────

pub async fn list_users(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<ListUsersQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    require_roles(&principal, &["admin"])?;

    let mut qb = QueryBuilder::<sqlx::Postgres>::new(
        "SELECT id, username, email, full_name, role, is_active, created_at, last_login, \
         totp_required, (totp_confirmed_at IS NOT NULL) AS totp_enrolled FROM users",
    );

    let search = params
        .search
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let mut need_where = true;
    if let Some(ref role) = params.role {
        if need_where {
            qb.push(" WHERE ");
            need_where = false;
        } else {
            qb.push(" AND ");
        }
        qb.push("role = ");
        qb.push_bind(role);
    }
    if let Some(active) = params.is_active {
        if need_where {
            qb.push(" WHERE ");
            need_where = false;
        } else {
            qb.push(" AND ");
        }
        qb.push("is_active = ");
        qb.push_bind(active);
    }
    if let Some(search) = search {
        if need_where {
            qb.push(" WHERE ");
            need_where = false;
        } else {
            qb.push(" AND ");
        }
        let pattern = format!("%{search}%");
        qb.push("(username ILIKE ");
        qb.push_bind(pattern.clone());
        qb.push(" OR email ILIKE ");
        qb.push_bind(pattern.clone());
        qb.push(" OR full_name ILIKE ");
        qb.push_bind(pattern);
        qb.push(")");
    }

    qb.push(" ORDER BY created_at DESC");
    qb.push(" LIMIT ").push_bind(params.limit.clamp(1, 500));
    qb.push(" OFFSET ").push_bind(params.offset.max(0));

    let rows = qb
        .build()
        .fetch_all(&state.pool)
        .await
        .map_err(AppError::Db)?;
    Ok(Json(rows.iter().map(row_to_json).collect()))
}

pub async fn list_assignable_users(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    require_roles(&principal, MANAGER_UP)?;

    let rows = sqlx::query(
        // Specialists first: they are who work is usually assigned to.
        "SELECT id, username, full_name, role FROM users WHERE is_active = TRUE \
         ORDER BY CASE role WHEN 'billing_specialist' THEN 0 \
                       WHEN 'billing_manager' THEN 1 \
                       WHEN 'rcm_director' THEN 2 \
                       ELSE 3 END, COALESCE(full_name, username)",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    Ok(Json(rows.iter().map(row_to_json).collect()))
}

pub async fn get_user(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(user_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_roles(&principal, &["admin"])?;

    let row = sqlx::query(
        "SELECT id, username, email, full_name, role, is_active, created_at, last_login \
         FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;

    Ok(Json(row_to_json(&row)))
}

pub async fn update_user(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(user_id): Path<Uuid>,
    Json(body): Json<UserUpdate>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_roles(&principal, &["admin"])?;

    // Prevent self-modification of role
    if is_self(&principal, &user_id) && body.role.is_some() {
        return Err(AppError::BadRequest("Cannot change your own role".into()));
    }

    // Check if email would conflict
    if let Some(ref email) = body.email {
        let existing = sqlx::query("SELECT id FROM users WHERE email = $1 AND id != $2")
            .bind(email)
            .bind(user_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(AppError::Db)?;
        if existing.is_some() {
            return Err(AppError::Conflict("Email already in use".into()));
        }
    }

    let mut sets = Vec::new();
    if body.email.is_some() {
        sets.push(format!("email = ${}", sets.len() + 1));
    }
    if body.full_name.is_some() {
        sets.push(format!("full_name = ${}", sets.len() + 1));
    }
    if let Some(ref role) = body.role {
        if !VALID_ROLES.contains(&role.as_str()) {
            return Err(AppError::BadRequest(format!(
                "Invalid role. Must be one of: {VALID_ROLES:?}"
            )));
        }
        sets.push(format!("role = ${}", sets.len() + 1));
    }
    if body.is_active.is_some() {
        sets.push(format!("is_active = ${}", sets.len() + 1));
    }

    if sets.is_empty() {
        return Err(AppError::BadRequest("No updates provided".into()));
    }

    // A role change ends existing sessions, like a password change does.
    if body.role.is_some() {
        sets.push("sessions_valid_from = date_trunc('second', NOW())".to_string());
    }
    sets.push("updated_at = NOW()".to_string());
    let id_idx = sets.len() + 1;

    let sql = format!(
        "UPDATE users SET {} WHERE id = ${} \
         RETURNING id, username, email, full_name, role, is_active, created_at, last_login",
        sets.join(", "),
        id_idx
    );
    let mut query = sqlx::query(&sql);
    if let Some(ref v) = body.email {
        query = query.bind(v);
    }
    if let Some(ref v) = body.full_name {
        query = query.bind(v);
    }
    if let Some(ref v) = body.role {
        query = query.bind(v);
    }
    if let Some(v) = body.is_active {
        query = query.bind(v);
    }
    let row = query
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::Db)?
        .ok_or(AppError::NotFound)?;

    // Deactivating through this route is the same event as DELETE, so it must
    // release the same work.
    let mut released: u64 = 0;
    if body.is_active == Some(false) {
        released = release_queue_items(&state.pool, user_id).await?;
    }

    let mut changes = serde_json::Map::new();
    if let Some(v) = &body.email {
        changes.insert("email".into(), serde_json::Value::String(v.clone()));
    }
    if let Some(v) = &body.full_name {
        changes.insert("full_name".into(), serde_json::Value::String(v.clone()));
    }
    if let Some(v) = &body.role {
        changes.insert("role".into(), serde_json::Value::String(v.clone()));
    }
    if let Some(v) = body.is_active {
        changes.insert("is_active".into(), serde_json::Value::Bool(v));
    }
    let mut details = serde_json::json!({ "changes": changes });
    if released > 0 {
        details["queue_items_released"] = serde_json::json!(released);
    }
    audit_insert(
        &state.pool,
        &principal,
        "update_user",
        "user",
        &user_id,
        &details,
    )
    .await;

    let mut result = row_to_json(&row);
    if body.is_active == Some(false) {
        result["queue_items_released"] = serde_json::json!(released);
    }
    Ok(Json(result))
}

pub async fn reset_user_password(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(user_id): Path<Uuid>,
    Json(body): Json<UserPasswordUpdate>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_roles(&principal, &["admin"])?;

    let user = sqlx::query("SELECT id, username FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::Db)?
        .ok_or(AppError::NotFound)?;
    let username: String = user.try_get("username").map_err(AppError::Db)?;

    let hashed = hash_password(&body.password).map_err(|e| AppError::Internal(e.to_string()))?;

    // An admin resetting a password is usually responding to a compromise, so
    // the sessions opened with the old one must end too.
    sqlx::query(
        "UPDATE users SET password_hash = $1, \
         sessions_valid_from = date_trunc('second', NOW()), updated_at = NOW() WHERE id = $2",
    )
    .bind(&hashed)
    .bind(user_id)
    .execute(&state.pool)
    .await
    .map_err(AppError::Db)?;

    audit_insert(
        &state.pool,
        &principal,
        "reset_password",
        "user",
        &user_id,
        &serde_json::json!({ "target_username": username }),
    )
    .await;

    Ok(Json(serde_json::json!({ "status": "password_reset" })))
}

pub async fn delete_user(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(user_id): Path<Uuid>,
    Query(params): Query<DeleteUserQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_roles(&principal, &["admin"])?;

    let user = sqlx::query("SELECT id, username, role FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::Db)?
        .ok_or(AppError::NotFound)?;
    let username: String = user.try_get("username").map_err(AppError::Db)?;
    let role: String = user.try_get("role").map_err(AppError::Db)?;

    // Prevent self-deletion
    if is_self(&principal, &user_id) {
        return Err(AppError::BadRequest("Cannot delete yourself".into()));
    }

    // Prevent deleting the last admin
    let admin_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM users WHERE role = 'admin' AND id != $1 AND is_active = TRUE",
    )
    .bind(user_id)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;
    if role == "admin" && admin_count.0 == 0 {
        return Err(AppError::BadRequest(
            "Cannot delete the last admin user".into(),
        ));
    }

    if params.purge {
        if params.confirm_username.as_deref() != Some(username.as_str()) {
            return Err(AppError::BadRequest(format!(
                "Permanent deletion requires confirm_username to match the account exactly (expected '{username}')"
            )));
        }

        // Their live work goes back to the pool; closed items lose the record
        // of who did them, because that record was the user row.
        let released = release_queue_items(&state.pool, user_id).await?;
        let closed_items: (i64,) = sqlx::query_as(&format!(
            "SELECT COUNT(*) FROM appeals_queue \
                 WHERE assigned_user_id = $1 AND NOT {OPEN_QUEUE_CLAUSE}"
        ))
        .bind(user_id)
        .fetch_one(&state.pool)
        .await
        .map_err(AppError::Db)?;
        let audit_entries: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM audit_log WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&state.pool)
                .await
                .map_err(AppError::Db)?;

        // Written BEFORE the row disappears, so the log records who was
        // erased, by whom, and what it cost.
        audit_insert(
            &state.pool,
            &principal,
            "purge_user",
            "user",
            &user_id,
            &serde_json::json!({
                "username": username,
                "role": role,
                "queue_items_released": released,
                "closed_items_unattributed": closed_items.0,
                "audit_entries_orphaned": audit_entries.0,
            }),
        )
        .await;

        let mut tx = state.pool.begin().await.map_err(AppError::Db)?;
        // No foreign keys exist, so these would otherwise be left pointing at
        // an id that resolves to nobody.
        sqlx::query("UPDATE appeals_queue SET assigned_user_id = NULL WHERE assigned_user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(AppError::Db)?;
        sqlx::query("UPDATE feedback_loop SET user_id = NULL WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(AppError::Db)?;
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(AppError::Db)?;
        tx.commit().await.map_err(AppError::Db)?;

        tracing::warn!(
            "PURGED user {username} ({user_id}) by {}",
            principal.username
        );
        return Ok(Json(serde_json::json!({
            "status": "purged",
            "username": username,
            "queue_items_released": released,
            "closed_items_unattributed": closed_items.0,
            "audit_entries_orphaned": audit_entries.0,
        })));
    }

    // Soft delete by deactivating
    sqlx::query("UPDATE users SET is_active = FALSE, updated_at = NOW() WHERE id = $1")
        .bind(user_id)
        .execute(&state.pool)
        .await
        .map_err(AppError::Db)?;

    // Their live work goes back in the pool, or it is orphaned.
    let released = release_queue_items(&state.pool, user_id).await?;

    audit_insert(
        &state.pool,
        &principal,
        "deactivate_user",
        "user",
        &user_id,
        &serde_json::json!({ "username": username, "queue_items_released": released }),
    )
    .await;

    Ok(Json(serde_json::json!({
        "status": "deactivated",
        "queue_items_released": released,
    })))
}

// ── Two-factor administration ───────────────────────────────────────────
//
// An administrator decides WHETHER an account uses 2FA and can reset it when a
// device is lost. They never see or set the secret: enrolment happens between
// the user and their authenticator, so an admin cannot generate codes for
// someone else's account.

pub async fn set_totp_policy(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(user_id): Path<Uuid>,
    Json(body): Json<TotpPolicy>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_roles(&principal, &["admin"])?;

    let user = sqlx::query(
        "SELECT id, username, totp_required, totp_confirmed_at FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;
    let username: String = user.try_get("username").map_err(AppError::Db)?;
    let totp_confirmed: Option<DateTime<Utc>> =
        user.try_get("totp_confirmed_at").map_err(AppError::Db)?;

    let outcome = if body.required {
        // Turning it on does not clear an existing enrolment: an admin
        // toggling the policy should not silently invalidate a working
        // authenticator the user still holds.
        sqlx::query("UPDATE users SET totp_required = TRUE, updated_at = NOW() WHERE id = $1")
            .bind(user_id)
            .execute(&state.pool)
            .await
            .map_err(AppError::Db)?;
        if totp_confirmed.is_some() {
            "enrolled".to_string()
        } else {
            "enrollment_pending".to_string()
        }
    } else {
        // Turning it off discards the secret. Leaving it behind would mean
        // re-enabling 2FA later silently re-activates a device that may be
        // long gone, and keeps a password-equivalent secret for no reason.
        sqlx::query(
            "UPDATE users SET totp_required = FALSE, totp_secret = NULL, \
             totp_confirmed_at = NULL, totp_last_used_step = NULL, updated_at = NOW() \
             WHERE id = $1",
        )
        .bind(user_id)
        .execute(&state.pool)
        .await
        .map_err(AppError::Db)?;
        "disabled".to_string()
    };

    audit_insert(
        &state.pool,
        &principal,
        if body.required {
            "totp_required"
        } else {
            "totp_disabled"
        },
        "user",
        &user_id,
        &serde_json::json!({ "username": username, "outcome": outcome }),
    )
    .await;

    Ok(Json(serde_json::json!({
        "status": outcome,
        "username": username,
        "totp_required": body.required,
    })))
}

pub async fn reset_totp(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(user_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_roles(&principal, &["admin"])?;

    let user = sqlx::query("SELECT id, username, totp_required FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::Db)?
        .ok_or(AppError::NotFound)?;
    let username: String = user.try_get("username").map_err(AppError::Db)?;
    let totp_required: bool = user.try_get("totp_required").map_err(AppError::Db)?;

    // Every session is also ended: if the device is lost rather than
    // replaced, leaving existing sessions running would be the obvious gap.
    sqlx::query(
        "UPDATE users SET totp_secret = NULL, totp_confirmed_at = NULL, \
         totp_last_used_step = NULL, \
         sessions_valid_from = date_trunc('second', NOW()), updated_at = NOW() \
         WHERE id = $1",
    )
    .bind(user_id)
    .execute(&state.pool)
    .await
    .map_err(AppError::Db)?;

    audit_insert(
        &state.pool,
        &principal,
        "totp_reset",
        "user",
        &user_id,
        &serde_json::json!({ "username": username, "still_required": totp_required }),
    )
    .await;

    Ok(Json(serde_json::json!({
        "status": "reset",
        "username": username,
        "totp_required": totp_required,
        "note": "The user will be asked to set up an authenticator at their next sign-in.",
    })))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_users))
        .route("/assignable", get(list_assignable_users))
        .route(
            "/{user_id}",
            get(get_user).patch(update_user).delete(delete_user),
        )
        .route("/{user_id}/password", post(reset_user_password))
        .route("/{user_id}/totp", post(set_totp_policy))
        .route("/{user_id}/totp/reset", post(reset_totp))
}
