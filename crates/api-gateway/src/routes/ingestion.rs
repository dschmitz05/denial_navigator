//! Ingestion routes: 835/837 EDI file upload, parse, and store.

use std::collections::HashSet;

use axum::extract::{Multipart, Query, State};
use axum::routing::{get, post};
use axum::Extension;
use axum::Json;
use axum::Router;
use chrono::{NaiveDate, Utc};
use denial_auth::rbac::{Principal, PrincipalKind};
use denial_common::error::AppError;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::{Column, Row};
use uuid::Uuid;

use crate::reprocessing;
use crate::routes::{overpayments, provider_adjustments};
use crate::state::AppState;

const MAX_FILE_SIZE: usize = 25 * 1024 * 1024;

// ── Models ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct StoreIngestion {
    /// Required for service-to-service ingestion; user requests use their active membership.
    pub organization_id: Option<Uuid>,
    pub file_name: String,
    #[serde(default)]
    pub file_path: Option<String>,
    pub file_hash: String,
    pub file_size: i64,
    pub claims: Vec<serde_json::Value>,
    pub denials: Vec<serde_json::Value>,
    #[serde(default)]
    pub transaction_type: Option<String>,
    /// The 835's payment details, including its PLB provider adjustments.
    #[serde(default)]
    pub payment_info: Option<serde_json::Value>,
}

#[derive(Deserialize)]
pub struct ListIngestionLogQuery {
    #[serde(default = "default_log_limit")]
    pub limit: i64,
}

fn default_log_limit() -> i64 {
    100
}

#[derive(Deserialize)]
pub struct IngestHistoryQuery {
    #[serde(default = "default_history_limit")]
    pub limit: i64,
}

fn default_history_limit() -> i64 {
    50
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn limit_key(principal: &Principal) -> String {
    if principal.kind == PrincipalKind::User {
        if let Some(ref uid) = principal.user_id {
            return format!("user:{uid}");
        }
    }
    if !principal.username.is_empty() && principal.username != "anonymous" {
        return format!("svc:{}", principal.username);
    }
    format!("ip:{}", principal.ip.as_deref().unwrap_or("unknown"))
}

fn organization_id(principal: &Principal, requested: Option<Uuid>) -> Result<Uuid, AppError> {
    if principal.kind == PrincipalKind::Service {
        return requested.ok_or(AppError::Forbidden);
    }
    principal
        .organization_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(AppError::Forbidden)
}

fn validate_extension(filename: &str) -> Result<(), AppError> {
    if filename.is_empty() {
        return Err(AppError::BadRequest("No file name provided".into()));
    }
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    if ![".835", ".837", ".edi"]
        .iter()
        .any(|e| ext == e.trim_start_matches('.'))
    {
        return Err(AppError::BadRequest(format!(
            "Invalid file type '{filename}'. Allowed extensions: .835, .837, .edi"
        )));
    }
    Ok(())
}

fn parse_date(val: &str) -> Option<NaiveDate> {
    let s = val.trim();
    if s.is_empty() {
        return None;
    }
    for fmt in ["%Y%m%d", "%Y-%m-%d", "%m/%d/%Y", "%m%d%Y"] {
        if let Ok(d) = chrono::NaiveDate::parse_from_str(s, fmt) {
            return Some(d);
        }
    }
    None
}

fn str_from_json(v: &serde_json::Value) -> Option<String> {
    v.as_str().map(|s| s.to_string())
}

fn f64_from_json(v: &serde_json::Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

fn vec_str_from_json(v: &serde_json::Value) -> Vec<String> {
    match v {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect(),
        _ => Vec::new(),
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
                    v.map(serde_json::Value::from)
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
                row.try_get::<Option<chrono::DateTime<Utc>>, _>(name)
                    .map(|v| {
                        v.map(|v| serde_json::Value::String(v.to_rfc3339()))
                            .unwrap_or(serde_json::Value::Null)
                    })
            })
            .unwrap_or(serde_json::Value::Null);
        map.insert(name.to_string(), val);
    }
    serde_json::Value::Object(map)
}

fn clean_claim(claim: &serde_json::Value, seen_numbers: &mut HashSet<String>) -> serde_json::Value {
    let raw_id = str_from_json(claim.get("claim_id").unwrap_or(&serde_json::Value::Null))
        .unwrap_or_default();
    let patient_id = str_from_json(claim.get("patient_id").unwrap_or(&serde_json::Value::Null))
        .unwrap_or_default();
    let dob_raw = str_from_json(
        claim
            .get("date_of_birth")
            .unwrap_or(&serde_json::Value::Null),
    );
    let dob = dob_raw.as_deref().and_then(parse_date);
    let patient_name = claim.get("patient_name").cloned();
    let provider_npi = claim.get("provider_npi").cloned();
    let provider_name = claim.get("provider_name").cloned();
    let payer_name = claim.get("payer_name").cloned();
    let payer_id_number = claim.get("payer_id_number").cloned();
    let total_charged = f64_from_json(
        claim
            .get("total_charged")
            .unwrap_or(&serde_json::Value::Null),
    );
    let total_paid = f64_from_json(claim.get("total_paid").unwrap_or(&serde_json::Value::Null));
    let total_adjustment = f64_from_json(
        claim
            .get("total_adjustment")
            .unwrap_or(&serde_json::Value::Null),
    );
    let claim_type = str_from_json(claim.get("claim_type").unwrap_or(&serde_json::Value::Null))
        .unwrap_or_else(|| "professional".into());
    let facility_type_code = str_from_json(
        claim
            .get("facility_type_code")
            .unwrap_or(&serde_json::Value::Null),
    );
    let service_from = str_from_json(
        claim
            .get("service_from")
            .unwrap_or(&serde_json::Value::Null),
    )
    .and_then(|s| parse_date(&s));
    let service_to = str_from_json(claim.get("service_to").unwrap_or(&serde_json::Value::Null))
        .and_then(|s| parse_date(&s));
    let diagnosis_codes = vec_str_from_json(
        claim
            .get("diagnosis_codes")
            .unwrap_or(&serde_json::Value::Null),
    );

    let base = if let Some(dob) = dob {
        format!("{patient_id}-{dob}")
    } else {
        format!("{patient_id}-NODOB")
    };
    let mut claim_number = if raw_id.is_empty() {
        base.clone()
    } else {
        raw_id.clone()
    };

    if seen_numbers.contains(&claim_number) {
        let suffix = Uuid::new_v4()
            .to_string()
            .chars()
            .take(8)
            .collect::<String>();
        let id_part = if raw_id.is_empty() {
            base
        } else {
            raw_id.clone()
        };
        claim_number = format!("{id_part}-{suffix}");
    }
    seen_numbers.insert(claim_number.clone());

    serde_json::json!({
        "claim_id": claim_number,
        "patient_id": patient_id,
        "patient_name": patient_name,
        "date_of_birth": dob.map(|d| d.to_string()).unwrap_or_default(),
        "provider_npi": provider_npi,
        "provider_name": provider_name,
        "payer_name": payer_name,
        "payer_id_number": payer_id_number,
        "total_charged": total_charged,
        "total_paid": total_paid,
        "total_adjustment": total_adjustment,
        "claim_type": claim_type,
        "facility_type_code": facility_type_code,
        "service_from": service_from.map(|d| d.to_string()).unwrap_or_default(),
        "service_to": service_to.map(|d| d.to_string()).unwrap_or_default(),
        "diagnosis_codes": diagnosis_codes,
        "next_payer_name": claim.get("next_payer_name").cloned(),
        // Kept so a later remittance can tell a re-sent payment (same control
        // number) from a second, duplicate one (FB-08).
        "payer_claim_control_number": claim.get("payer_claim_control_number").cloned(),
        "claim_status_code": claim.get("claim_status_code").cloned(),
        "next_payer_source": claim.get("next_payer_source").cloned(),
    })
}

fn clean_denial(denial: &serde_json::Value) -> serde_json::Value {
    let claim_id = str_from_json(denial.get("claim_id").unwrap_or(&serde_json::Value::Null))
        .unwrap_or_default();
    let service_line_number = denial.get("service_line_number").and_then(|v| v.as_i64());
    let cpt_code = str_from_json(denial.get("cpt_code").unwrap_or(&serde_json::Value::Null));
    let hcpcs_code = str_from_json(denial.get("hcpcs_code").unwrap_or(&serde_json::Value::Null));
    let modifier_1 = str_from_json(denial.get("modifier_1").unwrap_or(&serde_json::Value::Null));
    let modifier_2 = str_from_json(denial.get("modifier_2").unwrap_or(&serde_json::Value::Null));
    let charge_amount = f64_from_json(
        denial
            .get("charge_amount")
            .unwrap_or(&serde_json::Value::Null),
    );
    let payment_amount = f64_from_json(
        denial
            .get("payment_amount")
            .unwrap_or(&serde_json::Value::Null),
    );
    let adjustment_amount = f64_from_json(
        denial
            .get("adjustment_amount")
            .unwrap_or(&serde_json::Value::Null),
    );
    let cagc = str_from_json(denial.get("cagc").unwrap_or(&serde_json::Value::Null));
    let carc_code = str_from_json(denial.get("carc_code").unwrap_or(&serde_json::Value::Null));
    let rarc_code = str_from_json(denial.get("rarc_code").unwrap_or(&serde_json::Value::Null));
    let denial_reason = str_from_json(
        denial
            .get("denial_reason")
            .unwrap_or(&serde_json::Value::Null),
    )
    .or_else(|| {
        str_from_json(
            denial
                .get("denial_reason_code")
                .unwrap_or(&serde_json::Value::Null),
        )
    });
    let denial_date = str_from_json(
        denial
            .get("denial_date")
            .unwrap_or(&serde_json::Value::Null),
    )
    .and_then(|s| parse_date(&s));
    // The 835 QTY segment quantity, if one was reported - distinct from
    // SVC05 billed units. `f64_from_json` returns 0.0 for a missing/null
    // value, which is indistinguishable from a genuinely reported zero;
    // only carry it through when the source actually had the key.
    let reported_quantity = denial
        .get("reported_quantity")
        .filter(|v| !v.is_null())
        .map(f64_from_json);
    let quantity_qualifier = str_from_json(
        denial
            .get("quantity_qualifier")
            .unwrap_or(&serde_json::Value::Null),
    );

    serde_json::json!({
        "claim_id": claim_id,
        "service_line_number": service_line_number,
        "cpt_code": cpt_code,
        "hcpcs_code": hcpcs_code,
        "modifier_1": modifier_1,
        "modifier_2": modifier_2,
        "charge_amount": charge_amount,
        "payment_amount": payment_amount,
        "adjustment_amount": adjustment_amount,
        "cagc": cagc,
        "carc_code": carc_code,
        "rarc_code": rarc_code,
        "denial_reason": denial_reason,
        "denial_date": denial_date.map(|d| d.to_string()).unwrap_or_default(),
        "reported_quantity": reported_quantity,
        "quantity_qualifier": quantity_qualifier,
    })
}

// ── Claims upsert SQL ──────────────────────────────────────────────────

const CLAIM_INSERT: &str = "INSERT INTO claims \
    (organization_id, claim_number, patient_id, patient_name, date_of_birth, provider_npi, provider_name, \
     payer_name, payer_id_number, total_charge, total_paid, total_adjustment, \
     status, claim_type, facility_type_code, service_from, service_to, icd_10_codes, raw_835_data, parsed_at) \
    VALUES ($1, $2, $3, $4, $5::date, $6, $7, $8, $9, $10, $11, $12, \
            'parsed', $13, $14, $15::date, $16::date, $17::text[], $18::jsonb, NOW())";

const ON_CONFLICT_835: &str = "ON CONFLICT (organization_id, claim_number) DO UPDATE SET \
    patient_name     = COALESCE(EXCLUDED.patient_name, claims.patient_name), \
    date_of_birth    = COALESCE(EXCLUDED.date_of_birth, claims.date_of_birth), \
    provider_npi     = COALESCE(EXCLUDED.provider_npi, claims.provider_npi), \
    provider_name    = COALESCE(EXCLUDED.provider_name, claims.provider_name), \
    payer_name       = COALESCE(EXCLUDED.payer_name, claims.payer_name), \
    payer_id_number  = COALESCE(EXCLUDED.payer_id_number, claims.payer_id_number), \
    total_charge     = CASE WHEN EXCLUDED.total_charge > 0 \
                            THEN EXCLUDED.total_charge ELSE claims.total_charge END, \
    total_paid       = EXCLUDED.total_paid, \
    total_adjustment = EXCLUDED.total_adjustment, \
    facility_type_code = COALESCE(EXCLUDED.facility_type_code, claims.facility_type_code), \
    service_from     = COALESCE(EXCLUDED.service_from, claims.service_from), \
    service_to       = COALESCE(EXCLUDED.service_to, claims.service_to), \
    icd_10_codes     = CASE \
                        WHEN EXCLUDED.icd_10_codes IS NOT NULL \
                         AND cardinality(EXCLUDED.icd_10_codes) > 0 \
                        THEN EXCLUDED.icd_10_codes ELSE claims.icd_10_codes END, \
    status           = 'parsed', \
    raw_835_data     = EXCLUDED.raw_835_data, \
    parsed_at        = NOW(), \
    updated_at       = NOW()";

/// 837→835 correlation: the 837 (response) is merged into the 835 (claim) only
/// when the match is corroborated by more than the claim number alone. The
/// merge records the real confidence instead of a hardcoded 1.00.
const ON_CONFLICT_837_MERGE: &str = "ON CONFLICT (organization_id, claim_number) DO UPDATE SET \
    patient_name     = COALESCE(claims.patient_name, EXCLUDED.patient_name), \
    date_of_birth    = COALESCE(claims.date_of_birth, EXCLUDED.date_of_birth), \
    provider_npi     = COALESCE(claims.provider_npi, EXCLUDED.provider_npi), \
    provider_name    = COALESCE(claims.provider_name, EXCLUDED.provider_name), \
    payer_name       = COALESCE(claims.payer_name, EXCLUDED.payer_name), \
    payer_id_number  = COALESCE(claims.payer_id_number, EXCLUDED.payer_id_number), \
    total_charge     = CASE WHEN claims.total_charge = 0 \
                            THEN EXCLUDED.total_charge ELSE claims.total_charge END, \
    claim_type       = EXCLUDED.claim_type, \
    facility_type_code = COALESCE(EXCLUDED.facility_type_code, claims.facility_type_code), \
    service_from     = COALESCE(claims.service_from, EXCLUDED.service_from), \
    service_to       = COALESCE(claims.service_to, EXCLUDED.service_to), \
    icd_10_codes     = CASE \
                        WHEN EXCLUDED.icd_10_codes IS NOT NULL \
                         AND cardinality(EXCLUDED.icd_10_codes) > 0 \
                        THEN EXCLUDED.icd_10_codes ELSE claims.icd_10_codes END, \
    correlation_status = 'matched', \
    correlation_confidence = $19, \
    updated_at       = NOW()";

/// A 837 is treated as the same claim as the 835 only when the match score
/// reaches this threshold. The claim number alone (0.5) is insufficient; at
/// least one corroborating field is required.
const CORRELATION_MATCH_THRESHOLD: f64 = 0.6;

/// Deterministic, explainable 837→835 match score in `[0, 1]`. The claim
/// number is the lookup key and always matches, so it contributes the 0.5
/// base. Each corroborating field adds its weight. An incoming 837 that is
/// missing a field (None / 0) does not earn that field's points — absence of
/// data is not evidence of a match.
#[derive(Clone, Copy)]
struct MatchFields<'a> {
    svc_from: Option<&'a str>,
    svc_to: Option<&'a str>,
    total_charge: f64,
    provider_npi: Option<&'a str>,
    payer_name: Option<&'a str>,
}

fn correlation_score(incoming: &MatchFields, existing: &MatchFields) -> f64 {
    let mut score: f64 = 0.5;
    if let (Some(a), Some(b)) = (incoming.svc_from, existing.svc_from) {
        if a == b {
            score += 0.1;
        }
    }
    if let (Some(a), Some(b)) = (incoming.svc_to, existing.svc_to) {
        if a == b {
            score += 0.1;
        }
    }
    if incoming.total_charge > 0.0 && (incoming.total_charge - existing.total_charge).abs() <= 0.01
    {
        score += 0.15;
    }
    if let (Some(a), Some(b)) = (incoming.provider_npi, existing.provider_npi) {
        if a == b {
            score += 0.1;
        }
    }
    if let (Some(a), Some(b)) = (incoming.payer_name, existing.payer_name) {
        if a.eq_ignore_ascii_case(b) {
            score += 0.15;
        }
    }
    score.min(1.0)
}

/// Upsert a single parsed claim. 835 (or unknown) is a re-parse of the same
/// claim and merges as before. An 837 (response) is correlated against any
/// existing claim with the same number: a corroborated match merges with its
/// real confidence; an unconfirmed match is flagged `ambiguous` for review and
/// is NOT merged over the 835.
async fn upsert_claim(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    organization_id: Uuid,
    transaction_type: Option<&str>,
    claim_data: &serde_json::Value,
) -> Result<(), AppError> {
    let claim_id: &str = claim_data
        .get("claim_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let patient_id: &str = claim_data
        .get("patient_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let patient_name: Option<&str> = claim_data.get("patient_name").and_then(|v| v.as_str());
    let dob: Option<&str> = claim_data
        .get("date_of_birth")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let provider_npi: Option<&str> = claim_data.get("provider_npi").and_then(|v| v.as_str());
    let provider_name: Option<&str> = claim_data.get("provider_name").and_then(|v| v.as_str());
    let payer_name: Option<&str> = claim_data.get("payer_name").and_then(|v| v.as_str());
    let payer_id_number: Option<&str> = claim_data.get("payer_id_number").and_then(|v| v.as_str());
    let total_charge: f64 = claim_data
        .get("total_charged")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let total_paid: f64 = claim_data
        .get("total_paid")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let total_adjustment: f64 = claim_data
        .get("total_adjustment")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let claim_type: &str = claim_data
        .get("claim_type")
        .and_then(|v| v.as_str())
        .unwrap_or("professional");
    let facility_type_code: Option<&str> = claim_data
        .get("facility_type_code")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let service_from: Option<&str> = claim_data
        .get("service_from")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let service_to: Option<&str> = claim_data
        .get("service_to")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let icd_codes: Vec<String> = claim_data
        .get("diagnosis_codes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();

    if transaction_type == Some("837") {
        let existing = sqlx::query(
            "SELECT service_from, service_to, total_charge, provider_npi, payer_name \
             FROM claims WHERE organization_id = $1 AND claim_number = $2",
        )
        .bind(organization_id)
        .bind(claim_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(AppError::Db)?;

        let score = match &existing {
            Some(row) => {
                let incoming = MatchFields {
                    svc_from: service_from,
                    svc_to: service_to,
                    total_charge,
                    provider_npi,
                    payer_name,
                };
                let existing = MatchFields {
                    svc_from: row.try_get("service_from").ok().flatten(),
                    svc_to: row.try_get("service_to").ok().flatten(),
                    total_charge: row.try_get("total_charge").unwrap_or(0.0),
                    provider_npi: row.try_get("provider_npi").ok().flatten(),
                    payer_name: row.try_get("payer_name").ok().flatten(),
                };
                correlation_score(&incoming, &existing)
            }
            None => 0.0,
        };

        if existing.is_none() {
            // No 835 to correlate against: insert the 837 as a new claim.
            sqlx::query(CLAIM_INSERT)
                .bind(organization_id)
                .bind(claim_id)
                .bind(patient_id)
                .bind(patient_name)
                .bind(dob)
                .bind(provider_npi)
                .bind(provider_name)
                .bind(payer_name)
                .bind(payer_id_number)
                .bind(total_charge)
                .bind(total_paid)
                .bind(total_adjustment)
                .bind(claim_type)
                .bind(facility_type_code)
                .bind(service_from)
                .bind(service_to)
                .bind(icd_codes)
                .bind(claim_data.to_string())
                .execute(&mut **tx)
                .await
                .map_err(AppError::Db)?;
        } else if score >= CORRELATION_MATCH_THRESHOLD {
            // Corroborated match: merge, recording the real confidence.
            sqlx::query(&format!("{CLAIM_INSERT} {ON_CONFLICT_837_MERGE}"))
                .bind(organization_id)
                .bind(claim_id)
                .bind(patient_id)
                .bind(patient_name)
                .bind(dob)
                .bind(provider_npi)
                .bind(provider_name)
                .bind(payer_name)
                .bind(payer_id_number)
                .bind(total_charge)
                .bind(total_paid)
                .bind(total_adjustment)
                .bind(claim_type)
                .bind(facility_type_code)
                .bind(service_from)
                .bind(service_to)
                .bind(icd_codes)
                .bind(claim_data.to_string())
                .bind(score)
                .execute(&mut **tx)
                .await
                .map_err(AppError::Db)?;
        } else {
            // Unconfirmed: flag for review; do NOT merge the 837 over the 835.
            sqlx::query(
                "UPDATE claims SET correlation_status = 'ambiguous', \
                        correlation_confidence = $3, updated_at = NOW() \
                 WHERE organization_id = $1 AND claim_number = $2",
            )
            .bind(organization_id)
            .bind(claim_id)
            .bind(score)
            .execute(&mut **tx)
            .await
            .map_err(AppError::Db)?;
        }
    } else {
        // 835 (or unknown): a re-parse of the same claim; merge as before.
        sqlx::query(&format!("{CLAIM_INSERT} {ON_CONFLICT_835}"))
            .bind(organization_id)
            .bind(claim_id)
            .bind(patient_id)
            .bind(patient_name)
            .bind(dob)
            .bind(provider_npi)
            .bind(provider_name)
            .bind(payer_name)
            .bind(payer_id_number)
            .bind(total_charge)
            .bind(total_paid)
            .bind(total_adjustment)
            .bind(claim_type)
            .bind(facility_type_code)
            .bind(service_from)
            .bind(service_to)
            .bind(icd_codes)
            .bind(claim_data.to_string())
            .execute(&mut **tx)
            .await
            .map_err(AppError::Db)?;
    }
    Ok(())
}

/// Records a payer that pays after this claim's payer, when the file names
/// one; a later file without that information leaves it in place.
async fn record_next_payer(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    organization_id: Uuid,
    claim: &serde_json::Value,
) -> Result<(), AppError> {
    let name = claim
        .get("next_payer_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let (Some(name), Some(number)) = (name, claim.get("claim_id").and_then(|v| v.as_str())) else {
        return Ok(());
    };
    let source = claim.get("next_payer_source").and_then(|v| v.as_str());
    sqlx::query(
        "UPDATE claims SET next_payer_name = $3, next_payer_source = $4, updated_at = NOW() \
         WHERE organization_id = $1 AND claim_number = $2",
    )
    .bind(organization_id)
    .bind(number)
    .bind(name)
    .bind(source)
    .execute(&mut **tx)
    .await
    .map_err(AppError::Db)?;
    Ok(())
}

/// Stamps when claims were submitted (837) or first answered (835), so a
/// submitted claim the payer never answers can be followed up (FB-09).
async fn record_submission_or_response(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    organization_id: Uuid,
    transaction_type: Option<&str>,
    claims: &[serde_json::Value],
) -> Result<(), AppError> {
    let numbers: Vec<&str> = claims
        .iter()
        .filter_map(|c| c.get("claim_id").and_then(|v| v.as_str()))
        .collect();
    if numbers.is_empty() {
        return Ok(());
    }
    let column = if transaction_type == Some("837") {
        "submitted_at"
    } else {
        "remittance_received_at"
    };
    sqlx::query(&format!(
        "UPDATE claims SET {column} = COALESCE({column}, NOW()) \
         WHERE organization_id = $1 AND claim_number = ANY($2::text[])"
    ))
    .bind(organization_id)
    .bind(&numbers)
    .execute(&mut **tx)
    .await
    .map_err(AppError::Db)?;
    Ok(())
}

/// What storing one parsed file changed.
struct Stored {
    claims: usize,
    denials: usize,
    denials_written: i64,
    claims_reversed: u64,
    provider_adjustments: u64,
    overpayments: u64,
    settled: Vec<reprocessing::Settled>,
}

/// Stores a parsed file's claims and denials inside `tx`.
///
/// Reversal loops (835 CLP02 22) are recorded on their claims instead of being
/// stored as claims, so their negated totals never overwrite the claim and the
/// corrected loop that follows keeps the real claim number. Payments on claims
/// that already have open denials settle them; see `reprocessing`.
async fn store_remittance(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    organization_id: Uuid,
    ingestion_id: Uuid,
    transaction_type: Option<&str>,
    payment_info: Option<&serde_json::Value>,
    claims: &[serde_json::Value],
    denials: &[serde_json::Value],
) -> Result<Stored, AppError> {
    let (claims, reversed_numbers) = reprocessing::split_reversals(claims);
    // Read before the upsert overwrites what each claim had been paid.
    let prior_payments = overpayments::prior_payments(tx, organization_id, &claims).await?;
    let mut seen_numbers = HashSet::new();
    let cleaned_claims: Vec<serde_json::Value> = claims
        .iter()
        .map(|c| clean_claim(c, &mut seen_numbers))
        .collect();
    let cleaned_denials: Vec<serde_json::Value> = denials.iter().map(clean_denial).collect();

    for claim_data in &cleaned_claims {
        upsert_claim(tx, organization_id, transaction_type, claim_data).await?;
        record_next_payer(tx, organization_id, claim_data).await?;
    }
    record_submission_or_response(tx, organization_id, transaction_type, &cleaned_claims).await?;

    let mut denials_written: i64 = 0;
    for denial_data in &cleaned_denials {
        let claim_id: &str = denial_data
            .get("claim_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let service_line_number: Option<i64> = denial_data
            .get("service_line_number")
            .and_then(|v| v.as_i64());
        let cpt_code: Option<&str> = denial_data.get("cpt_code").and_then(|v| v.as_str());
        let hcpcs_code: Option<&str> = denial_data.get("hcpcs_code").and_then(|v| v.as_str());
        let modifier_1: Option<&str> = denial_data.get("modifier_1").and_then(|v| v.as_str());
        let modifier_2: Option<&str> = denial_data.get("modifier_2").and_then(|v| v.as_str());
        let charge_amount: f64 = denial_data
            .get("charge_amount")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let payment_amount: f64 = denial_data
            .get("payment_amount")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let adjustment_amount: f64 = denial_data
            .get("adjustment_amount")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let cagc: Option<&str> = denial_data.get("cagc").and_then(|v| v.as_str());
        let carc_code: Option<&str> = denial_data.get("carc_code").and_then(|v| v.as_str());
        let rarc_code: Option<&str> = denial_data.get("rarc_code").and_then(|v| v.as_str());
        let denial_reason: Option<&str> = denial_data.get("denial_reason").and_then(|v| v.as_str());
        let denial_date: Option<&str> = denial_data
            .get("denial_date")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        // The 835 QTY segment quantity, if one was reported on this line -
        // distinct from SVC05 billed units. Lets an MUE evidence check
        // compare what was reported against a code's per-date unit limit.
        let reported_quantity: Option<f64> = denial_data
            .get("reported_quantity")
            .and_then(|v| v.as_f64());
        let quantity_qualifier: Option<&str> = denial_data
            .get("quantity_qualifier")
            .and_then(|v| v.as_str());

        // A payer re-sending a remittance changes the production date, which
        // is part of idx_denials_natural_key; an active copy of the same
        // adjustment on the same claim line is never inserted again.
        let row = sqlx::query(
            "INSERT INTO denials \
             (claim_id, service_line_number, cpt_code, hcpcs_code, modifier_1, modifier_2, \
              charge_amount, payment_amount, adjustment_amount, cagc, carc_code, rarc_code, \
              adjustment_reason, denial_date, status, appeal_deadline, \
              reported_quantity, quantity_qualifier) \
             SELECT c.id, $2, $3, $4, $5, $6, $7::numeric, $8::numeric, $9::numeric, $10, $11, $12, \
                    $13, $14::date, 'open', \
                    appeal_deadline_for(c.payer_name, COALESCE($14::date, CURRENT_DATE)), \
                    $17::numeric, $18 \
             FROM claims c \
             WHERE c.claim_number = $1 AND c.organization_id = $15 \
               AND NOT EXISTS ( \
                   SELECT 1 FROM denials d \
                   WHERE d.claim_id = c.id \
                     AND d.service_line_number IS NOT DISTINCT FROM $2 \
                     AND d.cpt_code IS NOT DISTINCT FROM $3 \
                     AND d.cagc = $10 AND d.carc_code IS NOT DISTINCT FROM $11 \
                     AND d.charge_amount = $7::numeric AND d.adjustment_amount = $9::numeric \
                     AND d.status = ANY($16::text[])) \
             ON CONFLICT DO NOTHING \
             RETURNING id",
        )
        .bind(claim_id)
        .bind(service_line_number)
        .bind(cpt_code)
        .bind(hcpcs_code)
        .bind(modifier_1)
        .bind(modifier_2)
        .bind(charge_amount)
        .bind(payment_amount)
        .bind(adjustment_amount)
        .bind(cagc)
        .bind(carc_code)
        .bind(rarc_code)
        .bind(denial_reason)
        .bind(denial_date)
        .bind(organization_id)
        .bind(reprocessing::ACTIVE_DENIAL_STATUSES)
        .bind(reported_quantity)
        .bind(quantity_qualifier)
        .fetch_optional(&mut **tx)
        .await
        .map_err(AppError::Db)?;

        if row.is_some() {
            denials_written += 1;
        }
    }

    if !cleaned_denials.is_empty() {
        let claim_numbers: Vec<&str> = cleaned_denials
            .iter()
            .filter_map(|d| d.get("claim_id").and_then(|v| v.as_str()))
            .collect();
        sqlx::query(
            "UPDATE claims c \
             SET status = CASE WHEN c.total_paid > 0 THEN 'partially_paid' ELSE 'denied' END, \
                 updated_at = NOW() \
             WHERE c.claim_number = ANY($1::text[]) AND c.organization_id = $2 \
               AND EXISTS (SELECT 1 FROM denials d WHERE d.claim_id = c.id)",
        )
        .bind(claim_numbers)
        .bind(organization_id)
        .execute(&mut **tx)
        .await
        .map_err(AppError::Db)?;
    }

    let claims_reversed =
        reprocessing::mark_reversed(tx, organization_id, &reversed_numbers).await?;
    // After the claims, so a PLB reference can link to one stored just now.
    let provider_adjustments =
        provider_adjustments::store(tx, organization_id, ingestion_id, payment_info).await?;
    let found = overpayments::find(&claims, &prior_payments, &reversed_numbers);
    let overpayments = overpayments::record(tx, organization_id, ingestion_id, &found).await?;
    let settled =
        reprocessing::settle_paid_denials(tx, organization_id, &reprocessing::payments(&claims))
            .await?;

    Ok(Stored {
        claims: cleaned_claims.len(),
        denials: cleaned_denials.len(),
        denials_written,
        claims_reversed,
        provider_adjustments,
        overpayments,
        settled,
    })
}

async fn already_ingested(
    pool: &sqlx::PgPool,
    organization_id: Uuid,
    file_hash: &str,
) -> Result<Option<sqlx::postgres::PgRow>, AppError> {
    sqlx::query(
        "SELECT id, file_name, created_at, claims_count, denials_count \
         FROM ingestion_log \
         WHERE file_hash = $1 AND organization_id = $2 AND status IN ('completed', 'parsed') \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(file_hash)
    .bind(organization_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut different = 0u8;
    for (x, y) in a.iter().zip(b) {
        different |= x ^ y;
    }
    different == 0
}

/// Entry point for an S3-compatible bucket's event notification webhook (a
/// MinIO/Garage-style `Authorization: Bearer <token>` target, not the
/// user JWT or sibling-service key this gateway otherwise requires — see the
/// `PUBLIC_EXACT` note in `denial_auth::rbac`). Configuring this is optional;
/// without it, the ediparser's own polling loop still picks up new objects.
/// The payload is forwarded to the ediparser as-is; it alone knows which
/// bucket/prefix/extensions are actually configured for import.
pub async fn s3_event_webhook(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let expected = state
        .config
        .s3_event_webhook_token
        .as_deref()
        .ok_or(AppError::NotFound)?;
    let supplied = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match supplied {
        Some(token) if constant_time_eq(token.as_bytes(), expected.as_bytes()) => {}
        _ => return Err(AppError::Unauthorized),
    }

    let result = state.ediparser.notify_s3_event(&payload).await?;
    Ok(Json(result))
}

// ── Handlers ────────────────────────────────────────────────────────────

pub async fn upload_file(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, AppError> {
    let key = limit_key(&principal);
    if !state.ingestion_limiter.allow(&key) {
        let retry = state.ingestion_limiter.retry_after(&key);
        return Err(AppError::RateLimited { retry_after: retry });
    }

    let mut file_bytes: Vec<u8> = Vec::new();
    let mut filename: Option<String> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| AppError::BadRequest("Invalid multipart form data".into()))?
    {
        if filename.is_none() {
            if let Some(name) = field.file_name() {
                filename = Some(name.to_string());
            }
        }
        let chunk = field
            .bytes()
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        file_bytes.extend_from_slice(&chunk);
        if file_bytes.len() > MAX_FILE_SIZE {
            return Err(AppError::BadRequest("File too large: maximum 25 MB".into()));
        }
    }

    let fname = filename.unwrap_or_default();
    validate_extension(&fname)?;

    if file_bytes.is_empty() || !file_bytes.starts_with(b"ISA") {
        return Err(AppError::BadRequest(
            "File does not appear to be a valid X12 file (missing ISA segment)".into(),
        ));
    }

    let mut hasher = Sha256::new();
    hasher.update(&file_bytes);
    let file_hash = format!("{:x}", hasher.finalize());

    let result = state.ediparser.parse_file(&file_bytes).await?;

    Ok(Json(serde_json::json!({
        "file_name": fname,
        "file_hash": file_hash,
        "claims_parsed": result.get("claims_parsed").and_then(|v| v.as_i64()).unwrap_or(0),
        "denials_parsed": result.get("denials_parsed").and_then(|v| v.as_i64()).unwrap_or(0),
        "status": result.get("status").and_then(|v| v.as_str()).unwrap_or("error"),
    })))
}

pub async fn ingest_file(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<IngestQuery>,
    mut multipart: Multipart,
) -> Result<(axum::http::StatusCode, Json<serde_json::Value>), AppError> {
    let organization_id = organization_id(&principal, None)?;
    let key = limit_key(&principal);
    if !state.ingestion_limiter.allow(&key) {
        let retry = state.ingestion_limiter.retry_after(&key);
        return Err(AppError::RateLimited { retry_after: retry });
    }

    let mut file_bytes: Vec<u8> = Vec::new();
    let mut filename: Option<String> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| AppError::BadRequest("Invalid multipart form data".into()))?
    {
        if filename.is_none() {
            if let Some(name) = field.file_name() {
                filename = Some(name.to_string());
            }
        }
        let chunk = field
            .bytes()
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        file_bytes.extend_from_slice(&chunk);
        if file_bytes.len() > MAX_FILE_SIZE {
            return Err(AppError::BadRequest("File too large: maximum 25 MB".into()));
        }
    }

    let fname = filename.unwrap_or_default();
    validate_extension(&fname)?;

    if file_bytes.is_empty() || !file_bytes.starts_with(b"ISA") {
        return Err(AppError::BadRequest(
            "File does not appear to be a valid X12 file (missing ISA segment)".into(),
        ));
    }

    let mut hasher = Sha256::new();
    hasher.update(&file_bytes);
    let file_hash = format!("{:x}", hasher.finalize());
    let file_size = file_bytes.len() as i64;
    let pool = &state.pool;

    if !params.force {
        if let Some(previous) = already_ingested(pool, organization_id, &file_hash).await? {
            let created_at: Option<chrono::DateTime<Utc>> = previous.get("created_at");
            let when = created_at
                .map(|dt| dt.format("%d %b %Y at %H:%M").to_string())
                .unwrap_or_else(|| "unknown".into());
            let file_name: String = previous.get("file_name");
            // INTEGER and nullable: reading them as i64 panicked and dropped
            // the connection instead of returning this 409.
            let count = |column: &str| {
                previous
                    .try_get::<Option<i32>, _>(column)
                    .ok()
                    .flatten()
                    .unwrap_or(0)
            };
            let claims_count = count("claims_count");
            let denials_count = count("denials_count");
            return Err(AppError::Conflict(format!(
                "This exact file was already ingested as '{file_name}' on {when} \
                 ({claims_count} claims, {denials_count} denials). \
                 Re-ingesting it would duplicate those denials. \
                 Send force=true if you intend to load it again anyway."
            )));
        }
    }

    let result = match state.ediparser.parse_file(&file_bytes).await {
        Ok(r) => r,
        Err(e) => {
            let _ = sqlx::query(
                "INSERT INTO ingestion_log (organization_id, file_name, file_size_bytes, file_hash, status, errors) \
                 VALUES ($1, $2, $3, $4, 'error', $5::jsonb)",
            )
            .bind(organization_id)
            .bind(&fname)
            .bind(file_size)
            .bind(&file_hash)
            .bind(serde_json::json!({ "error": e.to_string() }))
            .execute(pool)
            .await;
            return Err(AppError::BadRequest(format!("Parse failed: {e}")));
        }
    };

    let claims: Vec<serde_json::Value> = result
        .get("claims")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let denials: Vec<serde_json::Value> = result
        .get("denials")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let transaction_type: Option<String> = result
        .get("transaction_type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // A remittance can carry only PLB lines (a payment that is purely a
    // recoupment), so an empty claim list alone is not "nothing to store".
    let has_provider_adjustments = result
        .pointer("/payment_info/provider_adjustments")
        .and_then(|v| v.as_array())
        .is_some_and(|lines| !lines.is_empty());
    if claims.is_empty() && denials.is_empty() && !has_provider_adjustments {
        let _ = sqlx::query(
            "INSERT INTO ingestion_log \
             (organization_id, file_name, file_size_bytes, file_hash, status, claims_count, denials_count) \
             VALUES ($1, $2, $3, $4, 'completed', 0, 0)",
        )
        .bind(organization_id)
        .bind(&fname)
        .bind(file_size)
        .bind(&file_hash)
        .execute(pool)
        .await;
        return Ok((
            axum::http::StatusCode::CREATED,
            Json(serde_json::json!({
                "status": "stored",
                "file_name": fname,
                "claims_stored": 0,
                "denials_stored": 0,
            })),
        ));
    }

    // Transaction: ingestion log + claims + denials + status update
    let mut tx = pool.begin().await.map_err(AppError::Db)?;

    let ingestion_row = sqlx::query(
        "INSERT INTO ingestion_log \
         (organization_id, file_name, file_size_bytes, file_hash, status, claims_count, denials_count, raw_response) \
         VALUES ($1, $2, $3, $4, 'completed', $5, $6, $7::jsonb) \
         RETURNING id",
    )
    .bind(organization_id)
    .bind(&fname)
    .bind(file_size)
    .bind(&file_hash)
    .bind(claims.len() as i64)
    .bind(denials.len() as i64)
    .bind(&result)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::Db)?;

    let ingestion_id: Uuid = ingestion_row.get("id");

    let payment_info = result.get("payment_info").cloned();
    let stored = store_remittance(
        &mut tx,
        organization_id,
        ingestion_id,
        transaction_type.as_deref(),
        payment_info.as_ref(),
        &claims,
        &denials,
    )
    .await?;
    tx.commit().await.map_err(AppError::Db)?;
    reprocessing::audit_settled(pool, organization_id, &fname, &stored.settled).await;

    Ok((
        axum::http::StatusCode::CREATED,
        Json(serde_json::json!({
            "status": "stored",
            "ingestion_id": ingestion_id.to_string(),
            "file_name": fname,
            "claims_stored": stored.claims,
            "denials_stored": stored.denials_written,
            "denials_skipped_as_duplicates": stored.denials as i64 - stored.denials_written,
            "claims_reversed": stored.claims_reversed,
            "denials_settled": stored.settled.len(),
            "provider_adjustments_stored": stored.provider_adjustments,
            "overpayments_identified": stored.overpayments,
        })),
    ))
}

#[derive(Deserialize)]
pub struct IngestQuery {
    #[serde(default)]
    pub force: bool,
}

pub async fn list_ingestion_log(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<ListIngestionLogQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let organization_id = organization_id(&principal, None)?;
    let limit = params.limit.clamp(1, 1000);
    let rows = sqlx::query(
        "SELECT id, file_name, file_size_bytes, file_hash, status, claims_count, denials_count, created_at, completed_at \
         FROM ingestion_log WHERE organization_id = $1 ORDER BY created_at DESC LIMIT $2",
    )
    .bind(organization_id)
    .bind(limit)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    let values: Vec<serde_json::Value> = rows.iter().map(row_to_json).collect();
    Ok(Json(values))
}

pub async fn get_ingestion_history(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<IngestHistoryQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let organization_id = organization_id(&principal, None)?;
    let limit = params.limit.clamp(1, 500);
    let rows = sqlx::query(
        "SELECT id, file_name, file_size_bytes, file_hash, status, claims_count, denials_count, created_at, completed_at \
         FROM ingestion_log WHERE organization_id = $1 ORDER BY created_at DESC LIMIT $2",
    )
    .bind(organization_id)
    .bind(limit)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    let values: Vec<serde_json::Value> = rows.iter().map(row_to_json).collect();
    Ok(Json(values))
}

pub async fn store_parsed_data(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(body): Json<StoreIngestion>,
) -> Result<Json<serde_json::Value>, AppError> {
    let pool = &state.pool;
    let organization_id = organization_id(&principal, body.organization_id)?;

    if let Some(previous) = already_ingested(pool, organization_id, &body.file_hash).await? {
        let id: Uuid = previous.get("id");
        let created_at: Option<chrono::DateTime<Utc>> = previous.get("created_at");
        let when = created_at
            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "unknown".into());
        tracing::info!(
            "Skipping {}: identical to ingest {} ({})",
            body.file_name,
            id,
            when
        );
        return Ok(Json(serde_json::json!({
            "status": "skipped_duplicate",
            "file_name": body.file_name,
            "previous_ingestion_id": id.to_string(),
            "claims_stored": 0,
            "denials_stored": 0,
        })));
    }

    let mut tx = pool.begin().await.map_err(AppError::Db)?;

    let ingestion_row = sqlx::query(
        "INSERT INTO ingestion_log \
         (organization_id, file_name, file_path, file_size_bytes, file_hash, status, claims_count, denials_count) \
         VALUES ($1, $2, $3, $4, $5, 'completed', $6, $7) \
         RETURNING id",
    )
    .bind(organization_id)
    .bind(&body.file_name)
    .bind(&body.file_path)
    .bind(body.file_size)
    .bind(&body.file_hash)
    .bind(body.claims.len() as i64)
    .bind(body.denials.len() as i64)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::Db)?;

    let ingestion_id: Uuid = ingestion_row.get("id");

    let stored = store_remittance(
        &mut tx,
        organization_id,
        ingestion_id,
        body.transaction_type.as_deref(),
        body.payment_info.as_ref(),
        &body.claims,
        &body.denials,
    )
    .await?;
    tx.commit().await.map_err(AppError::Db)?;
    reprocessing::audit_settled(pool, organization_id, &body.file_name, &stored.settled).await;

    Ok(Json(serde_json::json!({
        "status": "stored",
        "ingestion_id": ingestion_id.to_string(),
        "file_name": body.file_name,
        "claims_stored": stored.claims,
        "denials_stored": stored.denials_written,
        "denials_skipped_as_duplicates": stored.denials as i64 - stored.denials_written,
        "claims_reversed": stored.claims_reversed,
        "denials_settled": stored.settled.len(),
        "provider_adjustments_stored": stored.provider_adjustments,
        "overpayments_identified": stored.overpayments,
    })))
}

// ── Router ──────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/upload", post(upload_file))
        .route("/ingest", post(ingest_file))
        .route("/s3-events", post(s3_event_webhook))
        .route("/log", post(list_ingestion_log))
        .route("/history", get(get_ingestion_history))
        .route("/store", post(store_parsed_data))
        .route("/provider-adjustments", get(provider_adjustments::list))
        .route(
            "/provider-adjustments/summary",
            get(provider_adjustments::summary),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_only_identical_tokens() {
        assert!(constant_time_eq(b"webhook-secret", b"webhook-secret"));
        assert!(!constant_time_eq(b"webhook-secret", b"wrong-secret"));
        assert!(!constant_time_eq(b"short", b"much-longer-value"));
    }

    fn fields<'a>(
        svc_from: Option<&'a str>,
        svc_to: Option<&'a str>,
        total_charge: f64,
        provider_npi: Option<&'a str>,
        payer_name: Option<&'a str>,
    ) -> MatchFields<'a> {
        MatchFields {
            svc_from,
            svc_to,
            total_charge,
            provider_npi,
            payer_name,
        }
    }

    #[test]
    fn claim_number_alone_is_not_enough() {
        // The 837 shares the claim number but nothing else: score stays at the
        // 0.5 base, below the 0.6 threshold, so it must NOT merge.
        let incoming = fields(None, None, 0.0, None, None);
        let existing = fields(None, None, 0.0, None, None);
        assert_eq!(correlation_score(&incoming, &existing), 0.5);
        assert!(correlation_score(&incoming, &existing) < CORRELATION_MATCH_THRESHOLD);
    }

    #[test]
    fn corroborated_match_reaches_threshold() {
        // Claim number + matching billed amount (0.5 + 0.15 = 0.65) clears the
        // threshold and should merge.
        let incoming = fields(None, None, 150.0, None, None);
        let existing = fields(None, None, 150.0, None, None);
        assert!(correlation_score(&incoming, &existing) >= CORRELATION_MATCH_THRESHOLD);
    }

    #[test]
    fn full_match_scores_one() {
        let incoming = fields(
            Some("2026-01-01"),
            Some("2026-01-08"),
            150.0,
            Some("1234567890"),
            Some("Aetna"),
        );
        let existing = fields(
            Some("2026-01-01"),
            Some("2026-01-08"),
            150.0,
            Some("1234567890"),
            Some("aetna"),
        );
        assert!((correlation_score(&incoming, &existing) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn zero_amount_is_not_evidence() {
        // A 837 with no billed amount (0) must not earn the amount points just
        // because the existing claim is also 0.
        let incoming = fields(None, None, 0.0, None, None);
        let existing = fields(None, None, 0.0, None, None);
        assert_eq!(correlation_score(&incoming, &existing), 0.5);
    }

    #[test]
    fn mismatched_fields_do_not_score() {
        let incoming = fields(
            Some("2026-01-01"),
            Some("2026-01-08"),
            150.0,
            Some("111"),
            Some("Aetna"),
        );
        let existing = fields(
            Some("2026-02-01"),
            Some("2026-02-08"),
            999.0,
            Some("222"),
            Some("Cigna"),
        );
        // Only the base claim-number score; every field differs.
        assert_eq!(correlation_score(&incoming, &existing), 0.5);
    }
}
