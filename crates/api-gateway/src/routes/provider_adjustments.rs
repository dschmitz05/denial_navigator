//! PLB provider-level adjustments from 835s (FB-07).
//!
//! A PLB line changes a payment without belonging to a patient claim: a
//! recoupment of an earlier overpayment (WO), a forward balance (FB), interest
//! (L6) and so on. An offset silently reduces a payment for other claims, so
//! billing staff need to see and reconcile them. A positive amount reduced the
//! payment; a negative one added to it.

use axum::extract::{Query, State};
use axum::{Extension, Json};
use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use serde::Deserialize;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::state::AppState;

fn organization_id(principal: &Principal) -> Result<Uuid, AppError> {
    principal
        .organization_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(AppError::Forbidden)
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Stores the PLB lines of one remittance. A reference that matches a claim
/// number in the organization links the adjustment to that claim. Returns how
/// many were new (a re-sent payment adds nothing).
pub async fn store(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    organization_id: Uuid,
    ingestion_id: Uuid,
    payment_info: Option<&Value>,
) -> Result<u64, AppError> {
    let Some(info) = payment_info else {
        return Ok(0);
    };
    let Some(lines) = info.get("provider_adjustments").and_then(Value::as_array) else {
        return Ok(0);
    };
    let mut stored = 0;
    for line in lines {
        let Some(reason) = text(line, "adjustment_reason_code") else {
            continue;
        };
        let amount = line.get("amount").and_then(Value::as_f64).unwrap_or(0.0);
        if amount == 0.0 {
            continue;
        }
        let result = sqlx::query(
            "INSERT INTO provider_adjustments \
                 (organization_id, ingestion_id, payer_name, payer_identifier, trace_number, \
                  payment_date, provider_identifier, fiscal_period_date, reason_code, \
                  reference_number, amount, claim_id) \
             VALUES ($1, $2, $3, $4, $5, $6::date, $7, $8::date, $9, $10, $11::numeric, \
                     (SELECT id FROM claims WHERE organization_id = $1 AND claim_number = $10)) \
             ON CONFLICT DO NOTHING",
        )
        .bind(organization_id)
        .bind(ingestion_id)
        .bind(text(info, "payer_name"))
        .bind(text(info, "payer_identifier").or_else(|| text(info, "payer_id_number")))
        .bind(text(info, "trace_number"))
        .bind(text(info, "payment_date"))
        .bind(text(line, "provider_identifier"))
        .bind(text(line, "fiscal_period_date"))
        .bind(&reason)
        .bind(text(line, "reference_number"))
        .bind(amount)
        .execute(&mut **tx)
        .await
        .map_err(AppError::Db)?;
        stored += result.rows_affected();
        // A recoupment names the claim it recovers from: the payer took back
        // the overpayment, so it no longer has to be refunded.
        if reason == "WO" {
            if let Some(reference) = text(line, "reference_number") {
                crate::routes::overpayments::mark_recouped(tx, organization_id, &reference).await?;
            }
        }
    }
    Ok(stored)
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub claim_number: Option<String>,
    pub reason_code: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    200
}

pub async fn list(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<Value>>, AppError> {
    let rows = sqlx::query(
        "SELECT pa.id, pa.payer_name, pa.trace_number, pa.payment_date, pa.provider_identifier, \
                pa.fiscal_period_date, pa.reason_code, pa.reference_number, \
                pa.amount::float8 AS amount, c.claim_number, il.file_name, pa.created_at \
         FROM provider_adjustments pa \
         LEFT JOIN claims c ON c.id = pa.claim_id \
         LEFT JOIN ingestion_log il ON il.id = pa.ingestion_id \
         WHERE pa.organization_id = $1 \
           AND ($2::text IS NULL OR c.claim_number = $2) \
           AND ($3::text IS NULL OR pa.reason_code = $3) \
         ORDER BY pa.payment_date DESC NULLS LAST, pa.created_at DESC \
         LIMIT $4",
    )
    .bind(organization_id(&principal)?)
    .bind(query.claim_number.as_deref())
    .bind(query.reason_code.as_deref())
    .bind(query.limit.clamp(1, 1000))
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    Ok(Json(rows.iter().map(row_json).collect()))
}

fn row_json(r: &sqlx::postgres::PgRow) -> Value {
    serde_json::json!({
        "id": r.get::<Uuid, _>("id").to_string(),
        "payer_name": r.get::<Option<String>, _>("payer_name"),
        "trace_number": r.get::<Option<String>, _>("trace_number"),
        "payment_date": r.get::<Option<chrono::NaiveDate>, _>("payment_date").map(|d| d.to_string()),
        "provider_identifier": r.get::<Option<String>, _>("provider_identifier"),
        "fiscal_period_date": r.get::<Option<chrono::NaiveDate>, _>("fiscal_period_date").map(|d| d.to_string()),
        "reason_code": r.get::<String, _>("reason_code"),
        "reference_number": r.get::<Option<String>, _>("reference_number"),
        "amount": r.get::<f64, _>("amount"),
        "claim_number": r.get::<Option<String>, _>("claim_number"),
        "file_name": r.get::<Option<String>, _>("file_name"),
    })
}

/// Totals by payer, month and reason, newest month first: what was recouped,
/// carried forward or paid as interest.
pub async fn summary(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<Value>>, AppError> {
    let rows = sqlx::query(
        "SELECT COALESCE(payer_name, 'Unknown') AS payer_name, \
                to_char(date_trunc('month', COALESCE(payment_date, created_at::date)), 'YYYY-MM') AS month, \
                reason_code, COUNT(*) AS lines, SUM(amount)::float8 AS amount \
         FROM provider_adjustments WHERE organization_id = $1 \
         GROUP BY 1, 2, 3 ORDER BY 2 DESC, 5 DESC",
    )
    .bind(organization_id(&principal)?)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    Ok(Json(
        rows.iter()
            .map(|r| {
                serde_json::json!({
                    "payer_name": r.get::<String, _>("payer_name"),
                    "month": r.get::<String, _>("month"),
                    "reason_code": r.get::<String, _>("reason_code"),
                    "lines": r.get::<i64, _>("lines"),
                    "amount": r.get::<f64, _>("amount"),
                })
            })
            .collect(),
    ))
}

/// Adjustments linked to one claim, for its detail view.
pub async fn for_claim(
    pool: &sqlx::PgPool,
    organization_id: Uuid,
    claim_id: Uuid,
) -> Result<Vec<Value>, AppError> {
    let rows = sqlx::query(
        "SELECT pa.id, pa.payer_name, pa.trace_number, pa.payment_date, pa.provider_identifier, \
                pa.fiscal_period_date, pa.reason_code, pa.reference_number, \
                pa.amount::float8 AS amount, c.claim_number, il.file_name \
         FROM provider_adjustments pa \
         JOIN claims c ON c.id = pa.claim_id \
         LEFT JOIN ingestion_log il ON il.id = pa.ingestion_id \
         WHERE pa.organization_id = $1 AND pa.claim_id = $2 \
         ORDER BY pa.payment_date DESC NULLS LAST",
    )
    .bind(organization_id)
    .bind(claim_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)?;
    Ok(rows.iter().map(row_json).collect())
}
