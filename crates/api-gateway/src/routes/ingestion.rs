//! Ingestion routes: 835/837 EDI file upload, parse, and store.

use std::collections::HashSet;

use axum::extract::{Multipart, Query, State};
use axum::routing::{get, post};
use axum::Extension;
use axum::Json;
use axum::Router;
use chrono::{NaiveDate, Utc};
use denial_common::error::AppError;
use denial_common::rbac::{Principal, PrincipalKind};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::{Column, Row};
use uuid::Uuid;

use crate::state::AppState;

const MAX_FILE_SIZE: usize = 25 * 1024 * 1024;

// ── Models ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct StoreIngestion {
    pub file_name: String,
    pub file_hash: String,
    pub file_size: i64,
    pub claims: Vec<serde_json::Value>,
    pub denials: Vec<serde_json::Value>,
    #[serde(default)]
    pub transaction_type: Option<String>,
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

fn validate_extension(filename: &str) -> Result<(), AppError> {
    if filename.is_empty() {
        return Err(AppError::BadRequest("No file name provided".into()));
    }
    let ext = filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_lowercase();
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
            .map(|v| v.map(serde_json::Value::String).unwrap_or(serde_json::Value::Null))
            .or_else(|_| {
                row.try_get::<Option<i64>, _>(name)
                    .map(|v| v.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null))
            })
            .or_else(|_| {
                row.try_get::<Option<f64>, _>(name)
                    .map(|v| v.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null))
            })
            .or_else(|_| {
                row.try_get::<Option<bool>, _>(name)
                    .map(|v| v.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null))
            })
            .or_else(|_| {
                row.try_get::<Option<Uuid>, _>(name)
                    .map(|v| v.map(|v| serde_json::Value::String(v.to_string())).unwrap_or(serde_json::Value::Null))
            })
            .or_else(|_| {
                row.try_get::<Option<NaiveDate>, _>(name)
                    .map(|v| v.map(|v| serde_json::Value::String(v.to_string())).unwrap_or(serde_json::Value::Null))
            })
            .or_else(|_| {
                row.try_get::<Option<chrono::DateTime<Utc>>, _>(name)
                    .map(|v| v.map(|v| serde_json::Value::String(v.to_rfc3339())).unwrap_or(serde_json::Value::Null))
            })
            .unwrap_or(serde_json::Value::Null);
        map.insert(name.to_string(), val);
    }
    serde_json::Value::Object(map)
}

fn clean_claim(
    claim: &serde_json::Value,
    seen_numbers: &mut HashSet<String>,
) -> serde_json::Value {
    let raw_id = str_from_json(claim.get("claim_id").unwrap_or(&serde_json::Value::Null)).unwrap_or_default();
    let patient_id = str_from_json(claim.get("patient_id").unwrap_or(&serde_json::Value::Null)).unwrap_or_default();
    let dob_raw = str_from_json(claim.get("date_of_birth").unwrap_or(&serde_json::Value::Null));
    let dob = dob_raw.as_deref().and_then(parse_date);
    let patient_name = claim.get("patient_name").cloned();
    let provider_npi = claim.get("provider_npi").cloned();
    let provider_name = claim.get("provider_name").cloned();
    let payer_name = claim.get("payer_name").cloned();
    let payer_id_number = claim.get("payer_id_number").cloned();
    let total_charged = f64_from_json(claim.get("total_charged").unwrap_or(&serde_json::Value::Null));
    let total_paid = f64_from_json(claim.get("total_paid").unwrap_or(&serde_json::Value::Null));
    let total_adjustment = f64_from_json(claim.get("total_adjustment").unwrap_or(&serde_json::Value::Null));
    let claim_type = str_from_json(claim.get("claim_type").unwrap_or(&serde_json::Value::Null)).unwrap_or_else(|| "professional".into());
    let service_from = str_from_json(claim.get("service_from").unwrap_or(&serde_json::Value::Null)).and_then(|s| parse_date(&s));
    let service_to = str_from_json(claim.get("service_to").unwrap_or(&serde_json::Value::Null)).and_then(|s| parse_date(&s));
    let diagnosis_codes = vec_str_from_json(claim.get("diagnosis_codes").unwrap_or(&serde_json::Value::Null));

    let base = if dob.is_some() {
        format!("{}-{}", patient_id, dob.unwrap())
    } else {
        format!("{patient_id}-NODOB")
    };
    let mut claim_number = if raw_id.is_empty() { base.clone() } else { raw_id.clone() };

    if seen_numbers.contains(&claim_number) {
        let suffix = Uuid::new_v4().to_string().chars().take(8).collect::<String>();
        let id_part = if raw_id.is_empty() { base } else { raw_id.clone() };
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
        "service_from": service_from.map(|d| d.to_string()).unwrap_or_default(),
        "service_to": service_to.map(|d| d.to_string()).unwrap_or_default(),
        "diagnosis_codes": diagnosis_codes,
    })
}

fn clean_denial(denial: &serde_json::Value) -> serde_json::Value {
    let claim_id = str_from_json(denial.get("claim_id").unwrap_or(&serde_json::Value::Null)).unwrap_or_default();
    let service_line_number = denial.get("service_line_number").and_then(|v| v.as_i64());
    let cpt_code = str_from_json(denial.get("cpt_code").unwrap_or(&serde_json::Value::Null));
    let hcpcs_code = str_from_json(denial.get("hcpcs_code").unwrap_or(&serde_json::Value::Null));
    let modifier_1 = str_from_json(denial.get("modifier_1").unwrap_or(&serde_json::Value::Null));
    let modifier_2 = str_from_json(denial.get("modifier_2").unwrap_or(&serde_json::Value::Null));
    let charge_amount = f64_from_json(denial.get("charge_amount").unwrap_or(&serde_json::Value::Null));
    let payment_amount = f64_from_json(denial.get("payment_amount").unwrap_or(&serde_json::Value::Null));
    let adjustment_amount = f64_from_json(denial.get("adjustment_amount").unwrap_or(&serde_json::Value::Null));
    let cagc = str_from_json(denial.get("cagc").unwrap_or(&serde_json::Value::Null));
    let carc_code = str_from_json(denial.get("carc_code").unwrap_or(&serde_json::Value::Null));
    let rarc_code = str_from_json(denial.get("rarc_code").unwrap_or(&serde_json::Value::Null));
    let denial_reason = str_from_json(denial.get("denial_reason").unwrap_or(&serde_json::Value::Null))
        .or_else(|| str_from_json(denial.get("denial_reason_code").unwrap_or(&serde_json::Value::Null)));
    let denial_date = str_from_json(denial.get("denial_date").unwrap_or(&serde_json::Value::Null))
        .and_then(|s| parse_date(&s));

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
    })
}

// ── Claims upsert SQL ──────────────────────────────────────────────────

const CLAIM_INSERT: &str = "INSERT INTO claims \
    (claim_number, patient_id, patient_name, date_of_birth, provider_npi, provider_name, \
     payer_name, payer_id_number, total_charge, total_paid, total_adjustment, \
     status, claim_type, service_from, service_to, icd_10_codes, raw_835_data, parsed_at) \
    VALUES ($1, $2, $3, $4::date, $5, $6, $7, $8, $9, $10, $11, \
            'parsed', $12, $13::date, $14::date, $15::text[], $16, NOW())";

const ON_CONFLICT_835: &str = "ON CONFLICT (claim_number) DO UPDATE SET \
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

const ON_CONFLICT_837: &str = "ON CONFLICT (claim_number) DO UPDATE SET \
    patient_name     = COALESCE(claims.patient_name, EXCLUDED.patient_name), \
    date_of_birth    = COALESCE(claims.date_of_birth, EXCLUDED.date_of_birth), \
    provider_npi     = COALESCE(claims.provider_npi, EXCLUDED.provider_npi), \
    provider_name    = COALESCE(claims.provider_name, EXCLUDED.provider_name), \
    payer_name       = COALESCE(claims.payer_name, EXCLUDED.payer_name), \
    payer_id_number  = COALESCE(claims.payer_id_number, EXCLUDED.payer_id_number), \
    total_charge     = CASE WHEN claims.total_charge = 0 \
                            THEN EXCLUDED.total_charge ELSE claims.total_charge END, \
    claim_type       = EXCLUDED.claim_type, \
    service_from     = COALESCE(claims.service_from, EXCLUDED.service_from), \
    service_to       = COALESCE(claims.service_to, EXCLUDED.service_to), \
    icd_10_codes     = CASE \
                        WHEN EXCLUDED.icd_10_codes IS NOT NULL \
                         AND cardinality(EXCLUDED.icd_10_codes) > 0 \
                        THEN EXCLUDED.icd_10_codes ELSE claims.icd_10_codes END, \
    updated_at       = NOW()";

fn claim_upsert_sql(transaction_type: Option<&str>) -> String {
    let conflict = if transaction_type == Some("837") {
        ON_CONFLICT_837
    } else {
        ON_CONFLICT_835
    };
    format!("{CLAIM_INSERT} {conflict}")
}

async fn already_ingested(pool: &sqlx::PgPool, file_hash: &str) -> Result<Option<sqlx::postgres::PgRow>, AppError> {
    sqlx::query(
        "SELECT id, file_name, created_at, claims_count, denials_count \
         FROM ingestion_log \
         WHERE file_hash = $1 AND status IN ('completed', 'parsed') \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(file_hash)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)
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
    while let Some(field) = multipart.next_field().await.map_err(|_| {
        AppError::BadRequest("Invalid multipart form data".into())
    })? {
        if filename.is_none() {
            if let Some(name) = field.file_name() {
                filename = Some(name.to_string());
            }
        }
        let mut chunk = field.bytes().await.map_err(|e| AppError::Internal(e.to_string()))?;
        file_bytes.extend_from_slice(&chunk);
        if file_bytes.len() > MAX_FILE_SIZE {
            return Err(AppError::BadRequest(
                "File too large: maximum 25 MB".into(),
            ));
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
    let key = limit_key(&principal);
    if !state.ingestion_limiter.allow(&key) {
        let retry = state.ingestion_limiter.retry_after(&key);
        return Err(AppError::RateLimited { retry_after: retry });
    }

    let mut file_bytes: Vec<u8> = Vec::new();
    let mut filename: Option<String> = None;
    while let Some(field) = multipart.next_field().await.map_err(|_| {
        AppError::BadRequest("Invalid multipart form data".into())
    })? {
        if filename.is_none() {
            if let Some(name) = field.file_name() {
                filename = Some(name.to_string());
            }
        }
        let chunk = field.bytes().await.map_err(|e| AppError::Internal(e.to_string()))?;
        file_bytes.extend_from_slice(&chunk);
        if file_bytes.len() > MAX_FILE_SIZE {
            return Err(AppError::BadRequest(
                "File too large: maximum 25 MB".into(),
            ));
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
        if let Some(previous) = already_ingested(pool, &file_hash).await? {
            let created_at: Option<chrono::DateTime<Utc>> = previous.get("created_at");
            let when = created_at
                .map(|dt| dt.format("%d %b %Y at %H:%M").to_string())
                .unwrap_or_else(|| "unknown".into());
            let file_name: String = previous.get("file_name");
            let claims_count: i64 = previous.get("claims_count");
            let denials_count: i64 = previous.get("denials_count");
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
                "INSERT INTO ingestion_log (file_name, file_size_bytes, file_hash, status, errors) \
                 VALUES ($1, $2, $3, 'error', $4::jsonb)",
            )
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

    if claims.is_empty() && denials.is_empty() {
        let _ = sqlx::query(
            "INSERT INTO ingestion_log \
             (file_name, file_size_bytes, file_hash, status, claims_count, denials_count) \
             VALUES ($1, $2, $3, 'completed', 0, 0)",
        )
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
         (file_name, file_size_bytes, file_hash, status, claims_count, denials_count, raw_response) \
         VALUES ($1, $2, $3, 'completed', $4, $5, $6::jsonb) \
         RETURNING id",
    )
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

    let mut seen_numbers = HashSet::new();
    let cleaned_claims: Vec<serde_json::Value> =
        claims.iter().map(|c| clean_claim(c, &mut seen_numbers)).collect();
    let cleaned_denials: Vec<serde_json::Value> =
        denials.iter().map(|d| clean_denial(d)).collect();

    let upsert_sql = claim_upsert_sql(transaction_type.as_deref());

    for claim_data in &cleaned_claims {
        let claim_id: &str = claim_data.get("claim_id").and_then(|v| v.as_str()).unwrap_or("");
        let patient_id: &str = claim_data.get("patient_id").and_then(|v| v.as_str()).unwrap_or("");
        let patient_name: Option<&str> = claim_data.get("patient_name").and_then(|v| v.as_str());
        let dob: Option<&str> = claim_data
            .get("date_of_birth")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let provider_npi: Option<&str> = claim_data.get("provider_npi").and_then(|v| v.as_str());
        let provider_name: Option<&str> = claim_data.get("provider_name").and_then(|v| v.as_str());
        let payer_name: Option<&str> = claim_data.get("payer_name").and_then(|v| v.as_str());
        let payer_id_number: Option<&str> = claim_data.get("payer_id_number").and_then(|v| v.as_str());
        let total_charge: f64 = claim_data.get("total_charged").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let total_paid: f64 = claim_data.get("total_paid").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let total_adjustment: f64 = claim_data.get("total_adjustment").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let claim_type: &str = claim_data.get("claim_type").and_then(|v| v.as_str()).unwrap_or("professional");
        let service_from: Option<&str> = claim_data
            .get("service_from")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let service_to: Option<&str> = claim_data
            .get("service_to")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let icd_codes: Vec<String> = {
            let arr = claim_data
                .get("diagnosis_codes")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        };

        sqlx::query(&upsert_sql)
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
            .bind(service_from)
            .bind(service_to)
            .bind(icd_codes)
            .bind(claim_data.to_string())
            .execute(&mut *tx)
            .await
            .map_err(AppError::Db)?;
    }

    let mut denials_written: i64 = 0;
    for denial_data in &cleaned_denials {
        let claim_id: &str = denial_data.get("claim_id").and_then(|v| v.as_str()).unwrap_or("");
        let service_line_number: Option<i64> = denial_data.get("service_line_number").and_then(|v| v.as_i64());
        let cpt_code: Option<&str> = denial_data.get("cpt_code").and_then(|v| v.as_str());
        let hcpcs_code: Option<&str> = denial_data.get("hcpcs_code").and_then(|v| v.as_str());
        let modifier_1: Option<&str> = denial_data.get("modifier_1").and_then(|v| v.as_str());
        let modifier_2: Option<&str> = denial_data.get("modifier_2").and_then(|v| v.as_str());
        let charge_amount: f64 = denial_data.get("charge_amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let payment_amount: f64 = denial_data.get("payment_amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let adjustment_amount: f64 = denial_data.get("adjustment_amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let cagc: Option<&str> = denial_data.get("cagc").and_then(|v| v.as_str());
        let carc_code: Option<&str> = denial_data.get("carc_code").and_then(|v| v.as_str());
        let rarc_code: Option<&str> = denial_data.get("rarc_code").and_then(|v| v.as_str());
        let denial_reason: Option<&str> = denial_data.get("denial_reason").and_then(|v| v.as_str());
        let denial_date: Option<&str> = denial_data
            .get("denial_date")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        let row = sqlx::query(
            "INSERT INTO denials \
             (claim_id, service_line_number, cpt_code, hcpcs_code, modifier_1, modifier_2, \
              charge_amount, payment_amount, adjustment_amount, cagc, carc_code, rarc_code, \
              adjustment_reason, denial_date, status, appeal_deadline) \
             VALUES ((SELECT id FROM claims WHERE claim_number = $1), $2, $3, $4, $5, $6, \
                     $7, $8, $9, $10, $11, $12, $13, $14::date, 'open', \
                     appeal_deadline_for( \
                         (SELECT payer_name FROM claims WHERE claim_number = $1), \
                         COALESCE($14::date, CURRENT_DATE))) \
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
        .fetch_optional(&mut *tx)
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
             WHERE c.claim_number = ANY($1::text[]) \
               AND EXISTS (SELECT 1 FROM denials d WHERE d.claim_id = c.id)",
        )
        .bind(claim_numbers)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Db)?;
    }

    tx.commit().await.map_err(AppError::Db)?;

    Ok((
        axum::http::StatusCode::CREATED,
        Json(serde_json::json!({
            "status": "stored",
            "ingestion_id": ingestion_id.to_string(),
            "file_name": fname,
            "claims_stored": cleaned_claims.len(),
            "denials_stored": denials_written,
            "denials_skipped_as_duplicates": cleaned_denials.len() as i64 - denials_written,
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
    Query(params): Query<ListIngestionLogQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let limit = params.limit.clamp(1, 1000);
    let rows = sqlx::query(
        "SELECT id, file_name, file_size_bytes, file_hash, status, claims_count, denials_count, created_at, completed_at \
         FROM ingestion_log ORDER BY created_at DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    let values: Vec<serde_json::Value> = rows.iter().map(row_to_json).collect();
    Ok(Json(values))
}

pub async fn get_ingestion_history(
    State(state): State<AppState>,
    Query(params): Query<IngestHistoryQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let limit = params.limit.clamp(1, 500);
    let rows = sqlx::query(
        "SELECT id, file_name, file_size_bytes, file_hash, status, claims_count, denials_count, created_at, completed_at \
         FROM ingestion_log ORDER BY created_at DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    let values: Vec<serde_json::Value> = rows.iter().map(row_to_json).collect();
    Ok(Json(values))
}

pub async fn store_parsed_data(
    State(state): State<AppState>,
    Json(body): Json<StoreIngestion>,
) -> Result<Json<serde_json::Value>, AppError> {
    let pool = &state.pool;

    if let Some(previous) = already_ingested(pool, &body.file_hash).await? {
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

    let mut seen_numbers = HashSet::new();
    let cleaned_claims: Vec<serde_json::Value> =
        body.claims.iter().map(|c| clean_claim(c, &mut seen_numbers)).collect();
    let cleaned_denials: Vec<serde_json::Value> =
        body.denials.iter().map(|d| clean_denial(d)).collect();

    let mut tx = pool.begin().await.map_err(AppError::Db)?;

    let ingestion_row = sqlx::query(
        "INSERT INTO ingestion_log \
         (file_name, file_size_bytes, file_hash, status, claims_count, denials_count) \
         VALUES ($1, $2, $3, 'completed', $4, $5) \
         RETURNING id",
    )
    .bind(&body.file_name)
    .bind(body.file_size)
    .bind(&body.file_hash)
    .bind(cleaned_claims.len() as i64)
    .bind(cleaned_denials.len() as i64)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::Db)?;

    let ingestion_id: Uuid = ingestion_row.get("id");

    let upsert_sql = claim_upsert_sql(body.transaction_type.as_deref());

    for claim_data in &cleaned_claims {
        let claim_id: &str = claim_data.get("claim_id").and_then(|v| v.as_str()).unwrap_or("");
        let patient_id: &str = claim_data.get("patient_id").and_then(|v| v.as_str()).unwrap_or("");
        let patient_name: Option<&str> = claim_data.get("patient_name").and_then(|v| v.as_str());
        let dob: Option<&str> = claim_data
            .get("date_of_birth")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let provider_npi: Option<&str> = claim_data.get("provider_npi").and_then(|v| v.as_str());
        let provider_name: Option<&str> = claim_data.get("provider_name").and_then(|v| v.as_str());
        let payer_name: Option<&str> = claim_data.get("payer_name").and_then(|v| v.as_str());
        let payer_id_number: Option<&str> = claim_data.get("payer_id_number").and_then(|v| v.as_str());
        let total_charge: f64 = claim_data.get("total_charged").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let total_paid: f64 = claim_data.get("total_paid").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let total_adjustment: f64 = claim_data.get("total_adjustment").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let claim_type: &str = claim_data.get("claim_type").and_then(|v| v.as_str()).unwrap_or("professional");
        let service_from: Option<&str> = claim_data
            .get("service_from")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let service_to: Option<&str> = claim_data
            .get("service_to")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let icd_codes: Vec<String> = {
            let arr = claim_data
                .get("diagnosis_codes")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        };

        sqlx::query(&upsert_sql)
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
            .bind(service_from)
            .bind(service_to)
            .bind(icd_codes)
            .bind(claim_data.to_string())
            .execute(&mut *tx)
            .await
            .map_err(AppError::Db)?;
    }

    let mut denials_written: i64 = 0;
    for denial_data in &cleaned_denials {
        let claim_id: &str = denial_data.get("claim_id").and_then(|v| v.as_str()).unwrap_or("");
        let service_line_number: Option<i64> = denial_data.get("service_line_number").and_then(|v| v.as_i64());
        let cpt_code: Option<&str> = denial_data.get("cpt_code").and_then(|v| v.as_str());
        let hcpcs_code: Option<&str> = denial_data.get("hcpcs_code").and_then(|v| v.as_str());
        let modifier_1: Option<&str> = denial_data.get("modifier_1").and_then(|v| v.as_str());
        let modifier_2: Option<&str> = denial_data.get("modifier_2").and_then(|v| v.as_str());
        let charge_amount: f64 = denial_data.get("charge_amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let payment_amount: f64 = denial_data.get("payment_amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let adjustment_amount: f64 = denial_data.get("adjustment_amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let cagc: Option<&str> = denial_data.get("cagc").and_then(|v| v.as_str());
        let carc_code: Option<&str> = denial_data.get("carc_code").and_then(|v| v.as_str());
        let rarc_code: Option<&str> = denial_data.get("rarc_code").and_then(|v| v.as_str());
        let denial_reason: Option<&str> = denial_data.get("denial_reason").and_then(|v| v.as_str());
        let denial_date: Option<&str> = denial_data
            .get("denial_date")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        let row = sqlx::query(
            "INSERT INTO denials \
             (claim_id, service_line_number, cpt_code, hcpcs_code, modifier_1, modifier_2, \
              charge_amount, payment_amount, adjustment_amount, cagc, carc_code, rarc_code, \
              adjustment_reason, denial_date, status, appeal_deadline) \
             VALUES ((SELECT id FROM claims WHERE claim_number = $1), $2, $3, $4, $5, $6, \
                     $7, $8, $9, $10, $11, $12, $13, $14::date, 'open', \
                     appeal_deadline_for( \
                         (SELECT payer_name FROM claims WHERE claim_number = $1), \
                         COALESCE($14::date, CURRENT_DATE))) \
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
        .fetch_optional(&mut *tx)
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
             WHERE c.claim_number = ANY($1::text[]) \
               AND EXISTS (SELECT 1 FROM denials d WHERE d.claim_id = c.id)",
        )
        .bind(claim_numbers)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Db)?;
    }

    tx.commit().await.map_err(AppError::Db)?;

    Ok(Json(serde_json::json!({
        "status": "stored",
        "ingestion_id": ingestion_id.to_string(),
        "file_name": body.file_name,
        "claims_stored": cleaned_claims.len(),
        "denials_stored": denials_written,
        "denials_skipped_as_duplicates": cleaned_denials.len() as i64 - denials_written,
    })))
}

// ── Router ──────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/upload", post(upload_file))
        .route("/ingest", post(ingest_file))
        .route("/log", post(list_ingestion_log))
        .route("/history", get(get_ingestion_history))
        .route("/store", post(store_parsed_data))
}
