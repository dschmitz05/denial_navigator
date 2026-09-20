//! Bulk LCD (Local Coverage Determination) CSV import.
//!
//! CMS's bulk LCD export is one CSV with a row per policy - title,
//! indication, coding guidelines, documentation requirements, bibliography,
//! each as HTML-formatted text. The single-document upload path
//! (`POST /knowledge/documents/upload`) treats whatever file it's given as
//! ONE document to chunk and embed, so uploading the raw export would mix
//! every LCD nationwide into a single incoherent document - a category-of-
//! data mismatch, not a size problem (though the export is also comfortably
//! over the per-document 25 MB cap).
//!
//! This parses the CSV server-side, splits it into one document per LCD, and
//! indexes them in admin-driven batches: `POST /` stages the parsed,
//! filtered document list in object storage and returns a job id;
//! `POST /{job_id}/batch` indexes the next `limit` of them and reports
//! progress, to be called repeatedly (mirroring the existing
//! `POST /knowledge/reindex` batch-per-request pattern, rather than either a
//! giant blocking request or a background job queue this app doesn't have).
//! Same shape of document as `scripts/split_lcd_csv.py` produces, so a
//! script-driven and a UI-driven import are interchangeable.

use std::collections::HashSet;

use axum::extract::{Multipart, Path, Query, State};
use axum::routing::post;
use axum::{Extension, Json, Router};
use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use denial_storage::ObjectKey;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use super::scope::organization_id;
use crate::state::AppState;

const MAX_BULK_CSV_BYTES: usize = 100 * 1024 * 1024;

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
    let mut out = String::with_capacity(value.len());
    let mut in_tag = false;
    for c in value.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    decoded
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn field<'a>(headers: &csv::StringRecord, record: &'a csv::StringRecord, name: &str) -> &'a str {
    headers
        .iter()
        .position(|h| h == name)
        .and_then(|i| record.get(i))
        .unwrap_or("")
        .trim()
}

fn build_document(headers: &csv::StringRecord, record: &csv::StringRecord) -> ImportedDoc {
    // Some titles carry markup too, e.g. "Vitamin B<sub>12</sub> Injections".
    let title_raw = field(headers, record, "title");
    let title = if title_raw.is_empty() {
        "(untitled LCD)".to_string()
    } else {
        strip_html(title_raw)
    };
    let mut parts = vec![title.clone()];

    let det_num = field(headers, record, "determination_number");
    let display_id = if det_num.is_empty() {
        field(headers, record, "lcd_id")
    } else {
        det_num
    };
    let eff = field(headers, record, "orig_det_eff_date");
    let rev = field(headers, record, "rev_eff_date");
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
        let raw = field(headers, record, field_name);
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
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| AppError::BadRequest("Invalid multipart form data".into()))?
    {
        if field.file_name().is_none() {
            continue;
        }
        let chunk = field
            .bytes()
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        raw.extend_from_slice(&chunk);
        if raw.len() > MAX_BULK_CSV_BYTES {
            return Err(AppError::BadRequest(format!(
                "File too large: maximum {} MB",
                MAX_BULK_CSV_BYTES / (1024 * 1024)
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

    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(raw.as_slice());
    let headers = reader
        .headers()
        .map_err(|e| AppError::BadRequest(format!("Invalid CSV: {e}")))?
        .clone();
    if !headers.iter().any(|h| h == "title") || !headers.iter().any(|h| h == "status") {
        return Err(AppError::BadRequest(
            "Not a recognised CMS LCD export - missing 'title' or 'status' column".into(),
        ));
    }

    let mut docs: Vec<ImportedDoc> = Vec::new();
    let mut skipped = 0usize;
    for result in reader.records() {
        let Ok(record) = result else {
            skipped += 1;
            continue;
        };
        let status = field(&headers, &record, "status").to_uppercase();
        if !wanted_status.contains(&status) {
            skipped += 1;
            continue;
        }
        let title_lower = field(&headers, &record, "title").to_lowercase();
        if !keywords.is_empty() && !keywords.iter().any(|k| title_lower.contains(k.as_str())) {
            skipped += 1;
            continue;
        }
        let doc = build_document(&headers, &record);
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
        let row = sqlx::query(
            "INSERT INTO knowledge_documents (organization_id, title, source_type, status) \
             VALUES ($1, $2, 'cms_lcd', 'pending') RETURNING id",
        )
        .bind(organization_id)
        .bind(&doc.title)
        .fetch_one(&state.pool)
        .await
        .map_err(AppError::Db)?;
        let doc_id: Uuid = row
            .try_get("id")
            .map_err(|e| AppError::Internal(e.to_string()))?;

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

    #[test]
    fn strips_markup_from_title_as_well_as_body_fields() {
        let mut reader = csv::ReaderBuilder::new().from_reader(
            "lcd_id,title,indication,status\n\
             33967,Vitamin B<sub>12</sub> Injections,<p>Covered when medically necessary.</p>,A\n"
                .as_bytes(),
        );
        let headers = reader.headers().unwrap().clone();
        let record = reader.records().next().unwrap().unwrap();
        let doc = build_document(&headers, &record);
        assert_eq!(doc.title, "Vitamin B12 Injections");
        assert!(!doc.content.contains('<'), "content: {}", doc.content);
    }
}
