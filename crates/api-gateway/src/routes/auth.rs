use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use denial_auth::auth::{
    create_token, create_token_for_organization, decode_token, hash_password, verify_password,
};
use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use denial_common::totp;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::state::AppState;

const LOGIN_USER_LIMIT: i64 = 6;
const LOGIN_IP_LIMIT: i64 = 20;
const LOGIN_WINDOW_MINUTES: i64 = 5;
const MIN_PASSWORD_LENGTH: usize = 8;
const TOTP_FAILURE_LIMIT: i64 = 5;
const TOTP_WINDOW_MINUTES: i64 = 10;

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    pub organization_id: Option<Uuid>,
}

#[derive(Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub email: String,
    pub password: String,
    pub full_name: Option<String>,
    #[serde(default = "default_role")]
    pub role: String,
}

fn default_role() -> String {
    "billing_specialist".to_string()
}

#[derive(Deserialize)]
pub struct PasswordChange {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Deserialize)]
pub struct TotpCode {
    pub code: String,
}

async fn recent_failures(
    pool: &sqlx::PgPool,
    username: &str,
    ip: Option<&str>,
) -> Result<(i64, i64), AppError> {
    let by_user: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM audit_log
         WHERE action = 'login_failed'
           AND lower(details->>'username') = lower($1)
           AND created_at > NOW() - INTERVAL '5 minutes'
           AND created_at > COALESCE((
                 SELECT MAX(created_at) FROM audit_log
                  WHERE action = 'login'
                    AND lower(details->>'username') = lower($1)
               ), 'epoch'::timestamptz)",
    )
    .bind(username)
    .fetch_one(pool)
    .await?;

    let by_ip = if let Some(ip) = ip {
        let row: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM audit_log
             WHERE action = 'login_failed'
               AND ip_address = $1::inet
               AND created_at > NOW() - INTERVAL '5 minutes'",
        )
        .bind(ip)
        .fetch_one(pool)
        .await?;
        row.0
    } else {
        0
    };

    Ok((by_user.0, by_ip))
}

pub async fn login(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(body): Json<LoginRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let ip = principal.ip.as_deref();
    let user_agent = None;

    let (fails_user, fails_ip) = recent_failures(&state.pool, &body.username, ip).await?;
    let over = if fails_user >= LOGIN_USER_LIMIT {
        Some("account")
    } else if fails_ip >= LOGIN_IP_LIMIT {
        Some("address")
    } else {
        None
    };

    if let Some(limit) = over {
        let _ = denial_audit::record(
            &state.pool,
            "login_blocked",
            "user",
            None,
            None,
            &serde_json::json!({
                "username": body.username,
                "reason": "rate_limited",
                "limit": limit,
                "failures_account": fails_user,
                "failures_address": fails_ip,
            }),
            ip,
            user_agent,
            None,
        )
        .await;
        return Err(AppError::RateLimited {
            retry_after: (LOGIN_WINDOW_MINUTES * 60) as u64,
        });
    }

    let user = sqlx::query(
        "SELECT u.id, u.username, u.email, u.full_name, om.role, u.is_active, u.password_hash, \
                u.totp_required, u.totp_confirmed_at, u.last_login \
         FROM users u JOIN organization_memberships om ON om.user_id = u.id \
         JOIN organizations o ON o.id = om.organization_id \
         WHERE u.username = $1 AND u.is_active = TRUE AND o.is_active = TRUE \
           AND ($2::uuid IS NULL OR om.organization_id = $2) \
         ORDER BY om.created_at ASC LIMIT 1",
    )
    .bind(&body.username)
    .bind(body.organization_id)
    .fetch_optional(&state.pool)
    .await?;

    let user = match user {
        Some(u) => u,
        None => {
            let _ = denial_audit::record(
                &state.pool,
                "login_failed",
                "user",
                None,
                None,
                &serde_json::json!({
                    "username": body.username,
                    "reason": "unknown_or_inactive_user",
                }),
                ip,
                user_agent,
                None,
            )
            .await;
            return Err(AppError::Unauthorized);
        }
    };

    let password_hash: String = user
        .try_get("password_hash")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if !verify_password(&body.password, &password_hash) {
        let user_id: Uuid = user
            .try_get("id")
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let _ = denial_audit::record(
            &state.pool,
            "login_failed",
            "user",
            Some(&user_id.to_string()),
            None,
            &serde_json::json!({
                "username": body.username,
                "reason": "bad_password",
            }),
            ip,
            user_agent,
            None,
        )
        .await;
        return Err(AppError::Unauthorized);
    }

    let user_id: Uuid = user
        .try_get("id")
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let organizations = sqlx::query(
        "SELECT o.id, o.name FROM organization_memberships om \
         JOIN organizations o ON o.id = om.organization_id \
         WHERE om.user_id = $1 AND o.is_active = TRUE ORDER BY om.created_at ASC",
    )
    .bind(user_id)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    if body.organization_id.is_none() && organizations.len() > 1 {
        return Ok(Json(serde_json::json!({
            "status": "organization_selection",
            "organizations": organizations.iter().map(|row| serde_json::json!({
                "id": row.try_get::<Uuid, _>("id").unwrap_or_default(),
                "name": row.try_get::<String, _>("name").unwrap_or_default(),
            })).collect::<Vec<_>>(),
        })));
    }
    let organization_id = body
        .organization_id
        .or_else(|| {
            organizations
                .first()
                .and_then(|row| row.try_get::<Uuid, _>("id").ok())
        })
        .ok_or(AppError::Unauthorized)?;
    let organization_context = organization_id.to_string();
    let username: String = user
        .try_get("username")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let email: String = user
        .try_get("email")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let full_name: Option<String> = user
        .try_get("full_name")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let role: String = user
        .try_get("role")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let totp_required: bool = user
        .try_get("totp_required")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let totp_confirmed_at: Option<DateTime<Utc>> = user
        .try_get("totp_confirmed_at")
        .map_err(|e| AppError::Internal(e.to_string()))?;

    sqlx::query("UPDATE users SET last_login = NOW() WHERE id = $1")
        .bind(user_id)
        .execute(&state.pool)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    if totp_required {
        let stage = if totp_confirmed_at.is_some() {
            "totp_required"
        } else {
            "enrollment_required"
        };
        let mfa_token = create_token_for_organization(
            &user_id.to_string(),
            &username,
            &role,
            "mfa",
            Some(&organization_id.to_string()),
            &state.config.jwt_secret,
            10,
        )
        .map_err(|e| AppError::Internal(e.to_string()))?;

        let _ = denial_audit::record(
            &state.pool,
            "login_password_ok",
            "user",
            Some(&user_id.to_string()),
            Some(&user_id.to_string()),
            &serde_json::json!({
                "username": username,
                "awaiting": stage,
            }),
            ip,
            user_agent,
            Some(organization_context.as_str()),
        )
        .await;

        return Ok(Json(serde_json::json!({
            "status": stage,
            "mfa_token": mfa_token,
            "token_type": "mfa",
            "username": username,
        })));
    }

    let token = create_token_for_organization(
        &user_id.to_string(),
        &username,
        &role,
        "full",
        Some(&organization_id.to_string()),
        &state.config.jwt_secret,
        state.config.jwt_expire_minutes,
    )
    .map_err(|e| AppError::Internal(e.to_string()))?;

    let _ = denial_audit::record(
        &state.pool,
        "login",
        "user",
        Some(&user_id.to_string()),
        Some(&user_id.to_string()),
        &serde_json::json!({
            "username": username,
            "role": role,
        }),
        ip,
        user_agent,
        Some(organization_context.as_str()),
    )
    .await;

    Ok(Json(serde_json::json!({
        "access_token": token,
        "token_type": "bearer",
        "user": {
            "id": user_id.to_string(),
            "username": username,
            "email": email,
            "full_name": full_name,
            "role": role,
            "is_active": true,
        },
    })))
}

pub async fn me(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<serde_json::Value>, AppError> {
    let user_id = principal.user_id.as_ref().ok_or(AppError::Unauthorized)?;

    let row = sqlx::query(
        "SELECT u.id, u.username, u.email, u.full_name, om.role, u.is_active, u.last_login, \
                u.totp_required, (u.totp_confirmed_at IS NOT NULL) AS totp_enrolled \
         FROM users u JOIN organization_memberships om ON om.user_id = u.id \
         JOIN organizations o ON o.id = om.organization_id \
         WHERE u.id = $1 AND o.is_active = TRUE \
           AND om.organization_id = $2::uuid \
         ORDER BY om.created_at ASC LIMIT 1",
    )
    .bind(Uuid::parse_str(user_id).map_err(|_| AppError::Unauthorized)?)
    .bind(
        principal
            .organization_id
            .as_deref()
            .ok_or(AppError::Unauthorized)?,
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let row = row.ok_or(AppError::Unauthorized)?;
    let id: Uuid = row
        .try_get("id")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let username: String = row
        .try_get("username")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let email: String = row
        .try_get("email")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let full_name: Option<String> = row
        .try_get("full_name")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let role: String = row
        .try_get("role")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let is_active: bool = row
        .try_get("is_active")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let last_login: Option<DateTime<Utc>> = row
        .try_get("last_login")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let totp_required: bool = row
        .try_get("totp_required")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let totp_enrolled: bool = row
        .try_get("totp_enrolled")
        .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(Json(serde_json::json!({
        "id": id.to_string(),
        "username": username,
        "email": email,
        "full_name": full_name,
        "role": role,
        "is_active": is_active,
        "last_login": last_login.map(|t| t.to_rfc3339()),
        "totp_required": totp_required,
        "totp_enrolled": totp_enrolled,
    })))
}

pub async fn register(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(body): Json<RegisterRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let role = principal.role.as_deref().ok_or(AppError::Forbidden)?;
    if !matches!(role, "system_admin" | "security_admin") {
        return Err(AppError::Forbidden);
    }
    let organization_id = principal
        .organization_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(AppError::Forbidden)?;

    if body.password.len() < MIN_PASSWORD_LENGTH {
        return Err(AppError::BadRequest(format!(
            "Password must be at least {MIN_PASSWORD_LENGTH} characters"
        )));
    }

    let valid_roles = [
        "system_admin",
        "security_admin",
        "revenue_cycle_manager",
        "billing_specialist",
        "coding_specialist",
        "auditor",
        "read_only",
    ];
    if !valid_roles.contains(&body.role.as_str()) {
        return Err(AppError::BadRequest(format!(
            "Invalid role. Must be one of: {valid_roles:?}"
        )));
    }

    let existing = sqlx::query("SELECT id FROM users WHERE username = $1 OR email = $2")
        .bind(&body.username)
        .bind(&body.email)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::Db)?;

    if existing.is_some() {
        return Err(AppError::Conflict(
            "Username or email already exists".into(),
        ));
    }

    let hashed = hash_password(&body.password).map_err(|e| AppError::Internal(e.to_string()))?;
    let row = sqlx::query(
        "WITH inserted AS ( \
             INSERT INTO users (username, email, password_hash, full_name, role, is_active) \
             VALUES ($1, $2, $3, $4, $5, TRUE) \
             RETURNING id, username, email, full_name, role, is_active \
         ), membership AS ( \
             INSERT INTO organization_memberships (organization_id, user_id, role) \
             SELECT $6, id, role FROM inserted \
         ) \
         SELECT id, username, email, full_name, role, is_active FROM inserted",
    )
    .bind(&body.username)
    .bind(&body.email)
    .bind(&hashed)
    .bind(&body.full_name)
    .bind(&body.role)
    .bind(organization_id)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let id: Uuid = row
        .try_get("id")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let username: String = row
        .try_get("username")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let email: String = row
        .try_get("email")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let full_name: Option<String> = row
        .try_get("full_name")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let role: String = row
        .try_get("role")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let is_active: bool = row
        .try_get("is_active")
        .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(Json(serde_json::json!({
        "id": id.to_string(),
        "username": username,
        "email": email,
        "full_name": full_name,
        "role": role,
        "is_active": is_active,
    })))
}

pub async fn change_password(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(body): Json<PasswordChange>,
) -> Result<Json<serde_json::Value>, AppError> {
    if body.new_password.len() < MIN_PASSWORD_LENGTH {
        return Err(AppError::BadRequest(format!(
            "New password must be at least {MIN_PASSWORD_LENGTH} characters"
        )));
    }
    if body.new_password == body.current_password {
        return Err(AppError::BadRequest(
            "New password must be different from the current one".into(),
        ));
    }

    let user_id = principal.user_id.as_ref().ok_or(AppError::Unauthorized)?;
    let uid = Uuid::parse_str(user_id).map_err(|_| AppError::Unauthorized)?;

    let user = sqlx::query(
        "SELECT id, username, password_hash FROM users WHERE id = $1 AND is_active = TRUE",
    )
    .bind(uid)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;

    let password_hash: String = user
        .try_get("password_hash")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if !verify_password(&body.current_password, &password_hash) {
        return Err(AppError::Unauthorized);
    }

    let hashed =
        hash_password(&body.new_password).map_err(|e| AppError::Internal(e.to_string()))?;
    sqlx::query(
        "UPDATE users SET password_hash = $1, sessions_valid_from = date_trunc('second', NOW()), \
         updated_at = NOW() WHERE id = $2",
    )
    .bind(&hashed)
    .bind(uid)
    .execute(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let username: String = user
        .try_get("username")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let role = principal.role.as_deref().unwrap_or("billing_specialist");

    let token = create_token_for_organization(
        &uid.to_string(),
        &username,
        role,
        "full",
        principal.organization_id.as_deref(),
        &state.config.jwt_secret,
        state.config.jwt_expire_minutes,
    )
    .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(Json(serde_json::json!({
        "status": "password_changed",
        "access_token": token,
        "token_type": "bearer",
    })))
}

async fn mfa_user(
    state: &AppState,
    principal: &Principal,
) -> Result<sqlx::postgres::PgRow, AppError> {
    if principal.scope.as_deref() != Some("mfa") {
        return Err(AppError::Unauthorized);
    }
    let uid = principal.user_id.as_ref().ok_or(AppError::Unauthorized)?;
    let uid = Uuid::parse_str(uid).map_err(|_| AppError::Unauthorized)?;

    sqlx::query(
        "SELECT u.id, u.username, om.role, u.totp_required, u.totp_secret, \
                u.totp_confirmed_at, u.totp_last_used_step \
         FROM users u JOIN organization_memberships om ON om.user_id = u.id \
         JOIN organizations o ON o.id = om.organization_id \
          WHERE u.id = $1 AND u.is_active = TRUE AND o.is_active = TRUE \
            AND om.organization_id = $2::uuid \
          ORDER BY om.created_at ASC LIMIT 1",
    )
    .bind(uid)
    .bind(
        principal
            .organization_id
            .as_deref()
            .ok_or(AppError::Unauthorized)?,
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::Unauthorized)
}

pub async fn totp_enroll(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<serde_json::Value>, AppError> {
    let user = mfa_user(&state, &principal).await?;

    let totp_confirmed_at: Option<DateTime<Utc>> = user
        .try_get("totp_confirmed_at")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if totp_confirmed_at.is_some() {
        return Err(AppError::Conflict(
            "This account is already enrolled. Ask an administrator to reset it.".into(),
        ));
    }

    let user_id: Uuid = user
        .try_get("id")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let username: String = user
        .try_get("username")
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let secret = totp::generate_secret();
    let encrypted =
        totp::fernet_encrypt(&state.config.totp_fernet_key, &secret).map_err(AppError::Internal)?;

    sqlx::query("UPDATE users SET totp_secret = $1, totp_last_used_step = NULL WHERE id = $2")
        .bind(&encrypted)
        .bind(user_id)
        .execute(&state.pool)
        .await
        .map_err(AppError::Db)?;

    let uri = totp::otpauth_uri(&state.config.totp_issuer, &username, &secret);
    let qr = totp::qr_svg(&uri);

    Ok(Json(serde_json::json!({
        "secret": secret,
        "otpauth_uri": uri,
        "qr_svg": qr,
        "issuer": state.config.totp_issuer,
    })))
}

async fn complete_totp(
    state: &AppState,
    principal: &Principal,
    code: &str,
    confirming: bool,
) -> Result<Json<serde_json::Value>, AppError> {
    let user = mfa_user(state, principal).await?;

    let user_id: Uuid = user
        .try_get("id")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let username: String = user
        .try_get("username")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let role: String = user
        .try_get("role")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let totp_secret: Option<String> = user
        .try_get("totp_secret")
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let failures: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM audit_log
         WHERE action = 'totp_failed'
           AND resource_id = $1::uuid
           AND created_at > NOW() - INTERVAL '10 minutes'
           AND created_at > COALESCE((
                 SELECT MAX(created_at) FROM audit_log
                  WHERE action IN ('login', 'totp_enrolled') AND resource_id = $1::uuid
               ), 'epoch'::timestamptz)",
    )
    .bind(user_id)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    if failures.0 >= TOTP_FAILURE_LIMIT {
        return Err(AppError::RateLimited {
            retry_after: (TOTP_WINDOW_MINUTES * 60) as u64,
        });
    }

    let secret_enc = totp_secret
        .ok_or_else(|| AppError::Conflict("No authenticator is set up for this account".into()))?;
    let secret =
        totp::fernet_decrypt(&state.config.totp_fernet_key, &secret_enc).ok_or_else(|| {
            AppError::Conflict(
                "The stored authenticator could not be read. Ask an administrator to reset it."
                    .into(),
            )
        })?;

    let now = Utc::now().timestamp() as u64;
    let step = match totp::matching_totp_step(&secret, code, now) {
        Some(step) => step,
        None => {
            let _ = denial_audit::record(
                &state.pool,
                "totp_failed",
                "user",
                Some(&user_id.to_string()),
                Some(&user_id.to_string()),
                &serde_json::json!({
                    "username": username,
                    "stage": if confirming { "enrollment" } else { "login" },
                }),
                None,
                None,
                principal.organization_id.as_deref(),
            )
            .await;
            return Err(AppError::Unauthorized);
        }
    };

    let updated = sqlx::query(
        "UPDATE users SET totp_last_used_step = $1, \
         totp_confirmed_at = COALESCE(totp_confirmed_at, NOW()), \
         last_login = NOW() WHERE id = $2 AND (totp_last_used_step IS NULL OR totp_last_used_step < $1)",
    )
    .bind(step)
    .bind(user_id)
    .execute(&state.pool)
    .await
    .map_err(AppError::Db)?;
    if updated.rows_affected() != 1 {
        return Err(AppError::Unauthorized);
    }

    let action = if confirming { "totp_enrolled" } else { "login" };
    let _ = denial_audit::record(
        &state.pool,
        action,
        "user",
        Some(&user_id.to_string()),
        Some(&user_id.to_string()),
        &serde_json::json!({
            "username": username,
            "role": role,
            "second_factor": "totp",
        }),
        None,
        None,
        principal.organization_id.as_deref(),
    )
    .await;

    let full = sqlx::query(
        "SELECT id, username, email, full_name, role, is_active FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let email: String = full
        .try_get("email")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let full_name: Option<String> = full
        .try_get("full_name")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let is_active: bool = full
        .try_get("is_active")
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let token = create_token_for_organization(
        &user_id.to_string(),
        &username,
        &role,
        "full",
        principal.organization_id.as_deref(),
        &state.config.jwt_secret,
        state.config.jwt_expire_minutes,
    )
    .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(Json(serde_json::json!({
        "access_token": token,
        "token_type": "bearer",
        "user": {
            "id": user_id.to_string(),
            "username": username,
            "email": email,
            "full_name": full_name,
            "role": role,
            "is_active": is_active,
        },
    })))
}

pub async fn totp_confirm(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(body): Json<TotpCode>,
) -> Result<Json<serde_json::Value>, AppError> {
    complete_totp(&state, &principal, &body.code, true).await
}

pub async fn login_totp(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(body): Json<TotpCode>,
) -> Result<Json<serde_json::Value>, AppError> {
    complete_totp(&state, &principal, &body.code, false).await
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/me", get(me))
        .route("/register", post(register))
        .route("/change-password", post(change_password))
        .route("/totp/enroll", post(totp_enroll))
        .route("/totp/confirm", post(totp_confirm))
        .route("/login/totp", post(login_totp))
}
