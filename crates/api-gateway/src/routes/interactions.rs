//! Structured payer-interaction log on a denial (FB-16).
//!
//! `payer_contact` has always been a resolution type and a follow-up action,
//! but the call itself — who was reached, what reference number they gave,
//! what they promised and by when — used to live only in a free-text note.
//! An appeal or an escalation later depends on exactly that detail, so it is
//! its own record here instead.

use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::NaiveDate;
use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use serde::Deserialize;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::routes::appeals::record_audit;
use crate::state::AppState;

const CHANNELS: &[&str] = &["phone", "portal", "fax", "mail", "email", "other"];

fn organization_id(principal: &Principal) -> Result<Uuid, AppError> {
    principal
        .organization_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(AppError::Forbidden)
}

/// A denial's payer-interaction history, most recent first.
pub async fn list_interactions(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(denial_id): Path<Uuid>,
) -> Result<Json<Vec<Value>>, AppError> {
    let organization_id = organization_id(&principal)?;
    let rows = sqlx::query(
        "SELECT pi.id, pi.channel, pi.occurred_at, pi.reference_number, pi.representative, \
                pi.summary, pi.follow_up_on, pi.follow_up_completed_at, pi.created_at, \
                u.full_name AS user_name \
         FROM payer_interactions pi \
         JOIN denials d ON d.id = pi.denial_id \
         JOIN claims c ON c.id = d.claim_id \
         LEFT JOIN users u ON u.id = pi.user_id \
         WHERE pi.denial_id = $1 AND c.organization_id = $2 \
         ORDER BY pi.occurred_at DESC",
    )
    .bind(denial_id)
    .bind(organization_id)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    Ok(Json(
        rows.iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.get::<Uuid, _>("id").to_string(),
                    "channel": r.get::<String, _>("channel"),
                    "occurred_at": r.get::<chrono::DateTime<chrono::Utc>, _>("occurred_at").to_rfc3339(),
                    "reference_number": r.get::<Option<String>, _>("reference_number"),
                    "representative": r.get::<Option<String>, _>("representative"),
                    "summary": r.get::<String, _>("summary"),
                    "follow_up_on": r.get::<Option<NaiveDate>, _>("follow_up_on").map(|d| d.to_string()),
                    "follow_up_completed_at": r.get::<Option<chrono::DateTime<chrono::Utc>>, _>("follow_up_completed_at").map(|t| t.to_rfc3339()),
                    "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
                    "user_name": r.get::<Option<String>, _>("user_name"),
                })
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct InteractionInput {
    pub channel: String,
    pub summary: String,
    #[serde(default)]
    pub occurred_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub reference_number: Option<String>,
    #[serde(default)]
    pub representative: Option<String>,
    #[serde(default)]
    pub follow_up_on: Option<NaiveDate>,
}

pub async fn record_interaction(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(denial_id): Path<Uuid>,
    Json(input): Json<InteractionInput>,
) -> Result<Json<Value>, AppError> {
    if !CHANNELS.contains(&input.channel.as_str()) {
        return Err(AppError::Unprocessable(format!(
            "channel must be one of: {}",
            CHANNELS.join(", ")
        )));
    }
    if input.summary.trim().is_empty() {
        return Err(AppError::BadRequest("summary is required".into()));
    }
    let organization_id = organization_id(&principal)?;
    let user_id = principal
        .user_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok());

    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO payer_interactions \
            (organization_id, denial_id, channel, occurred_at, reference_number, \
             representative, summary, follow_up_on, user_id) \
         SELECT $1, d.id, $3, COALESCE($4, NOW()), $5, $6, $7, $8, $9 \
         FROM denials d JOIN claims c ON c.id = d.claim_id \
         WHERE d.id = $2 AND c.organization_id = $1 \
         RETURNING id",
    )
    .bind(organization_id)
    .bind(denial_id)
    .bind(&input.channel)
    .bind(input.occurred_at)
    .bind(&input.reference_number)
    .bind(&input.representative)
    .bind(input.summary.trim())
    .bind(input.follow_up_on)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;

    record_audit(
        &state.pool,
        &principal,
        "payer_interaction_recorded",
        "denial",
        Some(&denial_id.to_string()),
        &serde_json::json!({
            "username": principal.username,
            "channel": input.channel,
            "follow_up_on": input.follow_up_on,
        }),
    )
    .await;
    Ok(Json(serde_json::json!({ "id": id.to_string() })))
}

/// Marks a promised follow-up done, so it stops showing as due in the
/// digest. Does not require it to have been the one who logged the call.
pub async fn complete_follow_up(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((denial_id, interaction_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let updated = sqlx::query(
        "UPDATE payer_interactions SET follow_up_completed_at = NOW() \
         WHERE id = $1 AND denial_id = $2 AND organization_id = $3 AND follow_up_on IS NOT NULL \
         RETURNING id",
    )
    .bind(interaction_id)
    .bind(denial_id)
    .bind(organization_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?;
    if updated.is_none() {
        return Err(AppError::NotFound);
    }
    Ok(Json(
        serde_json::json!({ "id": interaction_id.to_string(), "follow_up_completed": true }),
    ))
}
