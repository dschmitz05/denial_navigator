//! Admin-configurable system settings.
//!
//! The PHI disclosure level (plan §12.2) controls how much of a claim's
//! identifying reference is included in prompts sent to external LLM providers.
//! It is admin-only: the access-control middleware enforces
//! `settings -> (ADMIN_ONLY, ADMIN_ONLY)` and the handler double-checks. The
//! value is read on every analysis and handed to the RAG engine, which applies
//! it when building the prompt and falls back to its configured default when
//! the value is absent or unparseable.

use axum::extract::State;
use axum::routing::get;
use axum::{Extension, Json, Router};
use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use serde::Deserialize;
use uuid::Uuid;

use crate::state::AppState;

/// The plan §12.2 level names. Kept in sync with
/// `denial_ai::prompt::PhiDisclosureLevel::names()`.
const VALID_LEVELS: &[&str] = &["none", "deidentified", "limited_phi", "full_context"];

/// The plan §12.2 default: withhold the claim reference.
const DEFAULT_LEVEL: &str = "deidentified";

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

/// Read the configured disclosure level, falling back to the default when the
/// table or row is missing (e.g. a database that predates the settings table).
pub async fn current_level(pool: &sqlx::PgPool) -> String {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM system_settings WHERE key = 'phi_disclosure_level'")
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    match row {
        Some((value,)) if VALID_LEVELS.contains(&value.as_str()) => value,
        _ => DEFAULT_LEVEL.to_string(),
    }
}

#[derive(Deserialize)]
pub struct SetLevelRequest {
    pub level: String,
}

pub async fn get_phi_disclosure(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&principal)?;
    let level = current_level(&state.pool).await;
    Ok(Json(serde_json::json!({ "phi_disclosure_level": level })))
}

pub async fn set_phi_disclosure(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<SetLevelRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&principal)?;
    let level = req.level.trim().to_ascii_lowercase();
    if !VALID_LEVELS.contains(&level.as_str()) {
        return Err(AppError::Unprocessable(format!(
            "level must be one of: {}",
            VALID_LEVELS.join(", ")
        )));
    }

    let updated_by: Option<Uuid> = principal
        .user_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok());

    sqlx::query(
        "INSERT INTO system_settings (key, value, updated_by) \
         VALUES ('phi_disclosure_level', $1, $2) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = NOW(), updated_by = EXCLUDED.updated_by",
    )
    .bind(&level)
    .bind(updated_by)
    .execute(&state.pool)
    .await?;

    Ok(Json(serde_json::json!({ "phi_disclosure_level": level })))
}

fn organization_id(principal: &Principal) -> Result<Uuid, AppError> {
    principal
        .organization_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(AppError::Forbidden)
}

#[derive(Deserialize)]
pub struct SetThresholdRequest {
    pub threshold: f64,
}

/// The amount at or above which a write-off in the caller's organization needs
/// a second person's approval (0 = every write-off).
pub async fn get_write_off_approval(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&principal)?;
    let threshold: f64 = sqlx::query_scalar(
        "SELECT write_off_approval_threshold::float8 FROM organizations WHERE id = $1",
    )
    .bind(organization_id(&principal)?)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(serde_json::json!({ "threshold": threshold })))
}

pub async fn set_write_off_approval(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<SetThresholdRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(&principal)?;
    if !req.threshold.is_finite() || req.threshold < 0.0 {
        return Err(AppError::Unprocessable(
            "threshold must be a non-negative amount".into(),
        ));
    }
    let organization_id = organization_id(&principal)?;
    let previous: f64 = sqlx::query_scalar(
        "UPDATE organizations o SET write_off_approval_threshold = ROUND($2::numeric, 2)          FROM organizations old WHERE o.id = $1 AND old.id = o.id          RETURNING old.write_off_approval_threshold::float8",
    )
    .bind(organization_id)
    .bind(req.threshold)
    .fetch_one(&state.pool)
    .await?;
    crate::routes::appeals::record_audit(
        &state.pool,
        &principal,
        "write_off_threshold_changed",
        "organization",
        Some(&organization_id.to_string()),
        &serde_json::json!({
            "username": principal.username,
            "from": previous,
            "to": req.threshold,
        }),
    )
    .await;
    Ok(Json(serde_json::json!({ "threshold": req.threshold })))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/phi-disclosure",
            get(get_phi_disclosure).put(set_phi_disclosure),
        )
        .route(
            "/write-off-approval",
            get(get_write_off_approval).put(set_write_off_approval),
        )
}
