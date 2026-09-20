//! Reference-list routes: six code lists (CARC, RARC, ICD-10, CPT, HCPCS,
//! modifiers). CSV import with preview/apply, search, and deletion.

use std::collections::{BTreeSet, HashMap, HashSet};

use axum::extract::{Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Extension;
use axum::Json;
use axum::Router;
use chrono::{NaiveDate, Utc};
use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::postgres::Postgres;
use sqlx::{QueryBuilder, Row};

use crate::state::AppState;

// ── Kind metadata ─────────────────────────────────────────────────────────

fn table_for(kind: &str) -> Result<&'static str, AppError> {
    match kind {
        "carc" => Ok("carc_codes"),
        "rarc" => Ok("rarc_codes"),
        "icd10" => Ok("icd10_codes"),
        "cpt" => Ok("cpt_codes"),
        "modifier" => Ok("modifier_codes"),
        "hcpcs" => Ok("hcpcs_codes"),
        _ => Err(AppError::Unprocessable(format!(
            "Invalid reference kind: {kind}. Must be one of: carc, rarc, icd10, cpt, modifier, hcpcs"
        ))),
    }
}

fn extra_column(kind: &str) -> Option<&'static str> {
    match kind {
        "carc" => Some("category"),
        "rarc" => Some("applicable_cagc"),
        _ => None,
    }
}

fn max_bytes(kind: &str) -> Result<usize, AppError> {
    match kind {
        "carc" | "rarc" | "modifier" => Ok(1 << 20),
        "icd10" | "cpt" | "hcpcs" => Ok(50 << 20),
        _ => Err(AppError::Unprocessable(format!(
            "Invalid reference kind: {kind}"
        ))),
    }
}

fn positional_fields(kind: &str) -> &'static [&'static str] {
    match kind {
        "carc" => &[
            "code",
            "description",
            "category",
            "is_active",
            "effective_date",
            "expiration_date",
        ],
        "rarc" => &[
            "code",
            "description",
            "applicable_cagc",
            "is_active",
            "effective_date",
            "expiration_date",
        ],
        _ => &[
            "code",
            "description",
            "is_active",
            "effective_date",
            "expiration_date",
        ],
    }
}

fn alias_field(header: &str) -> Option<&'static str> {
    match header {
        "code"
        | "carc"
        | "carc code"
        | "carc_code"
        | "rarc"
        | "rarc code"
        | "rarc_code"
        | "code #"
        | "icd10code"
        | "icd10 code"
        | "icd 10 code"
        | "icd10"
        | "diagnosis code"
        | "diagnosable code"
        | "cptcode"
        | "cpt code"
        | "cpt"
        | "procedure code"
        | "hcpcs code"
        | "hcpcs"
        | "hcpcs code number"
        | "code number"
        | "service code"
        | "service/procedure code"
        | "modifier"
        | "modifier code"
        | "modifier_code"
        | "mod"
        | "mod #"
        | "modifier #" => Some("code"),
        "description" | "desc" | "meaning" | "definition" | "text" | "remark" | "remark text"
        | "short description" | "long description" | "shortdesc" | "longdesc" | "descriptor"
        | "shortdescription" | "longdescription" | "shortdescriptor" | "longdescriptor"
        | "code description" => Some("description"),
        "category" | "cagc category" | "adjustment category" => Some("category"),
        "is_active" | "active" | "is active" | "status" | "enabled" => Some("is_active"),
        "applicable_cagc" | "cagc" | "applicable cagc" | "adjustment group" => {
            Some("applicable_cagc")
        }
        "effective_date" | "effective" | "effective date" | "effectivedate" => {
            Some("effective_date")
        }
        "expiration_date" | "expiration" | "expiration date" | "expired" | "expirationdate"
        | "termination_date" | "termination" | "termination date" | "terminated"
        | "terminationdate" => Some("expiration_date"),
        _ => None,
    }
}

// ── Data structures ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ParsedRow {
    code: String,
    description: String,
    is_active: Option<bool>,
    effective_date: Option<NaiveDate>,
    expiration_date: Option<NaiveDate>,
    extra: Option<String>,
}

#[derive(Debug, Clone)]
struct ExistingRow {
    description: String,
    is_active: bool,
    effective_date: Option<NaiveDate>,
    expiration_date: Option<NaiveDate>,
    extra: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Add,
    Update,
    Deactivate,
    Reactivate,
    Unchanged,
}

fn action_name(a: Action) -> &'static str {
    match a {
        Action::Add => "add",
        Action::Update => "update",
        Action::Deactivate => "deactivate",
        Action::Reactivate => "reactivate",
        Action::Unchanged => "unchanged",
    }
}

#[derive(Debug)]
struct PlanItem {
    action: Action,
    row: ParsedRow,
    changes: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
struct RowError {
    row: i64,
    code: Option<String>,
    reason: String,
}

#[derive(Debug)]
struct Counts {
    add: i64,
    update: i64,
    deactivate: i64,
    reactivate: i64,
    unchanged: i64,
}

fn count_actions(plan: &[PlanItem]) -> Counts {
    let mut c = Counts {
        add: 0,
        update: 0,
        deactivate: 0,
        reactivate: 0,
        unchanged: 0,
    };
    for item in plan {
        match item.action {
            Action::Add => c.add += 1,
            Action::Update => c.update += 1,
            Action::Deactivate => c.deactivate += 1,
            Action::Reactivate => c.reactivate += 1,
            Action::Unchanged => c.unchanged += 1,
        }
    }
    c
}

// ── Parsing helpers ───────────────────────────────────────────────────────

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

fn decode_bytes(raw: &[u8]) -> String {
    let stripped = raw.strip_prefix(UTF8_BOM).unwrap_or(raw);
    String::from_utf8_lossy(stripped).into_owned()
}

fn parse_bool(value: Option<&str>) -> Option<bool> {
    let s = value?.trim().to_lowercase();
    if s.is_empty() {
        return None;
    }
    match s.as_str() {
        "true" | "t" | "yes" | "y" | "1" | "active" | "a" | "in effect" | "effective" | "added" => {
            Some(true)
        }
        "false" | "f" | "no" | "n" | "0" | "inactive" | "i" | "deactivated" | "d" | "disabled"
        | "deleted" => Some(false),
        _ => None,
    }
}

fn parse_date(value: Option<&str>) -> Result<Option<NaiveDate>, String> {
    let Some(v) = value else {
        return Ok(None);
    };
    let s = v.trim();
    if s.is_empty() {
        return Ok(None);
    }
    for fmt in ["%Y-%m-%d", "%m/%d/%Y", "%m/%d/%y", "%Y%m%d"] {
        if let Ok(d) = NaiveDate::parse_from_str(s, fmt) {
            return Ok(Some(d));
        }
    }
    Err(format!(
        "unrecognised date {s:?} (expected YYYY-MM-DD or MM/DD/YYYY)"
    ))
}

fn parse_csv(raw: &[u8], kind: &str) -> (Vec<ParsedRow>, Vec<RowError>, i64) {
    let text = decode_bytes(raw);
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let rows: Vec<Vec<String>> = reader
        .records()
        .filter_map(|r| r.ok())
        .map(|r| r.iter().map(|c| c.to_string()).collect::<Vec<String>>())
        .filter(|r| r.iter().any(|c| !c.trim().is_empty()))
        .collect();

    if rows.is_empty() {
        return (Vec::new(), Vec::new(), 0);
    }

    let mut header: HashMap<&str, usize> = HashMap::new();
    for (i, cell) in rows[0].iter().enumerate() {
        let mut name = cell.trim().to_lowercase();
        if name.is_empty() {
            continue;
        }
        // CMS's published ICD-10/HCPCS files annotate headers with the
        // revision, e.g. "SHORT DESCRIPTION (VALID ICD-10 FY2027)" - strip a
        // trailing parenthetical before matching so next year's file, with a
        // different fiscal year in the same spot, still recognises the
        // column instead of silently falling back to positional mapping.
        if let Some(paren) = name.find('(') {
            name.truncate(paren);
            name = name.trim().to_string();
        }
        if let Some(field) = alias_field(&name) {
            if !header.contains_key(field) {
                header.insert(field, i);
            }
        }
    }

    let (data_rows, start_line);
    if header.contains_key("code") && header.contains_key("description") {
        data_rows = &rows[1..];
        start_line = 2;
    } else {
        header.clear();
        for (i, name) in positional_fields(kind).iter().enumerate() {
            header.insert(name, i);
        }
        data_rows = &rows[..];
        start_line = 1;
    }

    let extra_col = extra_column(kind);
    let mut valid: Vec<ParsedRow> = Vec::new();
    let mut errors: Vec<RowError> = Vec::new();

    for (offset, cells) in data_rows.iter().enumerate() {
        let line_no = start_line as i64 + offset as i64;
        let get = |field: &str| -> Option<&str> {
            header
                .get(field)
                .and_then(|&idx| cells.get(idx))
                .map(|s| s.as_str())
        };

        let code = get("code").unwrap_or("").trim().to_string();
        let description = get("description").unwrap_or("").trim().to_string();
        if code.is_empty() {
            errors.push(RowError {
                row: line_no,
                code: None,
                reason: "missing code".into(),
            });
            continue;
        }
        if code.len() > 20 {
            errors.push(RowError {
                row: line_no,
                code: Some(code.clone()),
                reason: "code longer than 20 characters".into(),
            });
            continue;
        }
        if description.is_empty() {
            errors.push(RowError {
                row: line_no,
                code: Some(code.clone()),
                reason: "missing description".into(),
            });
            continue;
        }

        let parsed = (|| {
            let active = parse_bool(get("is_active"));
            let eff = parse_date(get("effective_date"))?;
            let exp = parse_date(get("expiration_date"))?;
            Ok((active, eff, exp))
        })();

        let (is_active, effective_date, expiration_date) = match parsed {
            Ok(v) => v,
            Err(e) => {
                errors.push(RowError {
                    row: line_no,
                    code: Some(code.clone()),
                    reason: e,
                });
                continue;
            }
        };

        let extra = extra_col
            .map(|c| get(c).unwrap_or("").trim().to_string())
            .filter(|s| !s.is_empty());

        valid.push(ParsedRow {
            code,
            description,
            is_active,
            effective_date,
            expiration_date,
            extra,
        });
    }

    (valid, errors, data_rows.len() as i64)
}

// ── Change planning ───────────────────────────────────────────────────────

fn plan_changes(kind: &str, row: &ParsedRow, current: &ExistingRow) -> (Vec<&'static str>, Action) {
    let mut changes: Vec<&'static str> = Vec::new();
    if row.description != current.description {
        changes.push("description");
    }
    if let Some(col) = extra_column(kind) {
        let new = row.extra.clone().unwrap_or_default();
        let old = current.extra.clone().unwrap_or_default();
        if !new.is_empty() && new != old {
            changes.push(col);
        }
    }
    if let Some(ed) = row.effective_date {
        if Some(ed) != current.effective_date {
            changes.push("effective_date");
        }
    }
    if let Some(xd) = row.expiration_date {
        if Some(xd) != current.expiration_date {
            changes.push("expiration_date");
        }
    }

    if let Some(active) = row.is_active {
        if active != current.is_active {
            return (
                changes,
                if active {
                    Action::Reactivate
                } else {
                    Action::Deactivate
                },
            );
        }
    }
    if !changes.is_empty() {
        (changes, Action::Update)
    } else {
        (changes, Action::Unchanged)
    }
}

fn build_plan(
    valid_rows: &[ParsedRow],
    existing: &HashMap<String, ExistingRow>,
    kind: &str,
) -> Vec<PlanItem> {
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut unique: Vec<ParsedRow> = Vec::new();
    for row in valid_rows {
        if let Some(&i) = index.get(&row.code) {
            unique[i] = row.clone();
        } else {
            index.insert(row.code.clone(), unique.len());
            unique.push(row.clone());
        }
    }

    let mut plan: Vec<PlanItem> = Vec::new();
    for row in &unique {
        match existing.get(&row.code) {
            None => plan.push(PlanItem {
                action: Action::Add,
                row: row.clone(),
                changes: Vec::new(),
            }),
            Some(current) => {
                let (changes, action) = plan_changes(kind, row, current);
                plan.push(PlanItem {
                    action,
                    row: row.clone(),
                    changes,
                });
            }
        }
    }
    plan
}

// ── DB helpers ────────────────────────────────────────────────────────────

async fn fetch_existing(
    pool: &sqlx::PgPool,
    table: &str,
    kind: &str,
) -> Result<HashMap<String, ExistingRow>, AppError> {
    let extra = extra_column(kind);
    let sql = match extra {
        Some(col) => format!(
            "SELECT code, description, {col}, is_active, effective_date, expiration_date FROM {table}"
        ),
        None => format!(
            "SELECT code, description, is_active, effective_date, expiration_date FROM {table}"
        ),
    };
    let rows = sqlx::query(&sql)
        .fetch_all(pool)
        .await
        .map_err(AppError::Db)?;

    let mut map = HashMap::new();
    for row in &rows {
        let code: String = row.get("code");
        let description: String = row.get("description");
        let is_active: Option<bool> = row.get("is_active");
        let effective_date: Option<NaiveDate> = row.get("effective_date");
        let expiration_date: Option<NaiveDate> = row.get("expiration_date");
        let extra_val: Option<String> = match extra {
            Some(col) => row.get(col),
            None => None,
        };
        map.insert(
            code,
            ExistingRow {
                description,
                is_active: is_active.unwrap_or(true),
                effective_date,
                expiration_date,
                extra: extra_val,
            },
        );
    }
    Ok(map)
}

async fn insert_row(
    tx: &mut sqlx::PgTransaction<'_>,
    table: &str,
    kind: &str,
    row: &ParsedRow,
) -> Result<(), AppError> {
    let extra = extra_column(kind);
    if let Some(extra_col) = extra {
        sqlx::query(&format!(
            "INSERT INTO {table} (code, description, {extra_col}, is_active, effective_date, expiration_date) \
             VALUES ($1, $2, $3, COALESCE($4, TRUE), COALESCE($5::date, CURRENT_DATE), $6::date)"
        ))
        .bind(&row.code)
        .bind(&row.description)
        .bind(&row.extra)
        .bind(row.is_active)
        .bind(row.effective_date)
        .bind(row.expiration_date)
        .execute(&mut **tx)
        .await
        .map_err(AppError::Db)?;
    } else {
        sqlx::query(&format!(
            "INSERT INTO {table} (code, description, is_active, effective_date, expiration_date) \
             VALUES ($1, $2, COALESCE($3, TRUE), COALESCE($4::date, CURRENT_DATE), $5::date)"
        ))
        .bind(&row.code)
        .bind(&row.description)
        .bind(row.is_active)
        .bind(row.effective_date)
        .bind(row.expiration_date)
        .execute(&mut **tx)
        .await
        .map_err(AppError::Db)?;
    }
    Ok(())
}

async fn apply_update(
    tx: &mut sqlx::PgTransaction<'_>,
    table: &str,
    kind: &str,
    row: &ParsedRow,
    changes: &[&str],
) -> Result<(), AppError> {
    let mut qb = QueryBuilder::<Postgres>::new("UPDATE ");
    qb.push(table);
    qb.push(" SET ");

    let mut first = true;
    if (row.is_active).is_some() && is_status_change(row) {
        if let Some(active) = row.is_active {
            if !first {
                qb.push(", ");
            }
            first = false;
            qb.push("is_active = ");
            qb.push_bind(active);
        }
    }
    for &col in changes {
        match col {
            "description" => {
                if !first {
                    qb.push(", ");
                }
                first = false;
                qb.push("description = ");
                qb.push_bind(&row.description);
            }
            "effective_date" => {
                if let Some(d) = row.effective_date {
                    if !first {
                        qb.push(", ");
                    }
                    first = false;
                    qb.push("effective_date = ");
                    qb.push_bind(d);
                }
            }
            "expiration_date" => {
                if let Some(d) = row.expiration_date {
                    if !first {
                        qb.push(", ");
                    }
                    first = false;
                    qb.push("expiration_date = ");
                    qb.push_bind(d);
                }
            }
            "category" | "applicable_cagc" => {
                if let Some(v) = &row.extra {
                    if !first {
                        qb.push(", ");
                    }
                    first = false;
                    qb.push(col);
                    qb.push(" = ");
                    qb.push_bind(v);
                }
            }
            _ => {}
        }
    }
    qb.push(", updated_at = NOW() WHERE code = ");
    qb.push_bind(&row.code);

    let _ = kind;
    qb.build().execute(&mut **tx).await.map_err(AppError::Db)?;
    Ok(())
}

fn is_status_change(row: &ParsedRow) -> bool {
    // Only the deactivation/re-activation rows set is_active in the UPDATE.
    row.is_active.is_some()
}

async fn apply_plan(
    pool: &sqlx::PgPool,
    table: &str,
    kind: &str,
    plan: &[PlanItem],
) -> Result<(), AppError> {
    let mut tx = pool.begin().await.map_err(AppError::Db)?;
    for item in plan {
        match item.action {
            Action::Add => insert_row(&mut tx, table, kind, &item.row).await?,
            Action::Unchanged => {}
            _ => apply_update(&mut tx, table, kind, &item.row, &item.changes).await?,
        }
    }
    tx.commit().await.map_err(AppError::Db)?;
    Ok(())
}

// ── Query models ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_page_size")]
    pub page_size: i64,
}

fn default_page() -> i64 {
    1
}
fn default_page_size() -> i64 {
    50
}

#[derive(Deserialize)]
pub struct DeleteCodesBody {
    pub codes: Vec<String>,
}

// ── Handlers ──────────────────────────────────────────────────────────────

pub async fn reference_summary(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let pool = &state.pool;
    let kinds = ["carc", "rarc", "icd10", "cpt", "modifier", "hcpcs"];
    let mut out = serde_json::Map::new();
    for kind in kinds {
        let table = table_for(kind)?;
        let row = sqlx::query(&format!(
            "SELECT COUNT(*) AS total, COUNT(*) FILTER (WHERE is_active) AS active FROM {table}"
        ))
        .fetch_one(pool)
        .await
        .map_err(AppError::Db)?;
        let total: i64 = row.get("total");
        let active: i64 = row.get("active");

        let last = sqlx::query(
            "SELECT imported_at, imported_by, filename, rows_added, rows_updated, rows_deactivated \
             FROM reference_imports WHERE kind = $1 ORDER BY imported_at DESC LIMIT 1",
        )
        .bind(kind)
        .fetch_optional(pool)
        .await
        .map_err(AppError::Db)?;

        let last_import = match last {
            Some(l) => {
                let at: chrono::DateTime<Utc> = l.get("imported_at");
                let by: Option<String> = l.get("imported_by");
                let filename: Option<String> = l.get("filename");
                // These columns are INTEGER, not BIGINT - sqlx's typed get()
                // panics rather than coerces on a width mismatch, which this
                // path never surfaced before nothing could reach it (see the
                // init.sql fix for the reference-code tables it reads from).
                let added: i32 = l.get("rows_added");
                let updated: i32 = l.get("rows_updated");
                let deactivated: i32 = l.get("rows_deactivated");
                json!({
                    "at": at.to_rfc3339(),
                    "by": by,
                    "filename": filename,
                    "added": added,
                    "updated": updated,
                    "deactivated": deactivated,
                })
            }
            None => Value::Null,
        };

        out.insert(
            kind.to_string(),
            json!({
                "total": total,
                "active": active,
                "last_import": last_import,
            }),
        );
    }
    Ok(Json(Value::Object(out)))
}

pub async fn reference_import(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(kind): Path<String>,
    mut multipart: Multipart,
) -> Result<Response, AppError> {
    let table = table_for(&kind)?;
    let cap = max_bytes(&kind)?;

    let mut file_bytes: Vec<u8> = Vec::new();
    let mut filename: Option<String> = None;
    let mut apply = false;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| AppError::BadRequest("Invalid multipart form data".into()))?
    {
        if let Some(fname) = field.file_name() {
            if filename.is_none() {
                filename = Some(fname.to_string());
            }
            let chunk = field
                .bytes()
                .await
                .map_err(|e| AppError::Internal(e.to_string()))?;
            file_bytes.extend_from_slice(&chunk);
        } else {
            let val = field
                .text()
                .await
                .map_err(|e| AppError::Internal(e.to_string()))?;
            if val == "true" || val == "1" || val.eq_ignore_ascii_case("yes") {
                apply = true;
            }
        }
    }

    if file_bytes.len() > cap {
        let mb = cap / (1024 * 1024);
        let resp = (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({
                "detail": format!("File too large: maximum {mb} MB for this list")
            })),
        )
            .into_response();
        return Ok(resp);
    }

    if file_bytes.is_empty() || file_bytes.iter().all(|b| b.is_ascii_whitespace()) {
        return Err(AppError::BadRequest("File is empty".into()));
    }

    let (valid_rows, row_errors, rows_parsed) = parse_csv(&file_bytes, &kind);
    if valid_rows.is_empty() {
        let detail = if !row_errors.is_empty() {
            format!(
                "No usable rows found - expected a CSV with at least 'code' and \
                 'description' columns. {} row(s) rejected, see row_errors",
                row_errors.len()
            )
        } else {
            "No usable rows found - expected a CSV with at least 'code' and 'description' columns."
                .into()
        };
        return Err(AppError::BadRequest(detail));
    }

    let pool = &state.pool;
    let existing = fetch_existing(pool, table, &kind).await?;
    let plan = build_plan(&valid_rows, &existing, &kind);
    let counts = count_actions(&plan);

    let file_codes: HashSet<String> = valid_rows.iter().map(|r| r.code.clone()).collect();
    let codes_not_in_file = existing.keys().filter(|k| !file_codes.contains(*k)).count();

    if apply {
        apply_plan(pool, table, &kind, &plan).await?;
        let updated = counts.update + counts.deactivate + counts.reactivate;
        sqlx::query(
            "INSERT INTO reference_imports \
             (kind, filename, rows_parsed, rows_added, rows_updated, rows_deactivated, \
              codes_not_in_file, row_errors, imported_by) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8::jsonb, $9)",
        )
        .bind(&kind)
        .bind(&filename)
        .bind(rows_parsed)
        .bind(counts.add)
        .bind(updated)
        .bind(counts.deactivate)
        .bind(codes_not_in_file as i64)
        .bind(serde_json::to_string(&row_errors).unwrap_or_else(|_| "[]".into()))
        .bind(&principal.username)
        .execute(pool)
        .await
        .map_err(AppError::Db)?;
    }

    let sample: Vec<Value> = plan
        .iter()
        .take(10)
        .map(|item| {
            json!({
                "code": item.row.code,
                "description": item.row.description,
                "action": action_name(item.action),
            })
        })
        .collect();

    let fname = filename.unwrap_or_default();
    let body = json!({
        "kind": kind,
        "mode": if apply { "applied" } else { "dry-run" },
        "filename": fname,
        "rows_parsed": rows_parsed,
        "valid_rows": valid_rows.len(),
        "row_errors": row_errors,
        "changes": {
            "add": counts.add,
            "update": counts.update,
            "deactivate": counts.deactivate,
            "reactivate": counts.reactivate,
            "unchanged": counts.unchanged,
        },
        "codes_not_in_file": codes_not_in_file,
        "sample": sample,
    });

    Ok(Json(body).into_response())
}

pub async fn reference_search(
    State(state): State<AppState>,
    Path(kind): Path<String>,
    Query(params): Query<SearchQuery>,
) -> Result<Json<Value>, AppError> {
    let table = table_for(&kind)?;
    let page = params.page.max(1);
    let page_size = params.page_size.clamp(1, 200);
    let needle = params.q.trim().to_string();
    let like = format!(
        "%{}%",
        needle
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );

    let pool = &state.pool;
    let (total, rows) = if !needle.is_empty() {
        let t = sqlx::query(&format!(
            "SELECT COUNT(*) AS n FROM {table} WHERE code ILIKE $1 OR description ILIKE $1"
        ))
        .bind(&like)
        .fetch_one(pool)
        .await
        .map_err(AppError::Db)?;
        let total: i64 = t.get("n");
        let rows = sqlx::query(&format!(
            "SELECT code, description, is_active, effective_date, expiration_date, updated_at \
             FROM {table} WHERE code ILIKE $1 OR description ILIKE $1 ORDER BY code LIMIT $2 OFFSET $3"
        ))
        .bind(&like)
        .bind(page_size)
        .bind((page - 1) * page_size)
        .fetch_all(pool)
        .await
        .map_err(AppError::Db)?;
        (total, rows)
    } else {
        let t = sqlx::query(&format!("SELECT COUNT(*) AS n FROM {table}"))
            .fetch_one(pool)
            .await
            .map_err(AppError::Db)?;
        let total: i64 = t.get("n");
        let rows = sqlx::query(&format!(
            "SELECT code, description, is_active, effective_date, expiration_date, updated_at \
             FROM {table} ORDER BY code LIMIT $1 OFFSET $2"
        ))
        .bind(page_size)
        .bind((page - 1) * page_size)
        .fetch_all(pool)
        .await
        .map_err(AppError::Db)?;
        (total, rows)
    };

    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            let code: String = r.get("code");
            let description: String = r.get("description");
            let is_active: Option<bool> = r.get("is_active");
            let effective_date: Option<NaiveDate> = r.get("effective_date");
            let expiration_date: Option<NaiveDate> = r.get("expiration_date");
            let updated_at: Option<chrono::DateTime<Utc>> = r.get("updated_at");
            json!({
                "code": code,
                "description": description,
                "is_active": is_active,
                "effective_date": effective_date.map(|d| d.to_string()),
                "expiration_date": expiration_date.map(|d| d.to_string()),
                "updated_at": updated_at.map(|d| d.to_rfc3339()),
            })
        })
        .collect();

    let pages = if total == 0 {
        0
    } else {
        (total + page_size - 1) / page_size
    };

    Ok(Json(json!({
        "kind": kind,
        "q": needle,
        "total": total,
        "page": page,
        "page_size": page_size,
        "pages": pages,
        "items": items,
    })))
}

pub async fn reference_delete(
    State(state): State<AppState>,
    Path(kind): Path<String>,
    Json(body): Json<DeleteCodesBody>,
) -> Result<Json<Value>, AppError> {
    let table = table_for(&kind)?;
    let mut set: BTreeSet<String> = BTreeSet::new();
    for c in &body.codes {
        let t = c.trim().to_string();
        if !t.is_empty() {
            set.insert(t);
        }
    }
    let codes: Vec<String> = set.into_iter().collect();
    if codes.is_empty() {
        return Err(AppError::BadRequest(
            "Provide at least one code to delete".into(),
        ));
    }
    if codes.len() > 10000 {
        return Err(AppError::BadRequest(
            "Too many codes in one request (maximum 10000)".into(),
        ));
    }
    let result = sqlx::query(&format!(
        "DELETE FROM {table} WHERE code = ANY($1::varchar[])"
    ))
    .bind(&codes)
    .execute(&state.pool)
    .await
    .map_err(AppError::Db)?;
    let deleted = result.rows_affected() as i64;
    Ok(Json(json!({
        "kind": kind,
        "requested": codes.len(),
        "deleted": deleted,
        "not_found": codes.len() as i64 - deleted,
    })))
}

pub async fn reference_clear(
    State(state): State<AppState>,
    Path(kind): Path<String>,
) -> Result<Json<Value>, AppError> {
    let table = table_for(&kind)?;
    let result = sqlx::query(&format!("DELETE FROM {table}"))
        .execute(&state.pool)
        .await
        .map_err(AppError::Db)?;
    let deleted = result.rows_affected() as i64;
    Ok(Json(json!({ "kind": kind, "deleted": deleted })))
}

/// Import the two CMS NCCI formats without forcing them into the simpler
/// code/description reference-list schema. `apply` is a multipart field; the
/// default is a safe preview. Expected headers are deliberately permissive:
/// PTP: column_1_code,column_2_code,modifier_indicator; MUE:
/// code,mue_value,adjudication_indicator. Effective/termination dates are
/// optional ISO dates.
pub async fn ncci_import(
    State(state): State<AppState>,
    Path(kind): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<Value>, AppError> {
    if kind != "ptp" && kind != "mue" {
        return Err(AppError::Unprocessable(
            "NCCI kind must be ptp or mue".into(),
        ));
    }
    let mut bytes = Vec::new();
    let mut apply = false;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| AppError::BadRequest("Invalid multipart form data".into()))?
    {
        if field.file_name().is_some() {
            bytes.extend_from_slice(
                &field
                    .bytes()
                    .await
                    .map_err(|_| AppError::BadRequest("Could not read CSV".into()))?,
            );
        } else if field
            .text()
            .await
            .unwrap_or_default()
            .trim()
            .eq_ignore_ascii_case("true")
        {
            apply = true;
        }
    }
    if bytes.is_empty() || bytes.len() > 50 * 1024 * 1024 {
        return Err(AppError::BadRequest(
            "CSV must be present and no larger than 50 MB".into(),
        ));
    }
    let mut csv = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(bytes.as_slice());
    let headers = csv
        .headers()
        .map_err(|_| AppError::BadRequest("NCCI CSV needs a header row".into()))?
        .iter()
        .map(|h| h.to_ascii_lowercase().replace([' ', '-'], "_"))
        .collect::<Vec<_>>();
    let find = |names: &[&str]| headers.iter().position(|h| names.contains(&h.as_str()));
    let c1 = find(&["column_1_code", "column1", "hcpcs_code_1"]);
    let c2 = find(&["column_2_code", "column2", "hcpcs_code_2"]);
    let modifier = find(&["modifier_indicator", "modifier"]);
    let code = find(&["code", "hcpcs_code"]);
    let value = find(&["mue_value", "mue"]);
    let adj = find(&["adjudication_indicator", "adjudication"]);
    let effective = find(&["effective_date", "effective"]);
    let termination = find(&["termination_date", "termination", "end_date"]);
    if (kind == "ptp" && (c1.is_none() || c2.is_none() || modifier.is_none()))
        || (kind == "mue" && (code.is_none() || value.is_none()))
    {
        return Err(AppError::BadRequest(
            "CSV is missing required NCCI columns".into(),
        ));
    }
    let mut valid = 0_i64;
    let mut errors = Vec::new();
    for (i, row) in csv.records().enumerate() {
        let row = row.map_err(|_| AppError::BadRequest("Malformed CSV row".into()))?;
        let get = |idx: Option<usize>| idx.and_then(|n| row.get(n)).unwrap_or("").trim();
        let date = |idx| {
            let v = get(idx);
            if v.is_empty() {
                Ok(None)
            } else {
                NaiveDate::parse_from_str(v, "%Y-%m-%d")
                    .map(Some)
                    .map_err(|_| ())
            }
        };
        let eff = date(effective).map_err(|_| {
            AppError::BadRequest(format!("row {} has invalid effective_date", i + 2))
        })?;
        let term = date(termination).map_err(|_| {
            AppError::BadRequest(format!("row {} has invalid termination_date", i + 2))
        })?;
        if kind == "ptp" {
            let a = get(c1);
            let b = get(c2);
            let m = get(modifier).parse::<i16>().ok();
            if a.is_empty() || b.is_empty() || !matches!(m, Some(0 | 1 | 9)) {
                errors.push(i + 2);
                continue;
            }
            valid += 1;
            if apply {
                sqlx::query("INSERT INTO ncci_ptp_edits (column_1_code,column_2_code,modifier_indicator,effective_date,termination_date) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (column_1_code,column_2_code,effective_date) DO UPDATE SET modifier_indicator=EXCLUDED.modifier_indicator,termination_date=EXCLUDED.termination_date").bind(a).bind(b).bind(m.unwrap()).bind(eff).bind(term).execute(&state.pool).await.map_err(AppError::Db)?;
            }
        } else {
            let c = get(code);
            let v = get(value).parse::<i32>().ok();
            let a = get(adj).parse::<i16>().ok();
            if c.is_empty() || v.unwrap_or(0) <= 0 || a.is_some_and(|n| !matches!(n, 1..=3)) {
                errors.push(i + 2);
                continue;
            }
            valid += 1;
            if apply {
                sqlx::query("INSERT INTO ncci_mue_edits (code,mue_value,adjudication_indicator,effective_date,termination_date) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (code,effective_date) DO UPDATE SET mue_value=EXCLUDED.mue_value,adjudication_indicator=EXCLUDED.adjudication_indicator,termination_date=EXCLUDED.termination_date").bind(c).bind(v.unwrap()).bind(a).bind(eff).bind(term).execute(&state.pool).await.map_err(AppError::Db)?;
            }
        }
    }
    Ok(Json(
        json!({"kind":kind,"mode":if apply{"applied"}else{"dry-run"},"valid_rows":valid,"invalid_rows":errors.len(),"invalid_row_numbers":errors.into_iter().take(50).collect::<Vec<_>>() }),
    ))
}

// ── Router ────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/summary", get(reference_summary))
        .route("/{kind}/import", post(reference_import))
        .route("/{kind}/search", get(reference_search))
        .route("/{kind}/delete", post(reference_delete))
        .route("/{kind}/clear", post(reference_clear))
        .route("/ncci/{kind}/import", post(ncci_import))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_with_a_trailing_parenthetical_annotation_still_matches() {
        // CMS's published ICD-10/HCPCS files annotate headers with the
        // revision year, e.g. "SHORT DESCRIPTION (VALID ICD-10 FY2027)",
        // which used to fail exact-string alias matching and silently fall
        // back to positional mapping - misaligning any trailing column (like
        // "NF EXCL") into effective_date and rejecting every row where it
        // held a non-date value.
        let csv = "CODE,SHORT DESCRIPTION (VALID ICD-10 FY2027),LONG DESCRIPTION (VALID ICD-10 FY2027),NF EXCL\n\
                    A000,Cholera due to Vibrio cholerae,Cholera due to Vibrio cholerae 01,Y\n\
                    A001,Cholera due to Vibrio cholerae eltor,Cholera due to Vibrio cholerae 01 eltor,\n";
        let (valid, errors, rows_parsed) = parse_csv(csv.as_bytes(), "icd10");
        assert!(
            errors.is_empty(),
            "NF EXCL must not be mistaken for a date column: {errors:?}"
        );
        assert_eq!(rows_parsed, 2);
        assert_eq!(valid.len(), 2);
        assert_eq!(valid[0].code, "A000");
        assert_eq!(valid[0].description, "Cholera due to Vibrio cholerae");
    }
}
