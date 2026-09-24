//! Knowledge base routes.
//!
//! Ported from `api-gateway/routes/knowledge.py`. Documents are text, PDFs,
//! CSVs and spreadsheets (.xlsx/.xls/.xlsb/.ods); a PDF is identified by its
//! magic bytes, while CSV/spreadsheet formats (text and zip/OLE containers
//! respectively) are identified by filename extension instead. Spreadsheet
//! rows and CSV rows are rendered as batched `header: value` text sections
//! rather than indexed as a raw grid, so retrieval returns coherent rows
//! instead of grid fragments. Indexing is delegated to the rag-engine, which
//! embeds the chunks into pgvector. Retiring a document ARCHIVES it by
//! default (row kept for audit, chunks deleted so it stops steering the
//! LLM); `?purge=true` removes the record entirely.

use axum::extract::{Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, NaiveDate, Utc};
use denial_auth::rbac::{Principal, PrincipalKind};
use denial_common::clients::KnowledgeSection;
use denial_common::error::AppError;
use denial_db::pgjson::row_to_json;
use serde::Deserialize;
use sqlx::{QueryBuilder, Row};
use uuid::Uuid;

use super::scope::organization_id;
use crate::state::AppState;

/// Per-caller key for the in-process rate limiter, matching `ingestion`.
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

const PDF_MAGIC: &[u8] = b"%PDF";
const MAX_DOCUMENT_BYTES: usize = 25 * 1024 * 1024;

// ── PDF / upload decoding ───────────────────────────────────────────────

/// Extract text from a PDF. Returns `(text, page_count)`, or an actionable
/// 4xx rather than letting an empty extraction be indexed as a silently
/// useless document. `pdf-extract` can panic on malformed input, so the call
/// runs on a blocking thread inside `catch_unwind`.
///
/// Falls back to OCR when the PDF has (almost) no extractable text layer -
/// a scan or an image-only PDF, where `pdf-extract` reads glyphs, not
/// pixels, and finds none.
async fn extract_pdf_text(raw: Vec<u8>) -> Result<(Vec<KnowledgeSection>, bool), AppError> {
    let page_text = tokio::task::spawn_blocking({
        let raw = raw.clone();
        move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                pdf_extract::extract_text_from_mem_by_pages(&raw)
            }))
        }
    })
    .await
    .map_err(|e| AppError::Internal(format!("PDF extraction task failed: {e}")))?
    .map_err(|_| AppError::BadRequest("Could not read PDF: malformed or unsupported file".into()))?
    .map_err(|e| AppError::BadRequest(format!("Could not read PDF: {e}")))?;

    let sections: Vec<KnowledgeSection> = page_text
        .into_iter()
        .enumerate()
        .filter_map(|(i, text)| {
            let content = text
                .lines()
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();
            (!content.is_empty()).then_some(KnowledgeSection {
                content,
                page: Some(i + 1),
                section: None,
            })
        })
        .collect();
    let chars: usize = sections.iter().map(|s| s.content.chars().count()).sum();

    if chars >= 50 {
        return Ok((sections, false));
    }

    // Almost certainly a scan. Try OCR before giving up - if that also
    // comes back empty, the PDF is genuinely unreadable (corrupt, or truly
    // blank pages) rather than just lacking a text layer.
    let page_count = sections.len();
    let ocr_sections = ocr_pdf(raw).await?;
    if ocr_sections.is_empty() {
        return Err(AppError::Unprocessable(format!(
            "No extractable text found in this PDF ({page_count} pages), and OCR found \
             nothing readable either. Upload a clearer scan, a text-based PDF, or paste the \
             text directly."
        )));
    }
    Ok((ocr_sections, true))
}

/// Rasterizes each page with poppler's `pdftoppm`, then reads each page
/// image with `tesseract`. Both are external processes - no pure-Rust
/// PDF-rasterization + OCR combination approaches their reliability, so
/// this shells out rather than binding to pdfium/leptonica directly. Needs
/// poppler-utils and tesseract-ocr installed in the image (see
/// Dockerfile.rust's api-gateway stage).
async fn ocr_pdf(raw: Vec<u8>) -> Result<Vec<KnowledgeSection>, AppError> {
    tokio::task::spawn_blocking(move || ocr_pdf_blocking(&raw))
        .await
        .map_err(|e| AppError::Internal(format!("OCR task failed: {e}")))?
}

// A scanned document runs page-at-a-time through two external processes
// within one HTTP request/response cycle; capped well under the reverse
// proxy's read timeout even at a few seconds per page.
const MAX_OCR_PAGES: usize = 60;

fn ocr_pdf_blocking(raw: &[u8]) -> Result<Vec<KnowledgeSection>, AppError> {
    let tmp_dir = tempfile::tempdir()
        .map_err(|e| AppError::Internal(format!("Could not create a temp dir for OCR: {e}")))?;
    let pdf_path = tmp_dir.path().join("input.pdf");
    std::fs::write(&pdf_path, raw)
        .map_err(|e| AppError::Internal(format!("Could not stage the PDF for OCR: {e}")))?;

    let page_prefix = tmp_dir.path().join("page");
    let status = std::process::Command::new("pdftoppm")
        .args(["-png", "-r", "200"])
        .arg(&pdf_path)
        .arg(&page_prefix)
        .status()
        .map_err(|e| AppError::Internal(format!("Could not run pdftoppm for OCR: {e}")))?;
    if !status.success() {
        return Err(AppError::Internal(
            "pdftoppm failed to rasterize the PDF for OCR".into(),
        ));
    }

    let mut page_files: Vec<std::path::PathBuf> = std::fs::read_dir(tmp_dir.path())
        .map_err(|e| AppError::Internal(format!("Could not read OCR temp dir: {e}")))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("png"))
        .collect();
    page_files.sort();

    if page_files.len() > MAX_OCR_PAGES {
        return Err(AppError::Unprocessable(format!(
            "This PDF has {} pages; OCR is limited to {MAX_OCR_PAGES} pages per upload.",
            page_files.len()
        )));
    }

    let mut sections = Vec::new();
    for (i, page_file) in page_files.iter().enumerate() {
        let output = std::process::Command::new("tesseract")
            .arg(page_file)
            .arg("stdout")
            .output()
            .map_err(|e| AppError::Internal(format!("Could not run tesseract for OCR: {e}")))?;
        if !output.status.success() {
            // One unreadable page shouldn't sink the whole document.
            continue;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let content = text
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        if !content.is_empty() {
            sections.push(KnowledgeSection {
                content,
                page: Some(i + 1),
                section: None,
            });
        }
    }
    Ok(sections)
}

/// Rows are batched (rather than one section per row) so a large export
/// doesn't fan out into thousands of tiny chunks; each batch repeats the
/// header for every row so a chunk boundary mid-batch still reads as
/// self-contained key/value pairs instead of orphaned values.
const ROWS_PER_SECTION: usize = 25;

/// Renders a sheet's rows (headers first) as batched `header: value` text
/// sections - friendlier for embedding/retrieval than a raw grid, since a
/// chunk boundary lands between rows rather than mid-row.
fn rows_to_sections(
    headers: &[String],
    rows: impl Iterator<Item = Vec<String>>,
    section_label: Option<&str>,
) -> (Vec<KnowledgeSection>, usize) {
    let mut sections = Vec::new();
    let mut batch = String::new();
    let mut batch_rows = 0usize;
    let mut total_rows = 0usize;

    let flush = |batch: &mut String, batch_rows: &mut usize, sections: &mut Vec<KnowledgeSection>| {
        if *batch_rows > 0 {
            sections.push(KnowledgeSection {
                content: std::mem::take(batch).trim_end().to_string(),
                page: None,
                section: section_label.map(str::to_string),
            });
            *batch_rows = 0;
        }
    };

    for row in rows {
        if row.iter().all(|cell| cell.trim().is_empty()) {
            continue;
        }
        for (i, cell) in row.iter().enumerate() {
            if cell.trim().is_empty() {
                continue;
            }
            let header = headers.get(i).map(String::as_str).unwrap_or("column");
            batch.push_str(header);
            batch.push_str(": ");
            batch.push_str(cell.trim());
            batch.push('\n');
        }
        batch.push('\n');
        batch_rows += 1;
        total_rows += 1;
        if batch_rows >= ROWS_PER_SECTION {
            flush(&mut batch, &mut batch_rows, &mut sections);
        }
    }
    flush(&mut batch, &mut batch_rows, &mut sections);
    (sections, total_rows)
}

/// Parses a CSV upload into batched `header: value` sections via the `csv`
/// crate (already used for LCD/reference-table imports).
fn parse_csv_upload(raw: &[u8]) -> Result<(Vec<KnowledgeSection>, serde_json::Value), AppError> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(raw);
    let headers: Vec<String> = reader
        .headers()
        .map_err(|e| AppError::BadRequest(format!("Could not read CSV header row: {e}")))?
        .iter()
        .map(str::to_string)
        .collect();

    let mut parse_error = None;
    let rows = reader.records().filter_map(|rec| match rec {
        Ok(record) => Some(record.iter().map(str::to_string).collect::<Vec<_>>()),
        Err(e) => {
            parse_error.get_or_insert(e);
            None
        }
    });
    let (sections, total_rows) = rows_to_sections(&headers, rows, None);
    if let Some(e) = parse_error {
        return Err(AppError::BadRequest(format!(
            "Could not parse CSV: {e}"
        )));
    }
    if sections.is_empty() {
        return Err(AppError::BadRequest("CSV has no data rows".into()));
    }
    Ok((
        sections,
        serde_json::json!({"format": "csv", "rows": total_rows, "columns": headers.len()}),
    ))
}

/// Parses a spreadsheet upload (.xlsx/.xls/.xlsb/.ods) into batched
/// `header: value` sections, one section-group per sheet. Format is
/// auto-detected by calamine from the file contents, not the extension.
fn parse_spreadsheet_upload(
    raw: Vec<u8>,
    filename: &str,
) -> Result<(Vec<KnowledgeSection>, serde_json::Value), AppError> {
    use calamine::{open_workbook_auto_from_rs, Reader};

    let mut workbook = open_workbook_auto_from_rs(std::io::Cursor::new(raw))
        .map_err(|e| AppError::BadRequest(format!("Could not read {filename} as a spreadsheet: {e}")))?;

    let mut sections = Vec::new();
    let mut total_rows = 0usize;
    let mut sheet_count = 0usize;
    for sheet_name in workbook.sheet_names().to_owned() {
        let range = match workbook.worksheet_range(&sheet_name) {
            Ok(range) => range,
            Err(_) => continue,
        };
        let mut rows = range.rows();
        let headers: Vec<String> = match rows.next() {
            Some(header_row) => header_row.iter().map(|c| c.to_string()).collect(),
            None => continue,
        };
        let data_rows = rows.map(|row| row.iter().map(|c| c.to_string()).collect::<Vec<_>>());
        let (mut sheet_sections, rows_in_sheet) =
            rows_to_sections(&headers, data_rows, Some(&sheet_name));
        if rows_in_sheet == 0 {
            continue;
        }
        sheet_count += 1;
        total_rows += rows_in_sheet;
        sections.append(&mut sheet_sections);
    }

    if sections.is_empty() {
        return Err(AppError::Unprocessable(
            "No data rows found in this spreadsheet (every sheet is empty or header-only)".into(),
        ));
    }
    Ok((
        sections,
        serde_json::json!({"format": "spreadsheet", "sheets": sheet_count, "rows": total_rows}),
    ))
}

fn has_extension(filename: &str, ext: &str) -> bool {
    filename.to_ascii_lowercase().ends_with(ext)
}

/// Turn an uploaded file into indexable text plus metadata.
async fn decode_upload(
    raw: Vec<u8>,
    filename: &str,
) -> Result<(Vec<KnowledgeSection>, serde_json::Value), AppError> {
    if raw.starts_with(PDF_MAGIC) {
        let (sections, ocr) = extract_pdf_text(raw).await?;
        let pages = sections.len();
        let chars: usize = sections.iter().map(|s| s.content.chars().count()).sum();
        return Ok((
            sections,
            serde_json::json!({
                "format": if ocr { "pdf-ocr" } else { "pdf" },
                "pages": pages,
                "chars": chars,
            }),
        ));
    }

    // Spreadsheets are zip- (xlsx/xlsb/ods) or OLE- (xls) container formats,
    // not text, so they're identified by extension rather than magic bytes
    // or a failed UTF-8 decode.
    if ["xlsx", "xls", "xlsb", "ods"]
        .iter()
        .any(|ext| has_extension(filename, &format!(".{ext}")))
    {
        let filename = filename.to_string();
        return tokio::task::spawn_blocking(move || parse_spreadsheet_upload(raw, &filename))
            .await
            .map_err(|e| AppError::Internal(format!("Spreadsheet parsing task failed: {e}")))?;
    }

    if has_extension(filename, ".csv") {
        return tokio::task::spawn_blocking(move || parse_csv_upload(&raw))
            .await
            .map_err(|e| AppError::Internal(format!("CSV parsing task failed: {e}")))?;
    }

    let text = String::from_utf8(raw).map_err(|_| {
        AppError::BadRequest(format!(
            "{filename} is neither a PDF nor UTF-8 text. Supported: .pdf, .csv, .xlsx, .xls, .ods, .txt, .md"
        ))
    })?;
    if text.trim().is_empty() {
        return Err(AppError::BadRequest("File is empty".into()));
    }
    let chars = text.chars().count();
    Ok((
        vec![KnowledgeSection {
            content: text,
            page: None,
            section: None,
        }],
        serde_json::json!({"format": "text", "chars": chars}),
    ))
}

// ── Request models ─────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DocumentCreate {
    pub title: String,
    pub source_type: String,
    pub payer_id: Option<String>,
    /// NULL/blank means the document applies to every payer (e.g. a CMS
    /// coverage determination).
    pub payer_name: Option<String>,
    pub effective_date: Option<NaiveDate>,
    pub expiration_date: Option<NaiveDate>,
    pub jurisdiction: Option<String>,
    pub version_label: Option<String>,
    pub content: Option<String>,
}

#[derive(Deserialize)]
pub struct DocumentContent {
    pub content: String,
}

#[derive(Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default = "default_top_k")]
    pub top_k: u32,
    #[serde(default)]
    pub filters: serde_json::Value,
}

fn default_top_k() -> u32 {
    5
}

#[derive(Deserialize)]
pub struct ListDocumentsQuery {
    pub source_type: Option<String>,
    pub status: Option<String>,
    /// "expiring_soon" (within `expiring_within_days`, not yet expired) or
    /// "expired_active" (past expiration, not archived — still being used to
    /// argue analyses, since nothing besides the date itself excludes it).
    pub expiry: Option<String>,
    #[serde(default = "default_expiring_within_days")]
    pub expiring_within_days: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    50
}

fn default_expiring_within_days() -> i64 {
    30
}

#[derive(Deserialize)]
pub struct UploadQuery {
    pub title: Option<String>,
    #[serde(default = "default_source_type")]
    pub source_type: String,
    pub payer_name: Option<String>,
    pub effective_date: Option<NaiveDate>,
    pub expiration_date: Option<NaiveDate>,
    pub jurisdiction: Option<String>,
    pub version_label: Option<String>,
}

fn default_source_type() -> String {
    "payer_policy".to_string()
}

#[derive(Deserialize)]
pub struct DeleteQuery {
    #[serde(default)]
    pub purge: bool,
}

// ── Handlers ───────────────────────────────────────────────────────────

pub async fn list_documents(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<ListDocumentsQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let organization_id = organization_id(&principal)?;
    // chunk_count makes "indexed" verifiable at a glance: a document with 0
    // chunks contributes nothing to retrieval no matter what status says.
    let mut qb = QueryBuilder::<sqlx::Postgres>::new(
        "SELECT kd.*, \
         (SELECT COUNT(*) FROM knowledge_chunks kc WHERE kc.knowledge_document_id = kd.id) \
             AS chunk_count, \
         (SELECT s.title FROM knowledge_documents s WHERE s.id = kd.superseded_by) \
             AS superseded_by_title \
         FROM knowledge_documents kd WHERE kd.organization_id = ",
    );
    qb.push_bind(organization_id);
    if let Some(ref v) = params.source_type {
        qb.push(" AND kd.source_type = ").push_bind(v);
    }
    if let Some(ref v) = params.status {
        qb.push(" AND kd.status = ").push_bind(v);
    }
    match params.expiry.as_deref() {
        Some("expiring_soon") => {
            qb.push(" AND kd.status <> 'archived' AND kd.expiration_date IS NOT NULL \
                      AND kd.expiration_date >= CURRENT_DATE AND kd.expiration_date <= CURRENT_DATE + ");
            qb.push_bind(params.expiring_within_days.clamp(1, 3650) as i32);
        }
        Some("expired_active") => {
            qb.push(
                " AND kd.status <> 'archived' AND kd.expiration_date IS NOT NULL \
                      AND kd.expiration_date < CURRENT_DATE",
            );
        }
        _ => {}
    }
    qb.push(" ORDER BY kd.created_at DESC LIMIT ")
        .push_bind(params.limit.clamp(1, 500));

    let rows = qb
        .build()
        .fetch_all(&state.pool)
        .await
        .map_err(AppError::Db)?;
    Ok(Json(rows.iter().map(row_to_json).collect()))
}

pub async fn create_document(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(doc): Json<DocumentCreate>,
) -> Result<Response, AppError> {
    let organization_id = organization_id(&principal)?;
    let row = sqlx::query(
        "INSERT INTO knowledge_documents \
            (organization_id, title, source_type, payer_id, payer_name, effective_date, expiration_date, metadata, status) \
         VALUES ($1, $2, $3, NULLIF(btrim($4), '')::uuid, NULLIF(btrim($5), ''), $6, $7, $8::jsonb, 'pending') \
         RETURNING *",
    )
    .bind(organization_id)
    .bind(&doc.title)
    .bind(&doc.source_type)
    .bind(doc.payer_id.as_deref().unwrap_or(""))
    .bind(doc.payer_name.as_deref().unwrap_or(""))
    .bind(doc.effective_date)
    .bind(doc.expiration_date)
    .bind(serde_json::json!({"jurisdiction": doc.jurisdiction, "version_label": doc.version_label}).to_string())
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let mut created = row_to_json(&row);
    let doc_id: Uuid = row
        .try_get("id")
        .map_err(|e| AppError::Internal(e.to_string()))?;

    if let Some(content) = doc
        .content
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        match state
            .rag
            .ingest_document(&doc_id.to_string(), content)
            .await
        {
            Ok(result) => {
                created["chunks_indexed"] =
                    serde_json::json!(result.get("chunks").and_then(|c| c.as_i64()).unwrap_or(0));
                created["status"] = serde_json::json!("indexed");
            }
            Err(e) => {
                tracing::error!("indexing failed for {doc_id}: {e}");
                let _ =
                    sqlx::query("UPDATE knowledge_documents SET status = 'error' WHERE id = $1")
                        .bind(doc_id)
                        .execute(&state.pool)
                        .await;
                created["status"] = serde_json::json!("error");
                created["index_error"] = serde_json::json!(e.to_string());
            }
        }
    }

    Ok((StatusCode::CREATED, Json(created)).into_response())
}

pub async fn add_document_content(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(document_id): Path<Uuid>,
    Json(body): Json<DocumentContent>,
) -> Result<Json<serde_json::Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let exists: Option<(i32,)> =
        sqlx::query_as("SELECT 1 FROM knowledge_documents WHERE id = $1 AND organization_id = $2")
            .bind(document_id)
            .bind(organization_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(AppError::Db)?;
    if exists.is_none() {
        return Err(AppError::NotFound);
    }

    match state
        .rag
        .ingest_document(&document_id.to_string(), &body.content)
        .await
    {
        Ok(result) => Ok(Json(result)),
        Err(e) => {
            let _ = sqlx::query("UPDATE knowledge_documents SET status = 'error' WHERE id = $1")
                .bind(document_id)
                .execute(&state.pool)
                .await;
            Err(AppError::Upstream(format!("Indexing failed: {e}")))
        }
    }
}

pub async fn upload_document(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<UploadQuery>,
    mut multipart: Multipart,
) -> Result<Response, AppError> {
    let organization_id = organization_id(&principal)?;
    let key = limit_key(&principal);
    if !state.knowledge_limiter.allow(&key) {
        return Err(AppError::RateLimited {
            retry_after: state.knowledge_limiter.retry_after(&key),
        });
    }

    let mut raw: Vec<u8> = Vec::new();
    let mut filename: Option<String> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| AppError::BadRequest("Invalid multipart form data".into()))?
    {
        if field.file_name().is_none() && field.name() != Some("file") {
            continue;
        }
        if filename.is_none() {
            if let Some(name) = field.file_name() {
                filename = Some(name.to_string());
            }
        }
        let chunk = field
            .bytes()
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;
        raw.extend_from_slice(&chunk);
        if raw.len() > MAX_DOCUMENT_BYTES {
            return Err(AppError::BadRequest(format!(
                "File too large: maximum {} MB",
                MAX_DOCUMENT_BYTES / (1024 * 1024)
            )));
        }
    }

    if raw.is_empty() {
        return Err(AppError::BadRequest("File is empty".into()));
    }
    let filename = filename.unwrap_or_else(|| "upload".to_string());

    let (sections, meta) = decode_upload(raw.clone(), &filename).await?;
    let size = raw.len() as i64;
    let mime_type = match meta.get("format").and_then(|v| v.as_str()) {
        Some("pdf") | Some("pdf-ocr") => "application/pdf",
        Some("csv") => "text/csv",
        Some("spreadsheet") => {
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        }
        _ => "text/plain",
    };

    let row = sqlx::query(
        "INSERT INTO knowledge_documents \
            (organization_id, title, source_type, payer_name, effective_date, expiration_date, status, mime_type, file_size_bytes, metadata) \
         VALUES ($1, $2, $3, NULLIF(btrim($4), ''), $5, $6, 'pending', $7, $8, $9::jsonb) \
         RETURNING *",
    )
    .bind(organization_id)
    .bind(params.title.as_deref().unwrap_or(&filename))
    .bind(&params.source_type)
    .bind(params.payer_name.as_deref().unwrap_or(""))
    .bind(params.effective_date)
    .bind(params.expiration_date)
    .bind(mime_type)
    .bind(size)
    .bind(serde_json::json!({"upload": meta, "jurisdiction": params.jurisdiction, "version_label": params.version_label}).to_string())
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let mut created = row_to_json(&row);
    let doc_id: Uuid = row
        .try_get("id")
        .map_err(|e| AppError::Internal(e.to_string()))?;

    use denial_storage::ObjectKey;
    let storage_key = ObjectKey::parse(format!("knowledge/{doc_id}/source"))
        .map_err(|e| AppError::Internal(e.to_string()))?;
    state
        .object_storage
        .put(&storage_key, &raw)
        .map_err(|e| AppError::Internal(format!("Could not persist document source: {e}")))?;
    sqlx::query("UPDATE knowledge_documents SET metadata = metadata || jsonb_build_object('storage_key', $2) WHERE id = $1")
        .bind(doc_id).bind(storage_key.as_str()).execute(&state.pool).await.map_err(AppError::Db)?;

    match state
        .rag
        .ingest_sections(&doc_id.to_string(), &sections)
        .await
    {
        Ok(result) => {
            created["chunks_indexed"] =
                serde_json::json!(result.get("chunks").and_then(|c| c.as_i64()).unwrap_or(0));
            created["status"] = serde_json::json!("indexed");
            created["extracted"] = meta;
        }
        Err(e) => {
            let _ = sqlx::query("UPDATE knowledge_documents SET status = 'error' WHERE id = $1")
                .bind(doc_id)
                .execute(&state.pool)
                .await;
            return Err(AppError::Upstream(format!("Indexing failed: {e}")));
        }
    }

    Ok((StatusCode::CREATED, Json(created)).into_response())
}

pub async fn delete_document(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(document_id): Path<Uuid>,
    Query(params): Query<DeleteQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let doc =
        sqlx::query("SELECT id, title, status, metadata FROM knowledge_documents WHERE id = $1 AND organization_id = $2")
            .bind(document_id)
            .bind(organization_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(AppError::Db)?
            .ok_or(AppError::NotFound)?;
    let title: Option<String> = doc.try_get("title").ok().flatten();
    let storage_key: Option<String> = doc
        .try_get::<Option<serde_json::Value>, _>("metadata")
        .ok()
        .flatten()
        .and_then(|metadata| {
            metadata
                .get("storage_key")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        });

    let chunk_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM knowledge_chunks WHERE knowledge_document_id = $1")
            .bind(document_id)
            .fetch_one(&state.pool)
            .await
            .map_err(AppError::Db)?;

    let action = if params.purge {
        sqlx::query("DELETE FROM knowledge_documents WHERE id = $1 AND organization_id = $2")
            .bind(document_id)
            .bind(organization_id)
            .execute(&state.pool)
            .await
            .map_err(AppError::Db)?;
        if let Some(key) = storage_key {
            use denial_storage::ObjectKey;
            if let Ok(key) = ObjectKey::parse(key) {
                let _ = state.object_storage.delete(&key);
            }
        }
        "purged"
    } else {
        let mut tx = state.pool.begin().await.map_err(AppError::Db)?;
        sqlx::query("DELETE FROM knowledge_chunks WHERE knowledge_document_id = $1")
            .bind(document_id)
            .execute(&mut *tx)
            .await
            .map_err(AppError::Db)?;
        sqlx::query(
            "UPDATE knowledge_documents SET status = 'archived', updated_at = NOW() WHERE id = $1 AND organization_id = $2",
        )
        .bind(document_id)
        .bind(organization_id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Db)?;
        tx.commit().await.map_err(AppError::Db)?;
        "archived"
    };

    tracing::info!(
        "{action} knowledge document {document_id} ({} chunks removed)",
        chunk_count.0
    );
    Ok(Json(serde_json::json!({
        "status": action,
        "document_id": document_id.to_string(),
        "title": title,
        "chunks_removed": chunk_count.0,
    })))
}

pub async fn search_knowledge(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<SearchRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let key = limit_key(&principal);
    if !state.knowledge_limiter.allow(&key) {
        return Err(AppError::RateLimited {
            retry_after: state.knowledge_limiter.retry_after(&key),
        });
    }

    let mut filters = if request.filters.is_null() {
        serde_json::json!({})
    } else {
        request.filters.clone()
    };
    let filter_object = filters
        .as_object_mut()
        .ok_or_else(|| AppError::BadRequest("filters must be an object".into()))?;
    filter_object.insert(
        "organization_id".into(),
        serde_json::Value::String(organization_id.to_string()),
    );
    let result = state
        .rag
        .search(&request.query, request.top_k, filters)
        .await
        .map_err(|e| AppError::Upstream(format!("Search failed: {e}")))?;

    Ok(Json(serde_json::json!({
        "results": result.get("results").cloned().unwrap_or_else(|| serde_json::json!([])),
        "query": request.query,
        "top_k": request.top_k,
    })))
}

fn default_reindex_limit() -> i64 {
    25
}

#[derive(Deserialize)]
pub struct ReindexRequest {
    #[serde(default = "default_reindex_limit")]
    pub limit: i64,
}

/// Re-embeds one batch of the caller's organization's chunks whose embedding
/// provenance no longer matches the running config (FB-13). Call it
/// repeatedly — from Settings, or a cron script — until `done`; the mismatch
/// count in Settings → Service health tells you whether there's any need to.
pub async fn reindex(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<ReindexRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let result = state
        .rag
        .reindex_batch(organization_id, request.limit)
        .await
        .map_err(|e| AppError::Upstream(format!("Re-index failed: {e}")))?;
    Ok(Json(result))
}

fn default_expiry_summary_days() -> i64 {
    30
}

#[derive(Deserialize)]
pub struct ExpirySummaryQuery {
    #[serde(default = "default_expiry_summary_days")]
    pub within_days: i64,
}

/// Counts for the dashboard and Knowledge Base filter (FB-14): documents
/// still governing retrieval that either expire soon or already have,
/// excluding archived documents either way (an archived one is already out
/// of scope, so its expiration date is not this page's problem).
pub async fn expiry_summary(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<ExpirySummaryQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let within_days = params.within_days.clamp(1, 3650) as i32;
    let row = sqlx::query(
        "SELECT \
            count(*) FILTER (WHERE expiration_date >= CURRENT_DATE \
                              AND expiration_date <= CURRENT_DATE + $2) AS expiring_soon, \
            count(*) FILTER (WHERE expiration_date < CURRENT_DATE) AS expired_active \
         FROM knowledge_documents \
         WHERE organization_id = $1 AND status <> 'archived' AND expiration_date IS NOT NULL",
    )
    .bind(organization_id)
    .bind(within_days)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;
    Ok(Json(serde_json::json!({
        "expiring_soon": row.try_get::<i64, _>("expiring_soon").unwrap_or(0),
        "expired_active": row.try_get::<i64, _>("expired_active").unwrap_or(0),
        "within_days": params.within_days.clamp(1, 3650),
    })))
}

#[derive(Deserialize)]
pub struct SupersedeRequest {
    pub new_document_id: Uuid,
}

/// Links a replacement and ensures the old document expires (FB-14). Does
/// not extend an expiration date the document already had — a document set
/// to expire next month is not made to last longer by being superseded
/// today.
pub async fn supersede_document(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(document_id): Path<Uuid>,
    Json(request): Json<SupersedeRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    if request.new_document_id == document_id {
        return Err(AppError::BadRequest(
            "a document cannot supersede itself".into(),
        ));
    }
    let organization_id = organization_id(&principal)?;
    let replacement_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM knowledge_documents WHERE id = $1 AND organization_id = $2)",
    )
    .bind(request.new_document_id)
    .bind(organization_id)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;
    if !replacement_exists {
        return Err(AppError::BadRequest(
            "new_document_id is not a document in this organization".into(),
        ));
    }
    let row = sqlx::query(
        "UPDATE knowledge_documents \
         SET superseded_by = $1, \
             expiration_date = LEAST(COALESCE(expiration_date, CURRENT_DATE), CURRENT_DATE), \
             updated_at = NOW() \
         WHERE id = $2 AND organization_id = $3 \
         RETURNING *",
    )
    .bind(request.new_document_id)
    .bind(document_id)
    .bind(organization_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;
    Ok(Json(row_to_json(&row)))
}

/// Chunks overlap by design (`CHUNK_OVERLAP`, better retrieval at a chunk
/// boundary), so joining them naively for display shows that overlap as
/// duplicated text - a sentence appears to repeat itself mid-paragraph,
/// reading as garbled rather than merely imperfect. Finds the longest
/// matching run between the end of what's accumulated and the start of the
/// next chunk and skips past it, so continuous text stays continuous; a
/// genuine section break (no real overlap) still gets the original "\n\n"
/// separator.
fn merge_overlapping_chunks(chunks: &[String]) -> String {
    const MAX_OVERLAP_CHECK: usize = 400;
    const MIN_OVERLAP: usize = 20;

    let mut result = String::new();
    for chunk in chunks {
        if result.is_empty() {
            result.push_str(chunk);
            continue;
        }
        let tail: Vec<char> = {
            let mut t: Vec<char> = result.chars().rev().take(MAX_OVERLAP_CHECK).collect();
            t.reverse();
            t
        };
        let head: Vec<char> = chunk.chars().take(MAX_OVERLAP_CHECK).collect();
        let max_check = tail.len().min(head.len());

        let overlap = (MIN_OVERLAP..=max_check)
            .rev()
            .find(|&len| tail[tail.len() - len..] == head[..len])
            .unwrap_or(0);

        if overlap > 0 {
            let rest: String = chunk.chars().skip(overlap).collect();
            result.push_str(&rest);
        } else {
            result.push_str("\n\n");
            result.push_str(chunk);
        }
    }
    result
}

pub async fn get_document(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(document_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let doc = sqlx::query(
        "SELECT kd.*, \
         (SELECT COUNT(*) FROM knowledge_chunks kc WHERE kc.knowledge_document_id = kd.id) \
             AS chunk_count \
         FROM knowledge_documents kd WHERE kd.id = $1 AND kd.organization_id = $2",
    )
    .bind(document_id)
    .bind(organization_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;

    let chunks = sqlx::query(
        "SELECT chunk_index, content, token_count \
         FROM knowledge_chunks WHERE knowledge_document_id = $1 ORDER BY chunk_index",
    )
    .bind(document_id)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let created_at: Option<DateTime<Utc>> = doc.try_get("created_at").ok().flatten();
    let chunk_count: i64 = doc.try_get("chunk_count").unwrap_or(0);
    let chunk_texts: Vec<String> = chunks
        .iter()
        .map(|c| {
            c.try_get::<Option<String>, _>("content")
                .ok()
                .flatten()
                .unwrap_or_default()
        })
        .collect();
    let joined = merge_overlapping_chunks(&chunk_texts);
    let content = if chunks.is_empty() {
        "(no content — indexing may still be in progress)".to_string()
    } else {
        joined
    };

    Ok(Json(serde_json::json!({
        "id": document_id.to_string(),
        "title": doc.try_get::<Option<String>, _>("title").ok().flatten(),
        "source_type": doc.try_get::<Option<String>, _>("source_type").ok().flatten(),
        "status": doc.try_get::<Option<String>, _>("status").ok().flatten(),
        "chunk_count": chunk_count,
        "created_at": created_at.map(|t| t.to_rfc3339()),
        "content": content,
        "chunks": chunks.iter().map(row_to_json).collect::<Vec<_>>(),
    })))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/documents", get(list_documents).post(create_document))
        .route("/documents/upload", post(upload_document))
        .route("/documents/expiry-summary", get(expiry_summary))
        .route(
            "/documents/{document_id}",
            get(get_document).delete(delete_document),
        )
        .route(
            "/documents/{document_id}/content",
            post(add_document_content),
        )
        .route(
            "/documents/{document_id}/supersede",
            post(supersede_document),
        )
        .route("/search", post(search_knowledge))
        .route("/reindex", post(reindex))
}

#[cfg(test)]
mod tests {
    use super::{merge_overlapping_chunks, ocr_pdf_blocking, parse_csv_upload};

    #[test]
    fn parses_csv_rows_into_header_value_sections() {
        let csv = "Code,Description,Rate\nA1,Widget,10.50\nA2,Gadget,20.00\n";
        let (sections, meta) = parse_csv_upload(csv.as_bytes()).unwrap();
        assert_eq!(sections.len(), 1);
        assert!(sections[0].content.contains("Code: A1"));
        assert!(sections[0].content.contains("Description: Widget"));
        assert!(sections[0].content.contains("Rate: 10.50"));
        assert!(sections[0].content.contains("Code: A2"));
        assert_eq!(meta["format"], "csv");
        assert_eq!(meta["rows"], 2);
    }

    #[test]
    fn csv_with_no_data_rows_is_rejected() {
        let csv = "Code,Description\n";
        let err = parse_csv_upload(csv.as_bytes()).unwrap_err();
        assert!(format!("{err:?}").contains("no data rows"));
    }

    #[test]
    fn csv_batches_rows_so_a_large_file_does_not_fan_out_one_section_per_row() {
        let mut csv = String::from("Code\n");
        for i in 0..60 {
            csv.push_str(&format!("A{i}\n"));
        }
        let (sections, meta) = parse_csv_upload(csv.as_bytes()).unwrap();
        // 60 rows / 25 rows-per-section = 3 batches (25, 25, 10).
        assert_eq!(sections.len(), 3);
        assert_eq!(meta["rows"], 60);
    }

    fn have_ocr_tools() -> bool {
        std::process::Command::new("pdftoppm")
            .arg("-v")
            .output()
            .is_ok()
            && std::process::Command::new("tesseract")
                .arg("--version")
                .output()
                .is_ok()
    }

    #[test]
    fn ocr_reads_an_image_only_pdf_with_no_text_layer() {
        // poppler-utils/tesseract-ocr are installed in the api-gateway image
        // (see Dockerfile.rust) but not necessarily wherever `cargo test`
        // runs (e.g. this repo's own CI image doesn't have them) - skip
        // rather than fail when they're unavailable, same as this app's
        // convention for other external-process-dependent checks.
        if !have_ocr_tools() {
            eprintln!("skipping: pdftoppm/tesseract not on PATH");
            return;
        }
        // A single blank white page has no text; pdftoppm+tesseract should
        // still run cleanly and correctly find nothing, rather than erroring.
        let pdf = magick_blank_pdf();
        let sections = ocr_pdf_blocking(&pdf).expect("OCR should not error on a blank page");
        assert!(sections.is_empty(), "a blank page should OCR to nothing");
    }

    /// A minimal single-page PDF (no text layer), built by hand so this test
    /// doesn't depend on ImageMagick being installed too.
    fn magick_blank_pdf() -> Vec<u8> {
        b"%PDF-1.1\n\
          1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
          2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
          3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >> endobj\n\
          xref\n\
          0 4\n\
          0000000000 65535 f \n\
          trailer << /Size 4 /Root 1 0 R >>\n\
          startxref\n\
          0\n\
          %%EOF"
            .to_vec()
    }

    #[test]
    fn skips_the_overlap_instead_of_duplicating_it() {
        let chunks = vec![
            "The quick brown fox jumps over the lazy dog near the riverbank".to_string(),
            "over the lazy dog near the riverbank at sunset every evening".to_string(),
        ];
        let merged = merge_overlapping_chunks(&chunks);
        assert_eq!(
            merged,
            "The quick brown fox jumps over the lazy dog near the riverbank at sunset every evening"
        );
    }

    #[test]
    fn keeps_the_separator_when_there_is_no_real_overlap() {
        let chunks = vec![
            "First section content.".to_string(),
            "Second section content.".to_string(),
        ];
        let merged = merge_overlapping_chunks(&chunks);
        assert_eq!(merged, "First section content.\n\nSecond section content.");
    }

    #[test]
    fn a_single_chunk_is_returned_unchanged() {
        let chunks = vec!["Only chunk.".to_string()];
        assert_eq!(merge_overlapping_chunks(&chunks), "Only chunk.");
    }
}
