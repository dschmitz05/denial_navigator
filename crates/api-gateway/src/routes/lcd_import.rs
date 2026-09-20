//! Bulk LCD (Local Coverage Determination) import, from either of the two
//! formats CMS actually publishes its bulk export in: a CSV, or the raw
//! Access database (.mdb/.accdb) it was generated from - one row per
//! policy either way, with title, indication, coding guidelines,
//! documentation requirements, and bibliography as HTML-formatted text. The
//! single-document upload path (`POST /knowledge/documents/upload`) treats
//! whatever file it's given as ONE document to chunk and embed, so
//! uploading either export as-is would mix every LCD nationwide into a
//! single incoherent document - a category-of-data mismatch, not a size
//! problem (though the export is also comfortably over the per-document
//! 25 MB cap).
//!
//! This parses the file server-side, splits it into one document per LCD,
//! and indexes them in admin-driven batches: `POST /` stages the parsed,
//! filtered document list in object storage and returns a job id;
//! `POST /{job_id}/batch` indexes the next `limit` of them and reports
//! progress, to be called repeatedly (mirroring the existing
//! `POST /knowledge/reindex` batch-per-request pattern, rather than either a
//! giant blocking request or a background job queue this app doesn't have).
//! Same shape of document as `scripts/split_lcd_csv.py` produces, so a
//! script-driven and a UI-driven import are interchangeable.

use std::collections::{HashMap, HashSet};
use std::io::Write;

use axum::extract::{Multipart, Path, Query, State};
use axum::routing::post;
use axum::{Extension, Json, Router};
use chrono::NaiveDate;
use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use denial_storage::ObjectKey;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use super::scope::organization_id;
use crate::state::AppState;

// CMS's own .mdb export runs to ~115 MB; comfortable headroom above that
// for a larger future one, well short of the 200 MB reverse-proxy cap that
// exists specifically to admit this.
const MAX_BULK_IMPORT_BYTES: usize = 180 * 1024 * 1024;

/// The Jet/ACE signature at a fixed offset, present regardless of what
/// filename a caller's browser reports. Checked as a fallback when the
/// filename extension is missing or doesn't say .mdb/.accdb, since that's
/// what actually determines which parser can read the bytes.
fn looks_like_jet_database(raw: &[u8]) -> bool {
    raw.get(4..19) == Some(b"Standard Jet DB") || raw.get(4..19) == Some(b"Standard ACE DB")
}

/// One CSV row or one Access `lcd` table row, by canonical column name -
/// both formats use the same names (CMS generates the CSV from this exact
/// table), so everything after parsing is format-agnostic.
type LcdRow = HashMap<String, String>;

fn field<'a>(row: &'a LcdRow, name: &str) -> &'a str {
    row.get(name).map(String::as_str).unwrap_or("").trim()
}

// Kept in the document body, in this order. Left out deliberately: CMS
// process/administrative fields (adv_meeting, comment_start_dt,
// draft_contact, revenue_para, issue_change, ...) that aren't coverage
// guidance an appeal would cite.
const BODY_FIELDS: &[(&str, &str)] = &[
    ("Indication", "indication"),
    ("Diagnoses that support coverage", "diagnoses_support"),
    (
        "Diagnoses that do not support coverage",
        "diagnoses_dont_support",
    ),
    ("Coding guidelines", "coding_guidelines"),
    ("Documentation requirements", "doc_reqs"),
    ("Utilization guide", "util_guide"),
    ("Summary of evidence", "summary_of_evidence"),
    ("Analysis of evidence", "analysis_of_evidence"),
    ("Bibliography", "bibliography"),
];

#[derive(Serialize, Deserialize)]
struct ImportedDoc {
    title: String,
    content: String,
}

#[derive(Serialize, Deserialize)]
struct JobState {
    organization_id: Uuid,
    total: usize,
    processed: usize,
}

/// CMS's fields are well-formed HTML fragments, not attacker-controlled
/// markup that needs a real parser - a tag-stripping pass is sufficient.
fn strip_html(value: &str) -> String {
    // A tag boundary must become a space, not nothing: this field's real
    // source is "<li>Covered</li><li>Not covered</li>", and dropping the
    // tags without inserting a separator runs adjacent list items,
    // paragraphs and inline markup (<sup>, <br>, ...) into one unbroken
    // word - the actual cause of the reported "text is missing / not
    // readable" reports, not any content actually being lost.
    let mut out = String::with_capacity(value.len());
    let mut in_tag = false;
    for c in value.chars() {
        match c {
            '<' => {
                if !in_tag {
                    out.push(' ');
                }
                in_tag = true;
            }
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    // decode_html_entities covers the full HTML5 named + numeric entity
    // table - CMS's export alone uses over 80 distinct named entities
    // (accented letters, Greek letters, typographic quotes, &ge;/&le;/
    // &plusmn; in clinical criteria) plus numeric references, not the
    // handful worth hardcoding by hand.
    let decoded = html_escape::decode_html_entities(&out);
    // Collapse the whitespace the tag-boundary spacing above introduced,
    // one line at a time so a real paragraph break in the source is kept.
    decoded
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_document(row: &LcdRow) -> ImportedDoc {
    // Some titles carry markup too, e.g. "Vitamin B<sub>12</sub> Injections".
    let title_raw = field(row, "title");
    let title = if title_raw.is_empty() {
        "(untitled LCD)".to_string()
    } else {
        strip_html(title_raw)
    };
    let mut parts = vec![title.clone()];

    let det_num = field(row, "determination_number");
    let display_id = if det_num.is_empty() {
        field(row, "lcd_id")
    } else {
        det_num
    };
    let eff = field(row, "orig_det_eff_date");
    let rev = field(row, "rev_eff_date");
    let mut meta_bits = Vec::new();
    if !display_id.is_empty() {
        meta_bits.push(format!("Determination number: {display_id}"));
    }
    if !eff.is_empty() {
        meta_bits.push(format!("Effective: {eff}"));
    }
    if !rev.is_empty() {
        meta_bits.push(format!("Last revised: {rev}"));
    }
    if !meta_bits.is_empty() {
        parts.push(meta_bits.join(" | "));
    }

    for (heading, field_name) in BODY_FIELDS {
        let raw = field(row, field_name);
        if raw.is_empty() {
            continue;
        }
        let cleaned = strip_html(raw);
        if !cleaned.is_empty() {
            parts.push(format!("## {heading}\n\n{cleaned}"));
        }
    }

    ImportedDoc {
        title,
        content: parts.join("\n\n"),
    }
}

/// Parses a CMS bulk LCD CSV export into rows keyed by column name.
fn read_csv_rows(raw: &[u8]) -> Result<Vec<LcdRow>, AppError> {
    let mut reader = csv::ReaderBuilder::new().flexible(true).from_reader(raw);
    let headers = reader
        .headers()
        .map_err(|e| AppError::BadRequest(format!("Invalid CSV: {e}")))?
        .clone();
    if !headers.iter().any(|h| h == "title") || !headers.iter().any(|h| h == "status") {
        return Err(AppError::BadRequest(
            "Not a recognised CMS LCD export - missing 'title' or 'status' column".into(),
        ));
    }

    let mut rows = Vec::new();
    for result in reader.records() {
        let Ok(record) = result else { continue };
        let row: LcdRow = headers
            .iter()
            .zip(record.iter())
            .map(|(h, v)| (h.to_string(), v.trim().to_string()))
            .collect();
        rows.push(row);
    }
    Ok(rows)
}

/// Access stores dates as days-since-1899-12-30; rendered to match the CSV
/// export's own "YYYY-MM-DD ..." text so a document reads the same either
/// way it was imported.
fn mdb_value_to_string(value: &jetdb::Value) -> String {
    use jetdb::Value;
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Byte(n) => n.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Long(n) => n.to_string(),
        Value::BigInt(n) => n.to_string(),
        Value::Float(n) => n.to_string(),
        Value::Double(n) => n.to_string(),
        Value::Text(s) => s.clone(),
        Value::Money(s) | Value::Numeric(s) | Value::Guid(s) | Value::DateTimeExtended(s) => {
            s.clone()
        }
        Value::Binary(_) => String::new(),
        Value::Timestamp(days) => NaiveDate::from_ymd_opt(1899, 12, 30)
            .and_then(|epoch| epoch.checked_add_signed(chrono::Duration::days(*days as i64)))
            .map(|d| d.format("%Y-%m-%d 00:00:00").to_string())
            .unwrap_or_default(),
    }
}

/// Parses the `lcd` table out of a CMS bulk export Access database. Blocking
/// I/O - the caller runs this on a blocking thread. `jetdb` only opens by
/// path, so the caller writes the uploaded bytes to a temp file first.
fn read_mdb_rows(path: &std::path::Path) -> Result<Vec<LcdRow>, AppError> {
    let mut reader = jetdb::PageReader::open(path)
        .map_err(|e| AppError::BadRequest(format!("Could not open Access database: {e}")))?;
    let catalog = jetdb::read_catalog(&mut reader)
        .map_err(|e| AppError::BadRequest(format!("Could not read Access database: {e}")))?;
    let entry = catalog.iter().find(|e| e.name == "lcd").ok_or_else(|| {
        AppError::BadRequest(
            "Not a recognised CMS LCD export - no 'lcd' table in this database".into(),
        )
    })?;
    let table_def = jetdb::read_table_def(&mut reader, &entry.name, entry.table_page)
        .map_err(|e| AppError::BadRequest(format!("Could not read the 'lcd' table: {e}")))?;
    let result = jetdb::read_table_rows(&mut reader, &table_def)
        .map_err(|e| AppError::BadRequest(format!("Could not read the 'lcd' table: {e}")))?;

    let rows = result
        .rows
        .iter()
        .map(|values| {
            table_def
                .columns
                .iter()
                .zip(values.iter())
                .map(|(col, val)| {
                    (
                        col.name.clone(),
                        mdb_value_to_string(val).trim().to_string(),
                    )
                })
                .collect::<LcdRow>()
        })
        .collect();
    Ok(rows)
}

#[derive(Deserialize)]
pub struct StartImportQuery {
    /// Comma-separated CMS status codes. Defaults to active only - a 'P'
    /// (proposed) LCD is not yet in effect, and surfacing it as current
    /// coverage guidance would be wrong for a billing decision.
    #[serde(default = "default_status")]
    pub status: String,
    /// Comma-separated, OR'd, case-insensitive substrings matched against
    /// the LCD title.
    pub keyword: Option<String>,
    pub limit: Option<usize>,
}

fn default_status() -> String {
    "A".to_string()
}

pub async fn start_import(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<StartImportQuery>,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, AppError> {
    let organization_id = organization_id(&principal)?;

    let mut raw: Vec<u8> = Vec::new();
    let mut filename: Option<String> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| AppError::BadRequest("Invalid multipart form data".into()))?
    {
        let Some(name) = field.file_name().map(str::to_string) else {
            continue;
        };
        filename = Some(name);
        let chunk = field
            .bytes()
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        raw.extend_from_slice(&chunk);
        if raw.len() > MAX_BULK_IMPORT_BYTES {
            return Err(AppError::BadRequest(format!(
                "File too large: maximum {} MB",
                MAX_BULK_IMPORT_BYTES / (1024 * 1024)
            )));
        }
    }
    if raw.is_empty() {
        return Err(AppError::BadRequest("File is empty".into()));
    }

    let wanted_status: HashSet<String> = params
        .status
        .split(',')
        .map(|s| s.trim().to_uppercase())
        .filter(|s| !s.is_empty())
        .collect();
    let keywords: Vec<String> = params
        .keyword
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    let lower_name = filename.as_deref().unwrap_or("").to_lowercase();
    let is_mdb = lower_name.ends_with(".mdb")
        || lower_name.ends_with(".accdb")
        || (!lower_name.ends_with(".csv") && looks_like_jet_database(&raw));

    let all_rows = if is_mdb {
        // jetdb only opens by path; write the buffered upload out once. The
        // parse itself is blocking file I/O against a file that can run over
        // 100 MB, so it runs on a blocking thread rather than tying up an
        // async worker for however long that takes.
        let mut tmp = tempfile::NamedTempFile::new()
            .map_err(|e| AppError::Internal(format!("Could not stage upload: {e}")))?;
        tmp.write_all(&raw)
            .map_err(|e| AppError::Internal(format!("Could not stage upload: {e}")))?;
        let path = tmp.path().to_path_buf();
        tokio::task::spawn_blocking(move || {
            let rows = read_mdb_rows(&path);
            drop(tmp); // keep the tempfile alive until parsing finishes
            rows
        })
        .await
        .map_err(|e| AppError::Internal(format!("Import task failed: {e}")))??
    } else {
        read_csv_rows(&raw)?
    };

    let mut docs: Vec<ImportedDoc> = Vec::new();
    let mut skipped = 0usize;
    for row in &all_rows {
        let status = field(row, "status").to_uppercase();
        if !wanted_status.contains(&status) {
            skipped += 1;
            continue;
        }
        let title_lower = field(row, "title").to_lowercase();
        if !keywords.is_empty() && !keywords.iter().any(|k| title_lower.contains(k.as_str())) {
            skipped += 1;
            continue;
        }
        let doc = build_document(row);
        if doc.content.trim().is_empty() {
            skipped += 1;
            continue;
        }
        docs.push(doc);
        if params.limit.is_some_and(|limit| docs.len() >= limit) {
            break;
        }
    }

    if docs.is_empty() {
        return Err(AppError::BadRequest(
            "No LCDs matched the given filter".into(),
        ));
    }

    let job_id = Uuid::new_v4();
    let total = docs.len();
    stage_job(&state, job_id, organization_id, &docs)?;

    Ok(Json(serde_json::json!({
        "job_id": job_id,
        "total": total,
        "skipped": skipped,
    })))
}

fn stage_job(
    state: &AppState,
    job_id: Uuid,
    organization_id: Uuid,
    docs: &[ImportedDoc],
) -> Result<(), AppError> {
    let docs_key = ObjectKey::parse(format!("lcd-imports/{job_id}/documents.json"))
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let state_key = ObjectKey::parse(format!("lcd-imports/{job_id}/state.json"))
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let docs_json = serde_json::to_vec(docs).map_err(|e| AppError::Internal(e.to_string()))?;
    state
        .object_storage
        .put(&docs_key, &docs_json)
        .map_err(|e| AppError::Internal(format!("Could not stage import: {e}")))?;

    let job = JobState {
        organization_id,
        total: docs.len(),
        processed: 0,
    };
    let job_json = serde_json::to_vec(&job).map_err(|e| AppError::Internal(e.to_string()))?;
    state
        .object_storage
        .put(&state_key, &job_json)
        .map_err(|e| AppError::Internal(format!("Could not stage import: {e}")))?;
    Ok(())
}

#[derive(Deserialize)]
pub struct BatchQuery {
    #[serde(default = "default_batch_limit")]
    pub limit: usize,
}

// Indexing one LCD (chunk + embed) runs ~10-15s depending on document
// length and the embedding model, and this whole call has to complete
// within the reverse proxy's read timeout. Small enough to stay well
// inside that even on slower hardware, still large enough that a UI
// polling loop isn't dominated by request overhead.
fn default_batch_limit() -> usize {
    3
}

pub async fn process_batch(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(job_id): Path<Uuid>,
    Query(params): Query<BatchQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let docs_key = ObjectKey::parse(format!("lcd-imports/{job_id}/documents.json"))
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let state_key = ObjectKey::parse(format!("lcd-imports/{job_id}/state.json"))
        .map_err(|e| AppError::Internal(e.to_string()))?;

    let job_bytes = state
        .object_storage
        .get(&state_key)
        .map_err(|_| AppError::NotFound)?;
    let mut job: JobState =
        serde_json::from_slice(&job_bytes).map_err(|e| AppError::Internal(e.to_string()))?;
    if job.organization_id != organization_id {
        return Err(AppError::NotFound);
    }

    let docs_bytes = state
        .object_storage
        .get(&docs_key)
        .map_err(|_| AppError::NotFound)?;
    let docs: Vec<ImportedDoc> =
        serde_json::from_slice(&docs_bytes).map_err(|e| AppError::Internal(e.to_string()))?;

    let start = job.processed;
    let end = (start + params.limit.max(1)).min(docs.len());

    let mut results = Vec::new();
    for doc in &docs[start..end] {
        // Re-importing the same export (the same file twice, or an
        // overlapping keyword/status filter run again) must not create a
        // second copy of an LCD already indexed under this title -
        // re-ingesting an existing document's id replaces its chunks rather
        // than duplicating the document itself.
        let existing = sqlx::query(
            "SELECT id FROM knowledge_documents \
             WHERE organization_id = $1 AND source_type = 'cms_lcd' \
               AND title = $2 AND status != 'archived'",
        )
        .bind(organization_id)
        .bind(&doc.title)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::Db)?;

        let doc_id: Uuid = if let Some(row) = existing {
            row.try_get("id")
                .map_err(|e| AppError::Internal(e.to_string()))?
        } else {
            let row = sqlx::query(
                "INSERT INTO knowledge_documents (organization_id, title, source_type, status) \
                 VALUES ($1, $2, 'cms_lcd', 'pending') RETURNING id",
            )
            .bind(organization_id)
            .bind(&doc.title)
            .fetch_one(&state.pool)
            .await
            .map_err(AppError::Db)?;
            row.try_get("id")
                .map_err(|e| AppError::Internal(e.to_string()))?
        };

        match state
            .rag
            .ingest_document(&doc_id.to_string(), &doc.content)
            .await
        {
            Ok(result) => {
                sqlx::query("UPDATE knowledge_documents SET status = 'indexed' WHERE id = $1")
                    .bind(doc_id)
                    .execute(&state.pool)
                    .await
                    .map_err(AppError::Db)?;
                let chunks = result.get("chunks").and_then(|c| c.as_i64()).unwrap_or(0);
                results.push(serde_json::json!({
                    "title": doc.title, "status": "indexed", "chunks": chunks,
                }));
            }
            Err(e) => {
                let _ =
                    sqlx::query("UPDATE knowledge_documents SET status = 'error' WHERE id = $1")
                        .bind(doc_id)
                        .execute(&state.pool)
                        .await;
                results.push(serde_json::json!({
                    "title": doc.title, "status": "error", "error": e.to_string(),
                }));
            }
        }
    }

    job.processed = end;
    let done = job.processed >= docs.len();
    if done {
        let _ = state.object_storage.delete(&docs_key);
        let _ = state.object_storage.delete(&state_key);
    } else {
        let job_json = serde_json::to_vec(&job).map_err(|e| AppError::Internal(e.to_string()))?;
        state
            .object_storage
            .put(&state_key, &job_json)
            .map_err(|e| AppError::Internal(format!("Could not save progress: {e}")))?;
    }

    Ok(Json(serde_json::json!({
        "job_id": job_id,
        "processed": job.processed,
        "total": docs.len(),
        "done": done,
        "results": results,
    })))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(start_import))
        .route("/{job_id}/batch", post(process_batch))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_row(csv_body: &str) -> LcdRow {
        let rows = read_csv_rows(csv_body.as_bytes()).unwrap();
        rows.into_iter().next().unwrap()
    }

    #[test]
    fn strips_markup_from_title_as_well_as_body_fields() {
        let row = one_row(
            "lcd_id,title,indication,status\n\
             33967,Vitamin B<sub>12</sub> Injections,<p>Covered when medically necessary.</p>,A\n",
        );
        let doc = build_document(&row);
        // A tag boundary always becomes a space (see strip_html), even an
        // inline one like <sub> - a harmless extra space here is the
        // trade-off for never running two list items or paragraphs
        // together into one unreadable word.
        assert_eq!(doc.title, "Vitamin B 12 Injections");
        assert!(!doc.content.contains('<'), "content: {}", doc.content);
    }

    #[test]
    fn adjacent_list_items_stay_separated_instead_of_running_together() {
        let row = one_row(
            "lcd_id,title,indication,status\n\
             33252,Test,<p>Covered:</p><ul><li>Condition A</li><li>Condition B</li></ul>,A\n",
        );
        let doc = build_document(&row);
        assert!(
            doc.content.contains("Condition A") && doc.content.contains("Condition B"),
            "content: {}",
            doc.content
        );
        assert!(
            !doc.content.contains("Condition ACondition B"),
            "list items ran together: {}",
            doc.content
        );
    }

    #[test]
    fn decodes_entities_beyond_the_five_basic_ones() {
        let row = one_row(
            "lcd_id,title,indication,status\n\
             33252,Test,HbA1c &ge; 9&#37; and age &lt; 65 &mdash; the patient&rsquo;s history,A\n",
        );
        let doc = build_document(&row);
        assert!(
            doc.content.contains("HbA1c ≥ 9% and age < 65"),
            "content: {}",
            doc.content
        );
        assert!(
            doc.content.contains("patient’s history"),
            "content: {}",
            doc.content
        );
    }

    #[test]
    fn recognises_a_jet_database_by_its_signature_regardless_of_filename() {
        let mut raw = vec![0u8; 4];
        raw.extend_from_slice(b"Standard Jet DB");
        assert!(looks_like_jet_database(&raw));

        let mut ace = vec![0u8; 4];
        ace.extend_from_slice(b"Standard ACE DB");
        assert!(looks_like_jet_database(&ace));

        assert!(!looks_like_jet_database(b"lcd_id,title,status\n"));
    }

    #[test]
    fn converts_an_access_timestamp_to_the_csv_exports_date_format() {
        // 2015-10-01, the same date the CSV path renders as
        // "2015-10-01 00:00:00" (see the module doc's worked example).
        let days_since_1899_12_30 = 42278.0;
        assert_eq!(
            mdb_value_to_string(&jetdb::Value::Timestamp(days_since_1899_12_30)),
            "2015-10-01 00:00:00"
        );
        assert_eq!(mdb_value_to_string(&jetdb::Value::Null), "");
        assert_eq!(mdb_value_to_string(&jetdb::Value::Long(33252)), "33252");
    }
}
