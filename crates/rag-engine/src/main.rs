//! RAG Engine Service — embedding generation, vector store, and semantic
//! retrieval. Ported from `rag-engine/main.py`.

mod chunk;
mod embed;
mod prompt;

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::postgres::PgPool;
use sqlx::{Row, QueryBuilder};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

use chunk::chunk_text;
use denial_common::config::{env_f64, env_or, env_usize};
use denial_common::db;
use denial_common::error::AppError;
use embed::{format_vector, generate_embeddings};
use prompt::{build_denial_prompt, DenialPromptInput};

/// Environment configuration for this service.
#[derive(Clone)]
struct Config {
    llama_base_url: String,
    embedding_model: String,
    embed_base_url: String,
    chunk_chars: usize,
    chunk_overlap: usize,
    min_similarity: f64,
    embed_batch: usize,
    cors_origins: Vec<String>,
}

impl Config {
    fn from_env() -> Self {
        Self {
            llama_base_url: env_or("LLAMA_BASE_URL", "http://localhost:8080"),
            embedding_model: env_or("EMBEDDING_MODEL", "nomic-embed-text"),
            // Embeddings do NOT come from LLAMA_BASE_URL. That server runs the
            // chat model and answers /v1/embeddings with 501. This is the
            // dedicated llama.cpp embedding server (768 dims, matching the
            // vector(768) column and its ivfflat cosine index).
            embed_base_url: env_or("EMBED_BASE_URL", "http://10.10.10.98:8081"),
            chunk_chars: env_usize("CHUNK_CHARS", 1500),
            chunk_overlap: env_usize("CHUNK_OVERLAP", 200),
            min_similarity: env_f64("MIN_SIMILARITY", 0.25),
            embed_batch: env_usize("EMBED_BATCH", 16),
            cors_origins: env_or("CORS_ORIGINS", "")
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        }
    }
}

#[derive(Clone)]
struct AppState {
    cfg: Config,
    pool: PgPool,
    http: reqwest::Client,
}

// ── Search ──

/// Semantic search: embed the query, then cosine-rank chunks in pgvector.
async fn search_similar(
    state: &AppState,
    query: &str,
    top_k: i64,
    source_type: Option<&str>,
    payer: Option<&str>,
) -> Result<Vec<Value>, AppError> {
    let embeddings = generate_embeddings(
        &state.http,
        &state.cfg.embed_base_url,
        &state.cfg.embedding_model,
        &[query.to_string()],
        state.cfg.embed_batch,
    )
    .await?;
    if embeddings.is_empty() {
        tracing::warn!("No embedding produced for query; returning no results");
        return Ok(vec![]);
    }
    let vec = format_vector(&embeddings[0]);

    let mut qb = QueryBuilder::<sqlx::Postgres>::default();
    qb.push(
        "SELECT kc.id, kc.knowledge_document_id, kc.chunk_index, kc.content, \
         kc.token_count, kc.metadata, kd.title AS document_title, kd.source_type, \
         1 - (kc.embedding <=> ",
    );
    qb.push_bind(vec.clone());
    qb.push(
        "::vector) AS similarity_score \
         FROM knowledge_chunks kc \
         JOIN knowledge_documents kd ON kd.id = kc.knowledge_document_id \
         WHERE kc.embedding IS NOT NULL \
         AND kd.status <> 'archived'",
    );
    if let Some(st) = source_type {
        qb.push(" AND kd.source_type = ");
        qb.push_bind(st.to_string());
    }
    if let Some(p) = payer {
        // Payer-agnostic documents carry a NULL payer_name and stay in scope;
        // matched loosely on case because payer names arrive in whatever case
        // the payer sends them.
        qb.push(" AND (kd.payer_name IS NULL OR lower(kd.payer_name) = lower(");
        qb.push_bind(p.to_string());
        qb.push("))");
    }
    qb.push(" AND 1 - (kc.embedding <=> ");
    qb.push_bind(vec.clone());
    qb.push("::vector) >= ");
    qb.push_bind(state.cfg.min_similarity);
    qb.push(" ORDER BY kc.embedding <=> ");
    qb.push_bind(vec);
    qb.push("::vector LIMIT ");
    qb.push_bind(top_k);

    let rows = qb.build().fetch_all(&state.pool).await?;
    let mut results = Vec::with_capacity(rows.len());
    for r in &rows {
        let metadata = r
            .try_get::<Value, _>("metadata")
            .unwrap_or(Value::Null);
        results.push(json!({
            "id": r.try_get::<uuid::Uuid, _>("id").map(|u| u.to_string()).unwrap_or_default(),
            "knowledge_document_id": r
                .try_get::<uuid::Uuid, _>("knowledge_document_id")
                .map(|u| u.to_string())
                .unwrap_or_default(),
            "chunk_index": r.try_get::<i32, _>("chunk_index").unwrap_or(0),
            "content": r.try_get::<String, _>("content").unwrap_or_default(),
            "token_count": r.try_get::<i32, _>("token_count").unwrap_or(0),
            "metadata": metadata,
            "document_title": r.try_get::<Option<String>, _>("document_title").ok(),
            "source_type": r.try_get::<Option<String>, _>("source_type").ok(),
            "similarity_score": r.try_get::<f64, _>("similarity_score").unwrap_or(0.0),
        }));
    }
    tracing::info!(
        "Vector search '{}' -> {} chunks",
        &query.chars().take(40).collect::<String>(),
        results.len()
    );
    Ok(results)
}

// ── Request models ──

fn default_top_k() -> i64 {
    5
}

#[derive(Deserialize)]
struct SearchRequest {
    query: String,
    #[serde(default = "default_top_k")]
    top_k: i64,
    filters: Option<Filters>,
}

#[derive(Deserialize, Default)]
struct Filters {
    source_type: Option<String>,
    payer: Option<String>,
}

#[derive(Deserialize)]
struct IngestDocumentRequest {
    document_id: String,
    content: String,
}

#[derive(Deserialize)]
struct PromptRequest {
    claim_id: String,
    payer_name: String,
    cpt_code: String,
    icd10_code: String,
    cagc: String,
    carc_code: String,
    carc_definition: String,
    rarc_code: String,
    rarc_definition: String,
    retrieved_policies: Vec<String>,
}

// ── Handlers ──

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "status": "healthy",
        "embedding_model": state.cfg.embedding_model,
        "llama_url": state.cfg.llama_base_url,
        "embed_url": state.cfg.embed_base_url,
    }))
}

/// `/embed` was a no-op stub that reported success without storing anything.
async fn embed_gone() -> impl IntoResponse {
    (
        StatusCode::GONE,
        Json(json!({
            "detail": "/embed was a no-op stub and has been removed; use /ingest-document"
        })),
    )
}

async fn search_knowledge(
    State(state): State<AppState>,
    Json(req): Json<SearchRequest>,
) -> Result<Json<Value>, AppError> {
    tracing::info!(
        "Searching for: {}...",
        &req.query.chars().take(50).collect::<String>()
    );
    if req.query.trim().is_empty() {
        return Ok(Json(json!({
            "results": Vec::<Value>::new(),
            "query": req.query,
            "top_k": req.top_k,
        })));
    }
    let filters = req.filters.unwrap_or_default();
    let results = search_similar(
        &state,
        &req.query,
        req.top_k,
        filters.source_type.as_deref(),
        filters.payer.as_deref(),
    )
    .await?;
    Ok(Json(json!({
        "results": results,
        "query": req.query,
        "top_k": req.top_k,
    })))
}

async fn ingest_document(
    State(state): State<AppState>,
    Json(req): Json<IngestDocumentRequest>,
) -> Result<Json<Value>, AppError> {
    let doc_id: uuid::Uuid = req
        .document_id
        .parse()
        .map_err(|_| AppError::BadRequest("invalid document_id".into()))?;

    let chunks = chunk_text(&req.content, state.cfg.chunk_chars, state.cfg.chunk_overlap);
    if chunks.is_empty() {
        return Err(AppError::BadRequest("Document content is empty".into()));
    }

    tracing::info!("Embedding {} chunks for document {doc_id}", chunks.len());
    let vectors =
        generate_embeddings(&state.http, &state.cfg.embed_base_url, &state.cfg.embedding_model, &chunks, state.cfg.embed_batch).await?;
    if vectors.len() != chunks.len() {
        return Err(AppError::Upstream(format!(
            "Embedding backend returned {} vectors for {} chunks",
            vectors.len(),
            chunks.len()
        )));
    }

    let mut tx = state.pool.begin().await?;
    // Re-ingesting a document replaces its chunks rather than duplicating them.
    sqlx::query("DELETE FROM knowledge_chunks WHERE knowledge_document_id = $1")
        .bind(doc_id)
        .execute(&mut *tx)
        .await?;
    for (i, (chunk, vec)) in chunks.iter().zip(vectors.iter()).enumerate() {
        let metadata = json!({ "chars": chunk.chars().count() }).to_string();
        let token_count = chunk.split_whitespace().count() as i32;
        sqlx::query(
            "INSERT INTO knowledge_chunks \
             (knowledge_document_id, chunk_index, content, embedding, metadata, token_count) \
             VALUES ($1, $2, $3, $4::vector, $5::jsonb, $6)",
        )
        .bind(doc_id)
        .bind(i as i32)
        .bind(chunk)
        .bind(format_vector(vec))
        .bind(metadata)
        .bind(token_count)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE knowledge_documents SET status = 'indexed', updated_at = NOW() WHERE id = $1",
    )
    .bind(doc_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(json!({
        "status": "indexed",
        "document_id": doc_id.to_string(),
        "chunks": chunks.len(),
        "embedding_model": state.cfg.embedding_model,
        "dimensions": vectors[0].len(),
    })))
}

async fn build_prompt(Json(req): Json<PromptRequest>) -> Json<Value> {
    let input = DenialPromptInput {
        claim_id: req.claim_id,
        payer_name: req.payer_name,
        cpt_code: req.cpt_code,
        icd10_code: req.icd10_code,
        cagc: req.cagc,
        carc_code: req.carc_code,
        carc_definition: req.carc_definition,
        rarc_code: req.rarc_code,
        rarc_definition: req.rarc_definition,
        retrieved_policies: req.retrieved_policies,
    };
    let (system, user) = build_denial_prompt(&input);
    Json(json!({ "system": system, "user": user }))
}

// ── Router / main ──

fn build_router(state: AppState) -> Router {
    let mut app = Router::new()
        .route("/health", get(health))
        .route("/embed", post(embed_gone))
        .route("/search", post(search_knowledge))
        .route("/ingest-document", post(ingest_document))
        .route("/prompt/denial-analysis", post(build_prompt));

    // Reached by the gateway over the Docker network, never a browser, so no
    // CORS by default. Added only if configured.
    if !state.cfg.cors_origins.is_empty() {
        let origins: Vec<axum::http::HeaderValue> = state
            .cfg
            .cors_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        let cors = CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins))
            .allow_methods(Any)
            .allow_headers(Any);
        app = app.layer(cors);
    }

    app.with_state(state)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cfg = Config::from_env();
    let db_cfg = denial_common::config::DbConfig::from_env();
    let pool = db::connect(&db_cfg)
        .await
        .expect("failed to connect to database");
    let state = AppState {
        cfg,
        pool,
        http: reqwest::Client::new(),
    };

    let port = env_or("PORT", "8000");
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind RAG engine port");
    tracing::info!("RAG engine listening on {addr}");
    axum::serve(listener, build_router(state))
        .await
        .expect("RAG engine error");
}
