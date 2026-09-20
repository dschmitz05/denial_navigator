//! Overpayments found in remittances, tracked to a refund deadline (FB-08).
//!
//! For many payers an identified overpayment must be reported and returned
//! within a fixed window (60 days from identification for Medicare), so this is
//! a compliance record, not only finance. Two kinds are found at ingestion:
//! a service line paid above its allowed amount, and a claim paid again under a
//! different payer claim control number with no reversal of the first payment.
//! A PLB recoupment (WO) naming the claim later marks it recouped.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
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

/// CLP02 values meaning the payer processed and paid the claim.
const PAID_STATUSES: &[&str] = &["1", "2", "3", "19", "20", "21"];

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn number(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

fn cents(amount: f64) -> f64 {
    (amount * 100.0).round() / 100.0
}

/// What was already recorded for a claim before this remittance.
#[derive(Debug, Clone)]
pub struct PriorPayment {
    pub total_paid: f64,
    pub control_number: Option<String>,
    pub reversed: bool,
}

/// An overpayment found in a remittance.
#[derive(Debug, PartialEq)]
pub struct Found {
    pub claim_number: String,
    pub kind: &'static str,
    pub line_number: Option<i64>,
    pub amount: f64,
    pub detail: String,
}

/// Loads what each claim in the file had been paid before it; call before the
/// claims are upserted.
pub async fn prior_payments(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    organization_id: Uuid,
    claims: &[Value],
) -> Result<HashMap<String, PriorPayment>, AppError> {
    let numbers: Vec<&str> = claims.iter().filter_map(|c| text(c, "claim_id")).collect();
    if numbers.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query(
        "SELECT claim_number, total_paid::float8 AS total_paid, \
                raw_835_data->>'payer_claim_control_number' AS control_number, \
                reversed_at IS NOT NULL AS reversed \
         FROM claims WHERE organization_id = $1 AND claim_number = ANY($2::text[])",
    )
    .bind(organization_id)
    .bind(&numbers)
    .fetch_all(&mut **tx)
    .await
    .map_err(AppError::Db)?;
    Ok(rows
        .iter()
        .map(|r| {
            (
                r.get::<String, _>("claim_number"),
                PriorPayment {
                    total_paid: r
                        .try_get::<Option<f64>, _>("total_paid")
                        .ok()
                        .flatten()
                        .unwrap_or(0.0),
                    control_number: r.get("control_number"),
                    reversed: r.get("reversed"),
                },
            )
        })
        .collect())
}

/// Overpayments in the incoming claims (reversal loops already removed).
pub fn find(
    claims: &[Value],
    prior: &HashMap<String, PriorPayment>,
    reversed_in_file: &[String],
) -> Vec<Found> {
    let mut found = Vec::new();
    for claim in claims {
        let Some(claim_number) = text(claim, "claim_id") else {
            continue;
        };
        for detail in claim
            .get("service_lines")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let line = detail.get("service_line").unwrap_or(&Value::Null);
            let paid = number(line, "paid_amount");
            let Some(allowed) = line.get("allowed_amount").and_then(Value::as_f64) else {
                continue;
            };
            let over = cents(paid - allowed);
            if over > 0.0 {
                found.push(Found {
                    claim_number: claim_number.to_string(),
                    kind: "paid_above_allowed",
                    line_number: line.get("line_number").and_then(Value::as_i64),
                    amount: over,
                    detail: format!(
                        "Line paid {paid:.2} against an allowed amount of {allowed:.2}"
                    ),
                });
            }
        }

        let paid_now = number(claim, "total_paid");
        let is_paid = text(claim, "claim_status_code").is_some_and(|s| PAID_STATUSES.contains(&s));
        let Some(before) = prior.get(claim_number) else {
            continue;
        };
        let control_now = text(claim, "payer_claim_control_number");
        // A different control number is a separate adjudication; the same one
        // is the same payment re-sent, which is not an overpayment.
        let different_payment = matches!(
            (before.control_number.as_deref(), control_now),
            (Some(a), Some(b)) if a != b
        );
        if is_paid
            && paid_now > 0.0
            && before.total_paid > 0.0
            && different_payment
            && !before.reversed
            && !reversed_in_file.iter().any(|r| r == claim_number)
        {
            found.push(Found {
                claim_number: claim_number.to_string(),
                kind: "duplicate_payment",
                line_number: None,
                amount: cents(before.total_paid.min(paid_now)),
                detail: format!(
                    "Paid {paid_now:.2} under control number {} after {:.2} under {}, with no reversal",
                    control_now.unwrap_or_default(),
                    before.total_paid,
                    before.control_number.as_deref().unwrap_or_default()
                ),
            });
        }
    }
    found
}

/// Records found overpayments with their refund deadline; returns how many
/// were new.
pub async fn record(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    organization_id: Uuid,
    ingestion_id: Uuid,
    found: &[Found],
) -> Result<u64, AppError> {
    let mut stored = 0;
    for f in found {
        let result = sqlx::query(
            "INSERT INTO overpayments \
                 (organization_id, claim_id, ingestion_id, kind, service_line_number, amount, \
                  payer_name, detail, due_date) \
             SELECT $1, c.id, $2, $3, $4, $5::numeric, c.payer_name, $6, \
                    CURRENT_DATE + o.overpayment_refund_days \
             FROM claims c JOIN organizations o ON o.id = c.organization_id \
             WHERE c.organization_id = $1 AND c.claim_number = $7 \
             ON CONFLICT DO NOTHING",
        )
        .bind(organization_id)
        .bind(ingestion_id)
        .bind(f.kind)
        .bind(f.line_number)
        .bind(f.amount)
        .bind(&f.detail)
        .bind(&f.claim_number)
        .execute(&mut **tx)
        .await
        .map_err(AppError::Db)?;
        stored += result.rows_affected();
    }
    Ok(stored)
}

/// A PLB recoupment (WO) naming a claim: the payer took the money back.
pub async fn mark_recouped(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    organization_id: Uuid,
    claim_number: &str,
) -> Result<u64, AppError> {
    let result = sqlx::query(
        "UPDATE overpayments o SET status = 'recouped', resolved_at = NOW(), \
                resolution_note = 'Recovered by the payer (PLB WO)' \
         FROM claims c \
         WHERE o.claim_id = c.id AND c.organization_id = $1 AND c.claim_number = $2 \
           AND o.status = 'identified'",
    )
    .bind(organization_id)
    .bind(claim_number)
    .execute(&mut **tx)
    .await
    .map_err(AppError::Db)?;
    Ok(result.rows_affected())
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub status: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<Value>>, AppError> {
    let rows = sqlx::query(
        "SELECT o.id, c.claim_number, o.kind, o.service_line_number, o.amount::float8 AS amount, \
                o.payer_name, o.detail, o.identified_at, o.due_date, o.status, o.resolved_at, \
                o.resolution_note, (o.status = 'identified' AND o.due_date < CURRENT_DATE) AS overdue \
         FROM overpayments o JOIN claims c ON c.id = o.claim_id \
         WHERE o.organization_id = $1 AND ($2::text IS NULL OR o.status = $2) \
         ORDER BY (o.status = 'identified') DESC, o.due_date ASC LIMIT 500",
    )
    .bind(organization_id(&principal)?)
    .bind(query.status.as_deref())
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    Ok(Json(
        rows.iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.get::<Uuid, _>("id").to_string(),
                    "claim_number": r.get::<String, _>("claim_number"),
                    "kind": r.get::<String, _>("kind"),
                    "service_line_number": r.get::<Option<i32>, _>("service_line_number"),
                    "amount": r.get::<f64, _>("amount"),
                    "payer_name": r.get::<Option<String>, _>("payer_name"),
                    "detail": r.get::<Option<String>, _>("detail"),
                    "identified_at": r.get::<chrono::DateTime<chrono::Utc>, _>("identified_at").to_rfc3339(),
                    "due_date": r.get::<chrono::NaiveDate, _>("due_date").to_string(),
                    "status": r.get::<String, _>("status"),
                    "overdue": r.get::<bool, _>("overdue"),
                    "resolved_at": r.get::<Option<chrono::DateTime<chrono::Utc>>, _>("resolved_at").map(|t| t.to_rfc3339()),
                    "resolution_note": r.get::<Option<String>, _>("resolution_note"),
                })
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct StatusUpdate {
    pub status: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// Records what happened to an overpayment: refunded, recouped by the payer,
/// disputed, or back to identified.
pub async fn update_status(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<Uuid>,
    Json(body): Json<StatusUpdate>,
) -> Result<Json<Value>, AppError> {
    if !["identified", "refunded", "recouped", "disputed"].contains(&body.status.as_str()) {
        return Err(AppError::Unprocessable(
            "status must be identified, refunded, recouped or disputed".into(),
        ));
    }
    let resolved = body.status != "identified";
    let user = principal
        .user_id
        .as_deref()
        .and_then(|u| Uuid::parse_str(u).ok());
    let updated = sqlx::query(
        "UPDATE overpayments SET status = $3, resolution_note = $4, \
                resolved_at = CASE WHEN $5 THEN NOW() END, \
                resolved_by = CASE WHEN $5 THEN $6 END \
         WHERE id = $1 AND organization_id = $2 RETURNING id",
    )
    .bind(id)
    .bind(organization_id(&principal)?)
    .bind(&body.status)
    .bind(&body.note)
    .bind(resolved)
    .bind(user)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?;
    if updated.is_none() {
        return Err(AppError::NotFound);
    }
    record_audit(
        &state.pool,
        &principal,
        "overpayment_status_changed",
        "overpayment",
        Some(&id.to_string()),
        &serde_json::json!({
            "username": principal.username,
            "status": body.status,
            "note": body.note,
        }),
    )
    .await;
    Ok(Json(
        serde_json::json!({ "id": id.to_string(), "status": body.status }),
    ))
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/", axum::routing::get(list))
        .route("/{id}/status", axum::routing::post(update_status))
}

#[cfg(test)]
mod tests {
    use super::{find, PriorPayment};
    use serde_json::json;
    use std::collections::HashMap;

    fn paid(number: &str, control: &str, total: f64) -> serde_json::Value {
        json!({"claim_id": number, "claim_status_code": "1", "total_paid": total,
               "payer_claim_control_number": control, "service_lines": []})
    }

    fn prior(total: f64, control: &str, reversed: bool) -> PriorPayment {
        PriorPayment {
            total_paid: total,
            control_number: Some(control.into()),
            reversed,
        }
    }

    #[test]
    fn a_line_paid_above_its_allowed_amount_is_an_overpayment() {
        let claim = json!({"claim_id": "A", "claim_status_code": "1", "total_paid": 120.0,
            "service_lines": [{"service_line": {"line_number": 1, "paid_amount": 120.0, "allowed_amount": 95.5}}]});
        let found = find(&[claim], &HashMap::new(), &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].kind, found[0].amount),
            ("paid_above_allowed", 24.5)
        );
    }

    #[test]
    fn a_second_payment_under_another_control_number_is_a_duplicate() {
        let before = HashMap::from([("A".to_string(), prior(100.0, "ICN1", false))]);
        let found = find(&[paid("A", "ICN2", 80.0)], &before, &[]);
        assert_eq!(
            (found[0].kind, found[0].amount),
            ("duplicate_payment", 80.0)
        );
    }

    #[test]
    fn a_resent_remittance_or_a_reversed_payment_is_not_a_duplicate() {
        let before = HashMap::from([("A".to_string(), prior(100.0, "ICN1", false))]);
        assert!(find(&[paid("A", "ICN1", 100.0)], &before, &[]).is_empty());
        assert!(find(&[paid("A", "ICN2", 100.0)], &before, &["A".to_string()]).is_empty());
        let reversed = HashMap::from([("A".to_string(), prior(100.0, "ICN1", true))]);
        assert!(find(&[paid("A", "ICN2", 100.0)], &reversed, &[]).is_empty());
    }
}
