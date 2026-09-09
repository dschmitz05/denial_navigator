//! Knowledge base routes.
//!
//! Ported from `api-gateway/routes/knowledge.py`. Documents are text and PDFs;
//! a PDF is identified by its magic bytes, not the filename or the browser's
//! content-type guess. Indexing is delegated to the rag-engine, which embeds
//! the chunks into pgvector. Retiring a document ARCHIVES it by default (row
//! kept for audit, chunks deleted so it stops steering the LLM); `?purge=true`
//! removes the record entirely.

use axum::extract::{Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, NaiveDate, Utc};
use denial_common::error::AppError;
use denial_common::rbac::{Principal, PrincipalKind};
use serde::Deserialize;
use sqlx::{Column, QueryBuilder, Row};
use uuid::Uuid;

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

// ── Column rendering ────────────────────────────────────────────────────

fn json_value_at(row: &sqlx::postgres::PgRow, name: &str) -> serde_json::Value {
    use serde_json::Value;
    if let Ok(v) = row.try_get::<Option<String>, _>(name) {
        return v.map(Value::String).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<bool>, _>(name) {
        return v.map(Value::from).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<i16>, _>(name) {
        return v.map(Value::from).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<i32>, _>(name) {
        return v.map(Value::from).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<i64>, _>(name) {
        return v.map(Value::from).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<f64>, _>(name) {
        return v.map(Value::from).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<Uuid>, _>(name) {
        return v.map(|u| Value::String(u.to_string())).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<DateTime<Utc>>, _>(name) {
        return v.map(|t| Value::String(t.to_rfc3339())).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<NaiveDate>, _>(name) {
        return v.map(|d| Value::String(d.to_string())).unwrap_or(Value::Null);
    }
    if let Ok(v) = row.try_get::<Option<serde_json::Value>, _>(name) {
        return v.unwrap_or(Value::Null);
    }
    Value::Null
}

fn row_to_json(row: &sqlx::postgres::PgRow) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for col in row.columns().iter() {
        map.insert(col.name().to_string(), json_value_at(row, col.name()));
    }
    serde_json::Value::Object(map)
}

// ── PDF / upload decoding ───────────────────────────────────────────────

/// Extract text from a PDF. Returns `(text, page_count)`, or an actionable
/// 4xx rather than letting an empty extraction be indexed as a silently
/// useless document. `pdf-extract` can panic on malformed input, so the call
/// runs on a blocking thread inside `catch_unwind`.
async fn extract_pdf_text(raw: Vec<u8>) -> Result<(String, usize), AppError> {
    let pages = lopdf::Document::load_mem(&raw)
        .map(|d| d.get_pages().len())
        .unwrap_or(0);

    let text = tokio::task::spawn_blocking(move || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pdf_extract::extract_text_from_mem(&raw)
        }))
    })
    .await
    .map_err(|e| AppError::Internal(format!("PDF extraction task failed: {e}")))?
    .map_err(|_| AppError::BadRequest("Could not read PDF: malformed or unsupported file".into()))?
    .map_err(|e| AppError::BadRequest(format!("Could not read PDF: {e}")))?;

    let compact = text
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    if compact.len() < 50 {
        // Almost certainly a scan: pdf-extract reads glyphs, not pixels.
        // Indexing this would create a document with no retrievable content.
        return Err(AppError::Unprocessable(format!(
            "No extractable text found in this PDF ({pages} pages). It is most likely a scan \
             or image-only PDF, which needs OCR. Upload a text-based PDF, or paste the text \
             directly."
        )));
    }

    Ok((compact, pages))
}

/// Turn an uploaded file into indexable text plus metadata.
async fn decode_upload(raw: Vec<u8>, filename: &str) -> Result<(String, serde_json::Value), AppError> {
    if raw.starts_with(PDF_MAGIC) {
        let (text, pages) = extract_pdf_text(raw).await?;
        let chars = text.chars().count();
        return Ok((text, serde_json::json!({"format": "pdf", "pages": pages, "chars": chars})));
    }

    let text = String::from_utf8(raw).map_err(|_| {
        AppError::BadRequest(format!(
            "{filename} is neither a PDF nor UTF-8 text. Supported: .pdf, .txt, .md"
        ))
    })?;
    if text.trim().is_empty() {
        return Err(AppError::BadRequest("File is empty".into()));
    }
    let chars = text.chars().count();
    Ok((text, serde_json::json!({"format": "text", "chars": chars})))
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
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    50
}

#[derive(Deserialize)]
pub struct UploadQuery {
    pub title: Option<String>,
    #[serde(default = "default_source_type")]
    pub source_type: String,
    pub payer_name: Option<String>,
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
    Query(params): Query<ListDocumentsQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    // chunk_count makes "indexed" verifiable at a glance: a document with 0
    // chunks contributes nothing to retrieval no matter what status says.
    let mut qb = QueryBuilder::<sqlx::Postgres>::new(
        "SELECT kd.*, \
         (SELECT COUNT(*) FROM knowledge_chunks kc WHERE kc.knowledge_document_id = kd.id) \
             AS chunk_count \
         FROM knowledge_documents kd WHERE 1=1",
    );
    if let Some(ref v) = params.source_type {
        qb.push(" AND kd.source_type = ").push_bind(v);
    }
    if let Some(ref v) = params.status {
        qb.push(" AND kd.status = ").push_bind(v);
    }
    qb.push(" ORDER BY kd.created_at DESC LIMIT ")
        .push_bind(params.limit.clamp(1, 500));

    let rows = qb.build().fetch_all(&state.pool).await.map_err(AppError::Db)?;
    Ok(Json(rows.iter().map(row_to_json).collect()))
}

pub async fn create_document(
    State(state): State<AppState>,
    Json(doc): Json<DocumentCreate>,
) -> Result<Response, AppError> {
    let row = sqlx::query(
        "INSERT INTO knowledge_documents \
            (title, source_type, payer_id, payer_name, effective_date, status) \
         VALUES ($1, $2, NULLIF(btrim($3), '')::uuid, NULLIF(btrim($4), ''), $5, 'pending') \
         RETURNING *",
    )
    .bind(&doc.title)
    .bind(&doc.source_type)
    .bind(doc.payer_id.as_deref().unwrap_or(""))
    .bind(doc.payer_name.as_deref().unwrap_or(""))
    .bind(doc.effective_date)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let mut created = row_to_json(&row);
    let doc_id: Uuid = row.try_get("id").map_err(|e| AppError::Internal(e.to_string()))?;

    if let Some(content) = doc.content.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        match state.rag.ingest_document(&doc_id.to_string(), content).await {
            Ok(result) => {
                created["chunks_indexed"] = serde_json::json!(result.get("chunks").and_then(|c| c.as_i64()).unwrap_or(0));
                created["status"] = serde_json::json!("indexed");
            }
            Err(e) => {
                tracing::error!("indexing failed for {doc_id}: {e}");
                let _ = sqlx::query("UPDATE knowledge_documents SET status = 'error' WHERE id = $1")
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
    Path(document_id): Path<Uuid>,
    Json(body): Json<DocumentContent>,
) -> Result<Json<serde_json::Value>, AppError> {
    let exists: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM knowledge_documents WHERE id = $1")
        .bind(document_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::Db)?;
    if exists.is_none() {
        return Err(AppError::NotFound);
    }

    match state.rag.ingest_document(&document_id.to_string(), &body.content).await {
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
        let chunk = field.bytes().await.map_err(|e| AppError::Internal(e.to_string()))?;
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

    let is_pdf = raw.starts_with(PDF_MAGIC);
    let (content, meta) = decode_upload(raw.clone(), &filename).await?;
    let size = raw.len() as i64;

    let row = sqlx::query(
        "INSERT INTO knowledge_documents \
            (title, source_type, payer_name, status, mime_type, file_size_bytes, metadata) \
         VALUES ($1, $2, NULLIF(btrim($3), ''), 'pending', $4, $5, $6::jsonb) \
         RETURNING *",
    )
    .bind(params.title.as_deref().unwrap_or(&filename))
    .bind(&params.source_type)
    .bind(params.payer_name.as_deref().unwrap_or(""))
    .bind(if is_pdf { "application/pdf" } else { "text/plain" })
    .bind(size)
    .bind(meta.to_string())
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let mut created = row_to_json(&row);
    let doc_id: Uuid = row.try_get("id").map_err(|e| AppError::Internal(e.to_string()))?;

    match state.rag.ingest_document(&doc_id.to_string(), &content).await {
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
    Path(document_id): Path<Uuid>,
    Query(params): Query<DeleteQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let doc = sqlx::query("SELECT id, title, status FROM knowledge_documents WHERE id = $1")
        .bind(document_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::Db)?
        .ok_or(AppError::NotFound)?;
    let title: Option<String> = doc.try_get("title").ok().flatten();

    let chunk_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM knowledge_chunks WHERE knowledge_document_id = $1",
    )
    .bind(document_id)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let action = if params.purge {
        sqlx::query("DELETE FROM knowledge_documents WHERE id = $1")
            .bind(document_id)
            .execute(&state.pool)
            .await
            .map_err(AppError::Db)?;
        "purged"
    } else {
        let mut tx = state.pool.begin().await.map_err(AppError::Db)?;
        sqlx::query("DELETE FROM knowledge_chunks WHERE knowledge_document_id = $1")
            .bind(document_id)
            .execute(&mut *tx)
            .await
            .map_err(AppError::Db)?;
        sqlx::query(
            "UPDATE knowledge_documents SET status = 'archived', updated_at = NOW() WHERE id = $1",
        )
        .bind(document_id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Db)?;
        tx.commit().await.map_err(AppError::Db)?;
        "archived"
    };

    tracing::info!("{action} knowledge document {document_id} ({} chunks removed)", chunk_count.0);
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
    let key = limit_key(&principal);
    if !state.knowledge_limiter.allow(&key) {
        return Err(AppError::RateLimited {
            retry_after: state.knowledge_limiter.retry_after(&key),
        });
    }

    let filters = if request.filters.is_null() {
        serde_json::json!({})
    } else {
        request.filters.clone()
    };
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

pub async fn get_document(
    State(state): State<AppState>,
    Path(document_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let doc = sqlx::query(
        "SELECT kd.*, \
         (SELECT COUNT(*) FROM knowledge_chunks kc WHERE kc.knowledge_document_id = kd.id) \
             AS chunk_count \
         FROM knowledge_documents kd WHERE kd.id = $1",
    )
    .bind(document_id)
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
    let joined = chunks
        .iter()
        .map(|c| c.try_get::<Option<String>, _>("content").ok().flatten().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n\n");
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
        .route(
            "/documents/{document_id}",
            get(get_document).delete(delete_document),
        )
        .route("/documents/{document_id}/content", post(add_document_content))
        .route("/search", post(search_knowledge))
}
