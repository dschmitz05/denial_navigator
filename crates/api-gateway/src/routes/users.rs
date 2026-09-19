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
    "system_admin",
    "security_admin",
    "revenue_cycle_manager",
    "billing_specialist",
    "coding_specialist",
    "auditor",
    "read_only",
];
const MANAGER_UP: &[&str] = &["revenue_cycle_manager", "system_admin"];

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

fn organization_id(principal: &Principal) -> Result<Uuid, AppError> {
    principal
        .organization_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(AppError::Forbidden)
}

/// Accounts are presently global identities. Until account attributes become
/// membership attributes, refuse tenant administration of a shared account so
/// an action in one organization cannot change another organization's access.
async fn require_exclusive_member(
    pool: &sqlx::PgPool,
    organization_id: Uuid,
    user_id: Uuid,
) -> Result<(), AppError> {
    let allowed: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM organization_memberships WHERE organization_id = $1 AND user_id = $2) \
         AND NOT EXISTS(SELECT 1 FROM organization_memberships WHERE organization_id <> $1 AND user_id = $2)",
    )
    .bind(organization_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .map_err(AppError::Db)?;
    if allowed {
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}

async fn require_exclusive_member_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    organization_id: Uuid,
    user_id: Uuid,
) -> Result<(), AppError> {
    let memberships = sqlx::query(
        "SELECT organization_id FROM organization_memberships WHERE user_id = $1 FOR UPDATE",
    )
    .bind(user_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(AppError::Db)?;
    let allowed = memberships.len() == 1
        && memberships[0].try_get::<Uuid, _>("organization_id").ok() == Some(organization_id);
    if allowed {
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
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
async fn release_queue_items_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    organization_id: Uuid,
    user_id: Uuid,
) -> Result<u64, AppError> {
    let sql = format!(
        "UPDATE appeals_queue aq SET assigned_user_id = NULL, updated_at = NOW() \
         FROM denials d JOIN claims c ON c.id = d.claim_id \
         WHERE aq.denial_id = d.id AND aq.assigned_user_id = $1 \
         AND c.organization_id = $2 AND {OPEN_QUEUE_CLAUSE} RETURNING aq.id"
    );
    let rows = sqlx::query(&sql)
        .bind(user_id)
        .bind(organization_id)
        .fetch_all(&mut **tx)
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
        "INSERT INTO audit_log (organization_id, user_id, action, resource_type, resource_id, details, ip_address) \
         VALUES ($1::uuid, $2::uuid, $3, $4, $5::uuid, $6::jsonb, $7::inet)",
    )
    .bind(principal.organization_id.as_deref())
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

async fn audit_insert_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    principal: &Principal,
    action: &str,
    resource_type: &str,
    resource_id: &Uuid,
    details: &serde_json::Value,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO audit_log (organization_id, user_id, action, resource_type, resource_id, details, ip_address) \
         VALUES ($1::uuid, $2::uuid, $3, $4, $5::uuid, $6::jsonb, $7::inet)",
    )
    .bind(principal.organization_id.as_deref())
    .bind(principal.user_id.as_deref())
    .bind(action)
    .bind(resource_type)
    .bind(resource_id)
    .bind(details)
    .bind(principal.ip.as_deref())
    .execute(&mut **tx)
    .await
    .map_err(AppError::Db)?;
    Ok(())
}

// ── Handlers ────────────────────────────────────────────────────────────

pub async fn list_users(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<ListUsersQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    require_roles(&principal, &["system_admin", "security_admin"])?;
    let organization_id = organization_id(&principal)?;

    let mut qb = QueryBuilder::<sqlx::Postgres>::new(
        "SELECT u.id, u.username, u.email, u.full_name, om.role, u.is_active, u.created_at, u.last_login, \
         u.totp_required, (u.totp_confirmed_at IS NOT NULL) AS totp_enrolled \
         FROM users u JOIN organization_memberships om ON om.user_id = u.id \
         WHERE om.organization_id = ",
    );
    qb.push_bind(organization_id);

    let search = params
        .search
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let mut need_where = false;
    if let Some(ref role) = params.role {
        if need_where {
            qb.push(" WHERE ");
            need_where = false;
        } else {
            qb.push(" AND ");
        }
        qb.push("om.role = ");
        qb.push_bind(role);
    }
    if let Some(active) = params.is_active {
        if need_where {
            qb.push(" WHERE ");
            need_where = false;
        } else {
            qb.push(" AND ");
        }
        qb.push("u.is_active = ");
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
        qb.push("(u.username ILIKE ");
        qb.push_bind(pattern.clone());
        qb.push(" OR u.email ILIKE ");
        qb.push_bind(pattern.clone());
        qb.push(" OR u.full_name ILIKE ");
        qb.push_bind(pattern);
        qb.push(")");
    }

    qb.push(" ORDER BY u.created_at DESC");
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
    let organization_id = organization_id(&principal)?;

    let rows = sqlx::query(
        // Specialists first: they are who work is usually assigned to.
        "SELECT u.id, u.username, u.full_name, om.role FROM users u \
         JOIN organization_memberships om ON om.user_id = u.id \
         WHERE om.organization_id = $1 AND u.is_active = TRUE \
         ORDER BY CASE om.role WHEN 'billing_specialist' THEN 0 \
                           WHEN 'coding_specialist' THEN 1 \
                           WHEN 'revenue_cycle_manager' THEN 2 \
                         ELSE 3 END, COALESCE(u.full_name, u.username)",
    )
    .bind(organization_id)
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
    require_roles(&principal, &["system_admin", "security_admin"])?;
    let organization_id = organization_id(&principal)?;

    let row = sqlx::query(
        "SELECT u.id, u.username, u.email, u.full_name, om.role, u.is_active, u.created_at, u.last_login \
         FROM users u JOIN organization_memberships om ON om.user_id = u.id \
         WHERE u.id = $1 AND om.organization_id = $2",
    )
    .bind(user_id)
    .bind(organization_id)
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
    require_roles(&principal, &["system_admin", "security_admin"])?;
    let organization_id = organization_id(&principal)?;
    require_exclusive_member(&state.pool, organization_id, user_id).await?;

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
        // Roles are tenant-scoped; membership is updated transactionally below.
    }
    if body.is_active.is_some() {
        sets.push(format!("is_active = ${}", sets.len() + 1));
    }

    if sets.is_empty() && body.role.is_none() {
        return Err(AppError::BadRequest("No updates provided".into()));
    }
    let id_idx = sets.len() + 1;

    // Role and activation changes end existing sessions.
    if body.role.is_some() || body.is_active.is_some() {
        sets.push("sessions_valid_from = date_trunc('second', NOW())".to_string());
    }
    sets.push("updated_at = NOW()".to_string());

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
    if let Some(v) = body.is_active {
        query = query.bind(v);
    }
    let mut tx = state.pool.begin().await.map_err(AppError::Db)?;
    if body.role.is_some() || body.is_active == Some(false) {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(organization_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(AppError::Db)?;
        let current_role: Option<String> = sqlx::query_scalar(
            "SELECT role FROM organization_memberships WHERE organization_id = $1 AND user_id = $2",
        )
        .bind(organization_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(AppError::Db)?;
        if current_role.as_deref() == Some("system_admin")
            && (body.is_active == Some(false) || body.role.as_deref() != Some("system_admin"))
        {
            let admins: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM users u JOIN organization_memberships om ON om.user_id = u.id \
                 WHERE om.organization_id = $1 AND om.role = 'system_admin' \
                   AND u.is_active = TRUE AND u.id != $2",
            )
            .bind(organization_id)
            .bind(user_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(AppError::Db)?;
            if admins == 0 {
                return Err(AppError::BadRequest(
                    "Cannot remove the last active organization administrator".into(),
                ));
            }
        }
    }
    let row = query
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(AppError::Db)?
        .ok_or(AppError::NotFound)?;

    if let Some(ref role) = body.role {
        sqlx::query(
            "UPDATE organization_memberships SET role = $1 \
             WHERE organization_id = $2 AND user_id = $3",
        )
        .bind(role)
        .bind(organization_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Db)?;
    }
    let mut released: u64 = 0;
    if body.is_active == Some(false) {
        let sql = format!(
            "UPDATE appeals_queue aq SET assigned_user_id = NULL, updated_at = NOW() \
             FROM denials d JOIN claims c ON c.id = d.claim_id \
             WHERE aq.denial_id = d.id AND aq.assigned_user_id = $1 \
             AND c.organization_id = $2 AND {OPEN_QUEUE_CLAUSE} RETURNING aq.id"
        );
        released = sqlx::query(&sql)
            .bind(user_id)
            .bind(organization_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(AppError::Db)?
            .len() as u64;
    }
    tx.commit().await.map_err(AppError::Db)?;

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
    if let Some(role) = body.role {
        result["role"] = serde_json::Value::String(role);
    }
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
    require_roles(&principal, &["system_admin", "security_admin"])?;
    let organization_id = organization_id(&principal)?;
    require_exclusive_member(&state.pool, organization_id, user_id).await?;

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
    require_roles(&principal, &["system_admin", "security_admin"])?;
    let organization_id = organization_id(&principal)?;
    let _user = sqlx::query(
        "SELECT u.id, u.username, om.role FROM users u \
         JOIN organization_memberships om ON om.user_id = u.id \
         WHERE u.id = $1 AND om.organization_id = $2",
    )
    .bind(user_id)
    .bind(organization_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;
    let mut username: String;
    let mut role: String;

    // Prevent self-deletion
    if is_self(&principal, &user_id) {
        return Err(AppError::BadRequest("Cannot delete yourself".into()));
    }

    let mut tx = state.pool.begin().await.map_err(AppError::Db)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(organization_id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(AppError::Db)?;
    sqlx::query("LOCK TABLE organization_memberships IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await
        .map_err(AppError::Db)?;
    require_exclusive_member_tx(&mut tx, organization_id, user_id).await?;
    let locked_user = sqlx::query(
        "SELECT u.username, om.role FROM users u \
         JOIN organization_memberships om ON om.user_id = u.id \
         WHERE u.id = $1 AND om.organization_id = $2 FOR UPDATE",
    )
    .bind(user_id)
    .bind(organization_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;
    username = locked_user.try_get("username").map_err(AppError::Db)?;
    role = locked_user.try_get("role").map_err(AppError::Db)?;

    // Prevent deleting the last admin
    let admin_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM users u JOIN organization_memberships om ON om.user_id = u.id \
         WHERE om.organization_id = $1 AND om.role = 'system_admin' AND u.id != $2 AND u.is_active = TRUE",
    )
    .bind(organization_id)
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::Db)?;
    if role == "system_admin" && admin_count.0 == 0 {
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
        let released = release_queue_items_tx(&mut tx, organization_id, user_id).await?;
        let closed_items: (i64,) = sqlx::query_as(&format!(
            "SELECT COUNT(*) FROM appeals_queue aq JOIN denials d ON d.id = aq.denial_id \
             JOIN claims c ON c.id = d.claim_id \
             WHERE aq.assigned_user_id = $1 AND c.organization_id = $2 AND NOT {OPEN_QUEUE_CLAUSE}"
        ))
        .bind(user_id)
        .bind(organization_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::Db)?;
        let audit_entries: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM audit_log WHERE user_id = $1 AND organization_id = $2",
        )
        .bind(user_id)
        .bind(organization_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(AppError::Db)?;

        // Written BEFORE the row disappears, so the log records who was
        // erased, by whom, and what it cost.
        audit_insert_tx(
            &mut tx,
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
        .await?;
        sqlx::query(
            "UPDATE audit_log SET user_id = NULL \
             WHERE organization_id = $1 AND user_id = $2",
        )
        .bind(organization_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Db)?;
        // No foreign keys exist, so these would otherwise be left pointing at
        // an id that resolves to nobody.
        sqlx::query(&format!(
            "UPDATE appeals_queue aq SET assigned_user_id = NULL \
             FROM denials d JOIN claims c ON c.id = d.claim_id \
             WHERE aq.denial_id = d.id AND aq.assigned_user_id = $1 \
               AND c.organization_id = $2"
        ))
        .bind(user_id)
        .bind(organization_id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Db)?;
        sqlx::query(
            "UPDATE feedback_loop fl SET user_id = NULL \
             FROM ai_analyses aa JOIN claims c ON c.id = aa.claim_id \
             WHERE fl.ai_analysis_id = aa.id AND fl.user_id = $1 \
               AND c.organization_id = $2",
        )
        .bind(user_id)
        .bind(organization_id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Db)?;
        sqlx::query("DELETE FROM notifications WHERE organization_id = $1 AND user_id = $2")
            .bind(organization_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(AppError::Db)?;
        sqlx::query(
            "DELETE FROM organization_memberships WHERE organization_id = $1 AND user_id = $2",
        )
        .bind(organization_id)
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
        .execute(&mut *tx)
        .await
        .map_err(AppError::Db)?;

    // Their live work goes back in the pool, or it is orphaned.
    let released = release_queue_items_tx(&mut tx, organization_id, user_id).await?;

    audit_insert_tx(
        &mut tx,
        &principal,
        "deactivate_user",
        "user",
        &user_id,
        &serde_json::json!({ "username": username, "queue_items_released": released }),
    )
    .await?;
    tx.commit().await.map_err(AppError::Db)?;

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
    require_roles(&principal, &["system_admin", "security_admin"])?;
    let organization_id = organization_id(&principal)?;
    require_exclusive_member(&state.pool, organization_id, user_id).await?;

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
    require_roles(&principal, &["system_admin", "security_admin"])?;
    let organization_id = organization_id(&principal)?;
    require_exclusive_member(&state.pool, organization_id, user_id).await?;

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
