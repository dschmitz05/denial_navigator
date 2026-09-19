//! Remittances that revisit a claim: payer reversals (835 CLP02 22) and later
//! payments that settle a denial.
//!
//! A reprocessed claim arrives as more claim-payment loops for the same claim
//! number, often a reversal followed by the corrected payment in the same file.
//! Without this module the reversal's negated totals could overwrite the claim,
//! the second loop became a phantom claim with a random suffix, and a denial the
//! payer later paid stayed open, so recovery was only ever recorded by hand.

use denial_common::error::AppError;
use denial_domain::TERMINAL_WORK_OUTCOMES;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

/// CLP02 for a payer's reversal of an earlier payment (X12 835).
const REVERSAL_STATUS: &str = "22";

/// Denial statuses that still represent open work a payment can settle.
pub const ACTIVE_DENIAL_STATUSES: &[&str] =
    &["open", "analyzed", "in_progress", "in_appeal", "appealed"];

/// CLP02 values meaning the payer processed the claim and paid it: as
/// primary, secondary or tertiary, or forwarded to another payer.
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

pub fn is_reversal(claim: &Value) -> bool {
    text(claim, "claim_status_code") == Some(REVERSAL_STATUS)
}

/// Separates reversal loops from the rest. The reversals are recorded against
/// their claims and must not be stored as claims themselves.
pub fn split_reversals(claims: &[Value]) -> (Vec<Value>, Vec<String>) {
    let mut kept = Vec::new();
    let mut reversed = Vec::new();
    for claim in claims {
        if is_reversal(claim) {
            if let Some(number) = text(claim, "claim_id") {
                reversed.push(number.to_string());
            }
        } else {
            kept.push(claim.clone());
        }
    }
    (kept, reversed)
}

/// A payment this remittance made on a claim or one of its lines, with the
/// adjustments it still applies there (as "GROUP:CARC").
#[derive(Debug, PartialEq)]
pub struct Payment {
    pub claim_number: String,
    /// `None` for the claim as a whole.
    pub line_number: Option<i64>,
    /// CPT or HCPCS on the line. Payers can renumber lines on reprocessing, so
    /// a line matches a denial only if this agrees too (when both are known).
    pub procedure: Option<String>,
    pub paid: f64,
    pub still_adjusted: Vec<String>,
}

fn adjustment_keys(adjustments: Option<&Value>) -> Vec<String> {
    adjustments
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|adj| number(adj, "amount") != 0.0)
        .filter_map(|adj| {
            let group = text(adj, "adjustment_group_code")?;
            let carc = text(adj, "reason_code")?;
            Some(format!("{group}:{carc}"))
        })
        .collect()
}

/// Every claim and line this remittance pays. Only paid claim statuses count,
/// so a denial-only or pending loop never settles anything.
pub fn payments(claims: &[Value]) -> Vec<Payment> {
    let mut found = Vec::new();
    for claim in claims {
        let Some(claim_number) = text(claim, "claim_id") else {
            continue;
        };
        if !text(claim, "claim_status_code").is_some_and(|code| PAID_STATUSES.contains(&code)) {
            continue;
        }
        if number(claim, "total_paid") > 0.0 {
            found.push(Payment {
                claim_number: claim_number.to_string(),
                line_number: None,
                procedure: None,
                paid: number(claim, "total_paid"),
                still_adjusted: adjustment_keys(claim.get("claim_level_adjustments")),
            });
        }
        for detail in claim
            .get("service_lines")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let line = detail.get("service_line").unwrap_or(&Value::Null);
            let paid = number(line, "paid_amount");
            if paid <= 0.0 {
                continue;
            }
            found.push(Payment {
                claim_number: claim_number.to_string(),
                line_number: line.get("line_number").and_then(Value::as_i64),
                procedure: text(line, "cpt_code")
                    .or_else(|| text(line, "hcpcs_code"))
                    .map(str::to_string),
                paid,
                still_adjusted: adjustment_keys(detail.get("adjustments")),
            });
        }
    }
    found
}

/// Records the payer's reversal on each claim. Totals are left as they are:
/// the corrected loop that usually follows carries the real ones.
pub async fn mark_reversed(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    organization_id: Uuid,
    claim_numbers: &[String],
) -> Result<u64, AppError> {
    if claim_numbers.is_empty() {
        return Ok(0);
    }
    let result = sqlx::query(
        "UPDATE claims SET reversed_at = NOW(), updated_at = NOW() \
         WHERE organization_id = $1 AND claim_number = ANY($2::text[])",
    )
    .bind(organization_id)
    .bind(claim_numbers)
    .execute(&mut **tx)
    .await
    .map_err(AppError::Db)?;
    Ok(result.rows_affected())
}

/// A denial this remittance settled.
pub struct Settled {
    pub denial_id: Uuid,
    pub claim_number: String,
    pub status: String,
    pub recovered: f64,
}

/// Closes active denials from earlier remittances that this one now pays.
///
/// A denial is settled when its claim (claim-level denial) or its line
/// (line-level denial) is paid here and this remittance no longer applies the
/// same group and CARC there. One under appeal becomes `overruled`, anything
/// else `resolved`; its open worklist items are closed to match. Denials created
/// by this same remittance are never touched.
pub async fn settle_paid_denials(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    organization_id: Uuid,
    payments: &[Payment],
) -> Result<Vec<Settled>, AppError> {
    let mut settled = Vec::new();
    for payment in payments {
        let rows = sqlx::query(
            "UPDATE denials d SET \
                 status = CASE WHEN d.status IN ('in_appeal', 'appealed') \
                               THEN 'overruled' ELSE 'resolved' END, \
                 resolution_source = 'remittance', \
                 recovered_amount = LEAST($4::numeric, \
                     CASE WHEN d.adjustment_amount > 0 THEN d.adjustment_amount ELSE $4::numeric END), \
                 resolved_at = NOW(), \
                 updated_at = NOW() \
             FROM claims c \
             WHERE d.claim_id = c.id \
               AND c.organization_id = $1 AND c.claim_number = $2 \
               AND d.service_line_number IS NOT DISTINCT FROM $3 \
               AND ($7::text IS NULL OR COALESCE(d.cpt_code, d.hcpcs_code) IS NULL \
                    OR COALESCE(d.cpt_code, d.hcpcs_code) = $7) \
               AND d.status = ANY($5::text[]) \
               AND d.created_at < transaction_timestamp() \
               AND (d.cagc || ':' || COALESCE(d.carc_code, '')) <> ALL($6::text[]) \
             RETURNING d.id, d.status, d.recovered_amount::float8 AS recovered",
        )
        .bind(organization_id)
        .bind(&payment.claim_number)
        .bind(payment.line_number)
        .bind(payment.paid)
        .bind(ACTIVE_DENIAL_STATUSES)
        .bind(&payment.still_adjusted)
        .bind(payment.procedure.as_deref())
        .fetch_all(&mut **tx)
        .await
        .map_err(AppError::Db)?;
        for row in rows {
            settled.push(Settled {
                denial_id: row.get("id"),
                claim_number: payment.claim_number.clone(),
                status: row.get("status"),
                recovered: row.try_get("recovered").unwrap_or(0.0),
            });
        }
    }

    let ids: Vec<Uuid> = settled.iter().map(|s| s.denial_id).collect();
    if !ids.is_empty() {
        sqlx::query(
            "UPDATE appeals_queue aq SET \
                 outcome_status = CASE WHEN d.status = 'overruled' THEN 'approved' ELSE 'resolved' END, \
                 updated_at = NOW() \
             FROM denials d \
             WHERE aq.denial_id = d.id AND d.id = ANY($1::uuid[]) \
               AND (aq.outcome_status IS NULL OR aq.outcome_status <> ALL($2::text[]))",
        )
        .bind(&ids)
        .bind(TERMINAL_WORK_OUTCOMES)
        .execute(&mut **tx)
        .await
        .map_err(AppError::Db)?;
    }
    Ok(settled)
}

/// Writes one audit entry per settled denial, after the transaction commits.
pub async fn audit_settled(
    pool: &sqlx::PgPool,
    organization_id: Uuid,
    file_name: &str,
    settled: &[Settled],
) {
    let organization = organization_id.to_string();
    for denial in settled {
        let id = denial.denial_id.to_string();
        denial_audit::record(
            pool,
            "denial_settled_by_remittance",
            "denial",
            Some(&id),
            None,
            &serde_json::json!({
                "claim_number": denial.claim_number,
                "status": denial.status,
                "recovered_amount": denial.recovered,
                "file_name": file_name,
            }),
            None,
            None,
            Some(&organization),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::{payments, split_reversals, Payment};
    use serde_json::json;

    fn reprocessed_file() -> Vec<serde_json::Value> {
        vec![
            json!({"claim_id": "PAT009", "claim_status_code": "22", "total_paid": 0.0}),
            json!({
                "claim_id": "PAT009", "claim_status_code": "1", "total_paid": 800.0,
                "service_lines": [
                    {"service_line": {"line_number": 1, "cpt_code": "71046", "paid_amount": 800.0}, "adjustments": []},
                    {"service_line": {"line_number": 2, "paid_amount": 50.0},
                     "adjustments": [{"adjustment_group_code": "CO", "reason_code": "45", "amount": 20.0}]},
                    {"service_line": {"line_number": 3, "paid_amount": 0.0},
                     "adjustments": [{"adjustment_group_code": "CO", "reason_code": "97", "amount": 90.0}]}
                ]
            }),
        ]
    }

    #[test]
    fn reversals_are_separated_from_claims_to_store() {
        let (kept, reversed) = split_reversals(&reprocessed_file());
        assert_eq!(reversed, ["PAT009"]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0]["claim_status_code"], "1");
    }

    #[test]
    fn payments_list_paid_claims_and_lines_with_remaining_adjustments() {
        let (kept, _) = split_reversals(&reprocessed_file());
        assert_eq!(
            payments(&kept),
            vec![
                Payment {
                    claim_number: "PAT009".into(),
                    line_number: None,
                    procedure: None,
                    paid: 800.0,
                    still_adjusted: vec![]
                },
                Payment {
                    claim_number: "PAT009".into(),
                    line_number: Some(1),
                    procedure: Some("71046".into()),
                    paid: 800.0,
                    still_adjusted: vec![]
                },
                Payment {
                    claim_number: "PAT009".into(),
                    line_number: Some(2),
                    procedure: None,
                    paid: 50.0,
                    still_adjusted: vec!["CO:45".into()],
                },
            ]
        );
    }

    #[test]
    fn denied_or_pending_claims_settle_nothing() {
        let denied = json!({"claim_id": "PAT010", "claim_status_code": "4", "total_paid": 0.0});
        let pending = json!({"claim_id": "PAT011", "claim_status_code": "4", "total_paid": 10.0});
        assert!(payments(&[denied, pending]).is_empty());
    }
}
