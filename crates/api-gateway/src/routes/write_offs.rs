//! Write-off approval (FB-03).
//!
//! Writing off a denial gives up money the practice is owed. At or above the
//! organization's `write_off_approval_threshold` the write-off is held as a
//! pending request until a revenue-cycle manager or system administrator who
//! did not ask for it approves it. Both ways of writing off (closing a
//! `write_off` worklist item, or setting a denial to `written_off`) go through
//! [`gate`].

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use serde::Deserialize;
use sqlx::Row;
use uuid::Uuid;

use super::scope::organization_id;
use crate::routes::appeals::{record_audit, refresh_claim_status};
use crate::state::AppState;

fn user_id(principal: &Principal) -> Option<Uuid> {
    principal
        .user_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
}

/// Whether a write-off may proceed now.
pub enum Gate {
    /// Below the threshold: write it off.
    Allowed,
    /// Held for approval; nothing was written off.
    Pending {
        request_id: Uuid,
        amount: f64,
        threshold: f64,
    },
}

impl Gate {
    /// The response for a held write-off.
    pub fn pending_response(request_id: Uuid, amount: f64, threshold: f64) -> serde_json::Value {
        serde_json::json!({
            "status": "pending_approval",
            "write_off_request_id": request_id.to_string(),
            "amount": amount,
            "threshold": threshold,
            "detail": format!(
                "Write-offs of ${threshold:.2} or more need approval from a revenue cycle \
                 manager or administrator; this one (${amount:.2}) is waiting for it."
            ),
        })
    }
}

/// Decides whether the caller may write off `denial_id` now, and if not,
/// records (or finds) the pending request. The amount is the denied amount,
/// falling back to the unpaid charge.
pub async fn gate(
    pool: &sqlx::PgPool,
    principal: &Principal,
    denial_id: Uuid,
    appeal_id: Option<Uuid>,
    reason: Option<&str>,
) -> Result<Gate, AppError> {
    let organization_id = organization_id(principal)?;
    let row = sqlx::query(
        "SELECT (CASE WHEN d.adjustment_amount > 0 THEN d.adjustment_amount \
                      ELSE GREATEST(d.charge_amount - d.payment_amount, 0) END)::float8 AS amount, \
                o.write_off_approval_threshold::float8 AS threshold \
         FROM denials d JOIN claims c ON c.id = d.claim_id \
         JOIN organizations o ON o.id = c.organization_id \
         WHERE d.id = $1 AND c.organization_id = $2",
    )
    .bind(denial_id)
    .bind(organization_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;
    let amount: f64 = row.get("amount");
    let threshold: f64 = row.get("threshold");
    if amount < threshold {
        return Ok(Gate::Allowed);
    }

    // One pending request per denial; asking again returns the same one.
    let inserted = sqlx::query(
        "INSERT INTO write_off_requests \
             (organization_id, denial_id, appeal_id, amount, reason, requested_by) \
         VALUES ($1, $2, $3, $4::numeric, $5, $6) \
         ON CONFLICT (denial_id) WHERE status = 'pending' DO NOTHING \
         RETURNING id",
    )
    .bind(organization_id)
    .bind(denial_id)
    .bind(appeal_id)
    .bind(amount)
    .bind(reason)
    .bind(user_id(principal))
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)?;
    let request_id: Uuid = match inserted {
        Some(row) => {
            let id: Uuid = row.get("id");
            record_audit(
                pool,
                principal,
                "write_off_requested",
                "write_off_request",
                Some(&id.to_string()),
                &serde_json::json!({
                    "username": principal.username,
                    "denial_id": denial_id.to_string(),
                    "amount": amount,
                    "threshold": threshold,
                }),
            )
            .await;
            id
        }
        None => sqlx::query_scalar(
            "SELECT id FROM write_off_requests WHERE denial_id = $1 AND status = 'pending'",
        )
        .bind(denial_id)
        .fetch_one(pool)
        .await
        .map_err(AppError::Db)?,
    };
    Ok(Gate::Pending {
        request_id,
        amount,
        threshold,
    })
}

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default = "default_status")]
    pub status: String,
}

fn default_status() -> String {
    "pending".into()
}

pub async fn list(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let organization_id = organization_id(&principal)?;
    let rows = sqlx::query(
        "SELECT w.id, w.denial_id, w.appeal_id, w.amount::float8 AS amount, w.reason, w.status, \
                w.requested_at, w.decided_at, w.decision_note, \
                ru.username AS requested_by, du.username AS decided_by, \
                c.claim_number, c.payer_name, d.cagc, d.carc_code, d.cpt_code \
         FROM write_off_requests w \
         JOIN denials d ON d.id = w.denial_id JOIN claims c ON c.id = d.claim_id \
         LEFT JOIN users ru ON ru.id = w.requested_by \
         LEFT JOIN users du ON du.id = w.decided_by \
         WHERE w.organization_id = $1 AND w.status = $2 \
         ORDER BY w.requested_at DESC LIMIT 500",
    )
    .bind(organization_id)
    .bind(&query.status)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    Ok(Json(
        rows.iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.get::<Uuid, _>("id").to_string(),
                    "denial_id": r.get::<Uuid, _>("denial_id").to_string(),
                    "appeal_id": r.get::<Option<Uuid>, _>("appeal_id").map(|id| id.to_string()),
                    "amount": r.get::<f64, _>("amount"),
                    "reason": r.get::<Option<String>, _>("reason"),
                    "status": r.get::<String, _>("status"),
                    "requested_by": r.get::<Option<String>, _>("requested_by"),
                    "requested_at": r.get::<chrono::DateTime<chrono::Utc>, _>("requested_at").to_rfc3339(),
                    "decided_by": r.get::<Option<String>, _>("decided_by"),
                    "decided_at": r.get::<Option<chrono::DateTime<chrono::Utc>>, _>("decided_at").map(|t| t.to_rfc3339()),
                    "decision_note": r.get::<Option<String>, _>("decision_note"),
                    "claim_number": r.get::<String, _>("claim_number"),
                    "payer_name": r.get::<Option<String>, _>("payer_name"),
                    "cagc": r.get::<String, _>("cagc"),
                    "carc_code": r.get::<Option<String>, _>("carc_code"),
                    "cpt_code": r.get::<Option<String>, _>("cpt_code"),
                })
            })
            .collect(),
    ))
}

#[derive(Deserialize, Default)]
pub struct Decision {
    #[serde(default)]
    pub note: Option<String>,
}

/// Loads a pending request for a decision, refusing the person who asked.
async fn pending_for_decision(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    principal: &Principal,
    request_id: Uuid,
) -> Result<(Uuid, Option<Uuid>), AppError> {
    let organization_id = organization_id(principal)?;
    let row = sqlx::query(
        "SELECT denial_id, appeal_id, requested_by FROM write_off_requests \
         WHERE id = $1 AND organization_id = $2 AND status = 'pending' FOR UPDATE",
    )
    .bind(request_id)
    .bind(organization_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;
    let requested_by: Option<Uuid> = row.get("requested_by");
    if requested_by.is_some() && requested_by == user_id(principal) {
        return Err(AppError::Forbidden);
    }
    Ok((row.get("denial_id"), row.get("appeal_id")))
}

pub async fn approve(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(request_id): Path<Uuid>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    // The note is optional, and so is the body: an empty POST approves.
    let note = if body.is_empty() {
        None
    } else {
        serde_json::from_slice::<Decision>(&body)
            .map_err(|e| AppError::BadRequest(format!("Invalid body: {e}")))?
            .note
    };
    let mut tx = state.pool.begin().await.map_err(AppError::Db)?;
    let (denial_id, appeal_id) = pending_for_decision(&mut tx, &principal, request_id).await?;

    sqlx::query(
        "UPDATE write_off_requests SET status = 'approved', decided_by = $2, decided_at = NOW(), \
                decision_note = $3 WHERE id = $1",
    )
    .bind(request_id)
    .bind(user_id(&principal))
    .bind(&note)
    .execute(&mut *tx)
    .await
    .map_err(AppError::Db)?;
    let claim_id: Uuid = sqlx::query_scalar(
        "UPDATE denials SET status = 'written_off', resolution_source = 'user', \
                resolved_at = NOW(), updated_at = NOW() \
         WHERE id = $1 RETURNING claim_id",
    )
    .bind(denial_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::Db)?;
    if let Some(appeal_id) = appeal_id {
        sqlx::query(
            "UPDATE appeals_queue SET outcome_status = 'resolved', updated_at = NOW() WHERE id = $1",
        )
        .bind(appeal_id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Db)?;
    }
    tx.commit().await.map_err(AppError::Db)?;
    refresh_claim_status(&state.pool, &claim_id).await?;

    record_audit(
        &state.pool,
        &principal,
        "write_off_approved",
        "write_off_request",
        Some(&request_id.to_string()),
        &serde_json::json!({
            "username": principal.username,
            "denial_id": denial_id.to_string(),
            "note": note,
        }),
    )
    .await;
    Ok(Json(serde_json::json!({
        "status": "approved",
        "write_off_request_id": request_id.to_string(),
        "denial_id": denial_id.to_string(),
    })))
}

pub async fn reject(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(request_id): Path<Uuid>,
    Json(body): Json<Decision>,
) -> Result<Json<serde_json::Value>, AppError> {
    let note = body
        .note
        .filter(|n| !n.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("Give a reason for rejecting the write-off".into()))?;
    let mut tx = state.pool.begin().await.map_err(AppError::Db)?;
    let (denial_id, _) = pending_for_decision(&mut tx, &principal, request_id).await?;
    sqlx::query(
        "UPDATE write_off_requests SET status = 'rejected', decided_by = $2, decided_at = NOW(), \
                decision_note = $3 WHERE id = $1",
    )
    .bind(request_id)
    .bind(user_id(&principal))
    .bind(&note)
    .execute(&mut *tx)
    .await
    .map_err(AppError::Db)?;
    tx.commit().await.map_err(AppError::Db)?;

    record_audit(
        &state.pool,
        &principal,
        "write_off_rejected",
        "write_off_request",
        Some(&request_id.to_string()),
        &serde_json::json!({
            "username": principal.username,
            "denial_id": denial_id.to_string(),
            "note": note,
        }),
    )
    .await;
    Ok(Json(serde_json::json!({
        "status": "rejected",
        "write_off_request_id": request_id.to_string(),
        "denial_id": denial_id.to_string(),
    })))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list))
        .route("/{id}/approve", post(approve))
        .route("/{id}/reject", post(reject))
}
