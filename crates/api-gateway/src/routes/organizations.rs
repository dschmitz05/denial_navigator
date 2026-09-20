//! Organization (tenant) provisioning.
//!
//! Nothing else in this app can create a membership in an organization other
//! than the caller's own (`auth::register` always targets
//! `principal.organization_id`), so before this route existed, the only
//! organization that could ever exist was the one seeded by `init.sql`.
//! system_admin/security_admin only, and not scoped to the caller's own
//! organization - creating a new tenant is a platform-level action.

use axum::extract::State;
use axum::routing::get;
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use denial_auth::auth::hash_password;
use denial_auth::password::check_new_password;
use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use serde::Deserialize;
use sqlx::Row;
use uuid::Uuid;

use crate::state::AppState;

#[derive(Deserialize)]
pub struct OrganizationCreate {
    pub slug: String,
    pub name: String,
    pub admin_username: String,
    pub admin_email: String,
    pub admin_password: String,
    pub admin_full_name: Option<String>,
}

fn normalize_slug(raw: &str) -> Result<String, AppError> {
    let slug = raw.trim().to_lowercase();
    let valid = !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if valid {
        Ok(slug)
    } else {
        Err(AppError::BadRequest(
            "slug must be non-empty, lowercase letters, digits, and hyphens only".into(),
        ))
    }
}

pub async fn list_organizations(
    State(state): State<AppState>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let rows = sqlx::query(
        "SELECT o.id, o.slug, o.name, o.is_active, o.created_at, \
                COUNT(om.user_id) AS member_count \
         FROM organizations o \
         LEFT JOIN organization_memberships om ON om.organization_id = o.id \
         GROUP BY o.id ORDER BY o.created_at",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let values = rows
        .iter()
        .map(|row| {
            let id: Uuid = row.get("id");
            let created_at: DateTime<Utc> = row.get("created_at");
            serde_json::json!({
                "id": id.to_string(),
                "slug": row.get::<String, _>("slug"),
                "name": row.get::<String, _>("name"),
                "is_active": row.get::<bool, _>("is_active"),
                "created_at": created_at.to_rfc3339(),
                "member_count": row.get::<i64, _>("member_count"),
            })
        })
        .collect();

    Ok(Json(values))
}

/// Create a new organization along with its first system_admin user, so the
/// organization is reachable the moment it exists - an organization created
/// with no members would be permanently locked out.
pub async fn create_organization(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(body): Json<OrganizationCreate>,
) -> Result<(axum::http::StatusCode, Json<serde_json::Value>), AppError> {
    let slug = normalize_slug(&body.slug)?;
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("name must not be empty".into()));
    }
    check_new_password(&body.admin_username, &body.admin_password).map_err(AppError::BadRequest)?;

    let existing_slug = sqlx::query("SELECT id FROM organizations WHERE slug = $1")
        .bind(&slug)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::Db)?;
    if existing_slug.is_some() {
        return Err(AppError::Conflict(
            "An organization with this slug already exists".into(),
        ));
    }
    let existing_user = sqlx::query("SELECT id FROM users WHERE username = $1 OR email = $2")
        .bind(&body.admin_username)
        .bind(&body.admin_email)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::Db)?;
    if existing_user.is_some() {
        return Err(AppError::Conflict(
            "Username or email already exists".into(),
        ));
    }

    let hashed =
        hash_password(&body.admin_password).map_err(|e| AppError::Internal(e.to_string()))?;

    let row = sqlx::query(
        "WITH org AS ( \
             INSERT INTO organizations (slug, name) VALUES ($1, $2) RETURNING id, slug, name \
         ), admin_user AS ( \
             INSERT INTO users (username, email, password_hash, full_name, role, is_active, \
                                 must_change_password) \
             VALUES ($3, $4, $5, $6, 'system_admin', TRUE, TRUE) \
             RETURNING id \
         ), membership AS ( \
             INSERT INTO organization_memberships (organization_id, user_id, role) \
             SELECT org.id, admin_user.id, 'system_admin' FROM org, admin_user \
         ) \
         SELECT org.id, org.slug, org.name FROM org",
    )
    .bind(&slug)
    .bind(name)
    .bind(&body.admin_username)
    .bind(&body.admin_email)
    .bind(&hashed)
    .bind(&body.admin_full_name)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let id: Uuid = row
        .try_get("id")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let returned_slug: String = row
        .try_get("slug")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let returned_name: String = row
        .try_get("name")
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let org_id_str = id.to_string();
    let _ = denial_audit::record(
        &state.pool,
        "organization_created",
        "organization",
        Some(org_id_str.as_str()),
        principal.user_id.as_deref(),
        &serde_json::json!({
            "slug": returned_slug,
            "name": returned_name,
            "admin_username": body.admin_username,
        }),
        principal.ip.as_deref(),
        None,
        Some(org_id_str.as_str()),
    )
    .await;

    Ok((
        axum::http::StatusCode::CREATED,
        Json(serde_json::json!({
            "id": org_id_str,
            "slug": returned_slug,
            "name": returned_name,
        })),
    ))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/", get(list_organizations).post(create_organization))
}
