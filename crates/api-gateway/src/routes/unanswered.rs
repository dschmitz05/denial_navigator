//! Claims submitted on an 837 that the payer has not answered (FB-09).
//!
//! A claim with no remittance never becomes a denial, so without this queue it
//! is simply lost to timely filing. A claim is due for follow-up once it has
//! waited longer than the payer normally takes to answer (the `payer_response`
//! deadline rule, 30 days if none), and it leaves the queue when its first 835
//! arrives. The timely-filing rule, when there is one, says how long is left.

use axum::extract::{Path, State};
use axum::{Extension, Json};
use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use serde::Deserialize;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use super::scope::organization_id;
use crate::routes::appeals::record_audit;
use crate::state::AppState;

/// Days to wait for an answer when the payer has no `payer_response` rule.
const DEFAULT_RESPONSE_DAYS: i32 = 30;

const FOLLOWUP_ACTIONS: &[&str] = &["status_inquiry", "resubmitted", "payer_contact"];

pub async fn list(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<Value>>, AppError> {
    let rows = sqlx::query(
        "SELECT c.id, c.claim_number, c.payer_name, c.total_charge::float8 AS total_charge, \
                c.service_from, c.submitted_at, \
                (CURRENT_DATE - c.submitted_at::date) AS days_outstanding, \
                COALESCE(resp.days, $2) AS response_days, \
                CASE WHEN tf.days IS NOT NULL AND c.service_from IS NOT NULL \
                     THEN c.service_from + tf.days END AS timely_filing_due, \
                fu.action AS last_action, fu.note AS last_note, fu.created_at AS last_action_at \
         FROM claims c \
         LEFT JOIN LATERAL ( \
             SELECT days FROM payer_deadline_rules r \
             WHERE r.organization_id = c.organization_id AND r.deadline_type = 'payer_response' \
               AND (lower(r.payer_name) = lower(COALESCE(c.payer_name, '')) OR r.payer_name = '*') \
             ORDER BY (r.payer_name = '*') LIMIT 1) resp ON TRUE \
         LEFT JOIN LATERAL ( \
             SELECT days FROM payer_deadline_rules r \
             WHERE r.organization_id = c.organization_id AND r.deadline_type = 'timely_filing' \
               AND (lower(r.payer_name) = lower(COALESCE(c.payer_name, '')) OR r.payer_name = '*') \
             ORDER BY (r.payer_name = '*') LIMIT 1) tf ON TRUE \
         LEFT JOIN LATERAL ( \
             SELECT action, note, created_at FROM claim_followups f \
             WHERE f.claim_id = c.id ORDER BY created_at DESC LIMIT 1) fu ON TRUE \
         WHERE c.organization_id = $1 \
           AND c.submitted_at IS NOT NULL AND c.remittance_received_at IS NULL \
           AND c.submitted_at::date + COALESCE(resp.days, $2) <= CURRENT_DATE \
         ORDER BY timely_filing_due NULLS LAST, days_outstanding DESC \
         LIMIT 500",
    )
    .bind(organization_id(&principal)?)
    .bind(DEFAULT_RESPONSE_DAYS)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    let today = chrono::Local::now().date_naive();
    Ok(Json(
        rows.iter()
            .map(|r| {
                let timely_filing_due: Option<chrono::NaiveDate> = r.get("timely_filing_due");
                serde_json::json!({
                    "id": r.get::<Uuid, _>("id").to_string(),
                    "claim_number": r.get::<String, _>("claim_number"),
                    "payer_name": r.get::<Option<String>, _>("payer_name"),
                    "total_charge": r.get::<Option<f64>, _>("total_charge"),
                    "service_from": r.get::<Option<chrono::NaiveDate>, _>("service_from").map(|d| d.to_string()),
                    "submitted_at": r.get::<chrono::DateTime<chrono::Utc>, _>("submitted_at").to_rfc3339(),
                    "days_outstanding": r.get::<i32, _>("days_outstanding"),
                    "response_days": r.get::<i32, _>("response_days"),
                    "timely_filing_due": timely_filing_due.map(|d| d.to_string()),
                    "days_to_timely_filing": timely_filing_due.map(|d| (d - today).num_days()),
                    "last_action": r.get::<Option<String>, _>("last_action"),
                    "last_note": r.get::<Option<String>, _>("last_note"),
                    "last_action_at": r.get::<Option<chrono::DateTime<chrono::Utc>>, _>("last_action_at").map(|t| t.to_rfc3339()),
                })
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct FollowupInput {
    pub action: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// Records what was done about an unanswered claim.
pub async fn record_followup(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(claim_id): Path<Uuid>,
    Json(input): Json<FollowupInput>,
) -> Result<Json<Value>, AppError> {
    if !FOLLOWUP_ACTIONS.contains(&input.action.as_str()) {
        return Err(AppError::Unprocessable(
            "action must be status_inquiry, resubmitted or payer_contact".into(),
        ));
    }
    let organization_id = organization_id(&principal)?;
    let user = principal
        .user_id
        .as_deref()
        .and_then(|u| Uuid::parse_str(u).ok());
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO claim_followups (organization_id, claim_id, action, note, user_id) \
         SELECT $1, c.id, $3, $4, $5 FROM claims c WHERE c.id = $2 AND c.organization_id = $1 \
         RETURNING id",
    )
    .bind(organization_id)
    .bind(claim_id)
    .bind(&input.action)
    .bind(&input.note)
    .bind(user)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;
    record_audit(
        &state.pool,
        &principal,
        "claim_followup_recorded",
        "claim",
        Some(&claim_id.to_string()),
        &serde_json::json!({ "username": principal.username, "action": input.action }),
    )
    .await;
    Ok(Json(
        serde_json::json!({ "id": id.to_string(), "action": input.action }),
    ))
}
