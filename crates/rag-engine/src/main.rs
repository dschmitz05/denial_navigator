//! RAG Engine Service — embedding generation, vector store, and semantic
//! retrieval. Ported from `rag-engine/main.py`.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::postgres::PgPool;
use sqlx::{QueryBuilder, Row};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

use denial_ai::embed::{
    format_vector, EmbedKind, EmbeddingProvider, OpenAiCompatibleEmbeddingProvider, TaskPrefixes,
};
use denial_ai::prompt::{build_denial_prompt, DenialPromptInput, PhiDisclosureLevel};
use denial_common::config::{env_bool, env_f64, env_or, env_required_secret, env_usize};
use denial_common::error::AppError;
use denial_db::db;
use denial_knowledge::chunk_text;

/// Environment configuration for this service.
#[derive(Clone)]
struct Config {
    llama_base_url: String,
    embedding_model: String,
    embed_prefixes: TaskPrefixes,
    embed_base_url: String,
    chunk_chars: usize,
    chunk_overlap: usize,
    min_similarity: f64,
    embed_batch: usize,
    vector_search_enabled: bool,
    phi_disclosure_level: PhiDisclosureLevel,
    internal_service_api_key: String,
    cors_origins: Vec<String>,
}

impl Config {
    fn from_env() -> Self {
        let embedding_model = env_or("EMBEDDING_MODEL", "nomic-embed-text");
        // Unset or empty falls back to the model's trained prefixes, so
        // compose can pass `${VAR:-}` through; "none" switches a prefix off.
        let defaults = TaskPrefixes::for_model(&embedding_model);
        let prefix = |key: &str, default: String| match env_or(key, "").as_str() {
            "" => default,
            "none" => String::new(),
            value => value.to_string(),
        };
        let embed_prefixes = TaskPrefixes {
            query: prefix("EMBED_QUERY_PREFIX", defaults.query),
            document: prefix("EMBED_DOCUMENT_PREFIX", defaults.document),
        };
        Self {
            llama_base_url: env_or("LLAMA_BASE_URL", "http://localhost:8080"),
            embedding_model,
            embed_prefixes,
            // Embeddings do NOT come from LLAMA_BASE_URL. That server runs the
            // chat model and answers /v1/embeddings with 501. This is the
            // dedicated llama.cpp embedding server (768 dims, matching the
            // vector(768) column and its ivfflat cosine index).
            embed_base_url: env_or("EMBED_BASE_URL", "http://10.10.10.98:8081"),
            chunk_chars: env_usize("CHUNK_CHARS", 1500),
            chunk_overlap: env_usize("CHUNK_OVERLAP", 200),
            min_similarity: env_f64("MIN_SIMILARITY", 0.25),
            embed_batch: env_usize("EMBED_BATCH", 16),
            vector_search_enabled: env_bool("VECTOR_SEARCH_ENABLED", true),
            phi_disclosure_level: PhiDisclosureLevel::parse(&env_or(
                "AI_PHI_DISCLOSURE_LEVEL",
                "deidentified",
            ))
            .unwrap_or(PhiDisclosureLevel::DEFAULT),
            internal_service_api_key: env_required_secret("RAG_INTERNAL_API_KEY"),
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
    embeddings: OpenAiCompatibleEmbeddingProvider,
}

// ── Search ──

/// Semantic search: embed the query, then cosine-rank chunks in pgvector.
/// Which documents a search may return.
struct Scope<'a> {
    organization_id: uuid::Uuid,
    source_type: Option<&'a str>,
    payer: Option<&'a str>,
    payer_id_number: Option<&'a str>,
    jurisdiction: Option<&'a str>,
    effective_on: Option<&'a str>,
}

/// Appends the scope conditions shared by vector and lexical search.
///
/// Payer-agnostic documents (NULL payer_name) stay in scope. A document also
/// matches when its payer name and the claim's payer (by name or payer ID)
/// are aliases of the same payer, so one payer spelled several ways, or
/// named by ID, still finds its policies (FB-11). The date of service keeps
/// out documents not in effect then.
fn push_scope(qb: &mut QueryBuilder<'_, sqlx::Postgres>, scope: &Scope<'_>) {
    qb.push(" AND kd.organization_id = ");
    qb.push_bind(scope.organization_id);
    if let Some(st) = scope.source_type {
        qb.push(" AND kd.source_type = ");
        qb.push_bind(st.to_string());
    }
    if let Some(p) = scope.payer {
        qb.push(" AND (kd.payer_name IS NULL OR lower(kd.payer_name) = lower(");
        qb.push_bind(p.to_string());
        qb.push(
            ") OR normalize_payer_name(kd.payer_name) IN (\
                 SELECT a2.alias_normalized FROM payer_aliases a1 \
                 JOIN payer_aliases a2 ON a2.payer_id = a1.payer_id \
                 WHERE a1.organization_id = ",
        );
        qb.push_bind(scope.organization_id);
        qb.push(" AND a1.alias_normalized IN (normalize_payer_name(");
        qb.push_bind(p.to_string());
        qb.push("), normalize_payer_name(");
        qb.push_bind(scope.payer_id_number.unwrap_or_default().to_string());
        qb.push("))))");
    }
    if let Some(j) = scope.jurisdiction {
        qb.push(" AND lower(COALESCE(kd.metadata->>'jurisdiction', '')) = lower(");
        qb.push_bind(j.to_string());
        qb.push(")");
    }
    if let Some(date) = scope.effective_on {
        // Cast: bound as text, compared with DATE columns.
        qb.push(" AND (kd.effective_date IS NULL OR kd.effective_date <= ");
        qb.push_bind(date.to_string());
        qb.push("::date) AND (kd.expiration_date IS NULL OR kd.expiration_date >= ");
        qb.push_bind(date.to_string());
        qb.push("::date)");
    }
}

async fn search_similar(
    state: &AppState,
    query: &str,
    top_k: i64,
    scope: &Scope<'_>,
) -> Result<Vec<Value>, AppError> {
    let embeddings = state
        .embeddings
        .embed(EmbedKind::Query, &[query.to_string()])
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
        "::vector) AS similarity_score, \
         ts_rank_cd(to_tsvector('english', kc.content), websearch_to_tsquery('english', ",
    );
    qb.push_bind(query.to_string());
    qb.push(
        ")) AS keyword_score \
         FROM knowledge_chunks kc \
         JOIN knowledge_documents kd ON kd.id = kc.knowledge_document_id \
         WHERE kc.embedding IS NOT NULL \
         AND kd.status <> 'archived'",
    );
    push_scope(&mut qb, scope);
    qb.push(" AND (1 - (kc.embedding <=> ");
    qb.push_bind(vec.clone());
    qb.push("::vector) >= ");
    qb.push_bind(state.cfg.min_similarity);
    qb.push(" OR to_tsvector('english', kc.content) @@ websearch_to_tsquery('english', ");
    qb.push_bind(query.to_string());
    qb.push(")) ORDER BY (0.7 * (1 - (kc.embedding <=> ");
    qb.push_bind(vec);
    qb.push("::vector)) + 0.3 * LEAST(ts_rank_cd(to_tsvector('english', kc.content), websearch_to_tsquery('english', ");
    qb.push_bind(query.to_string());
    qb.push(")), 1.0)) DESC LIMIT ");
    qb.push_bind(top_k);

    let rows = qb.build().fetch_all(&state.pool).await?;
    let mut results = Vec::with_capacity(rows.len());
    for r in &rows {
        let metadata = r.try_get::<Value, _>("metadata").unwrap_or(Value::Null);
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
            "keyword_score": r.try_get::<f32, _>("keyword_score").unwrap_or(0.0),
        }));
    }
    tracing::info!(
        "Vector search '{}' -> {} chunks",
        &query.chars().take(40).collect::<String>(),
        results.len()
    );
    Ok(results)
}

/// Lexical fallback for deployments that intentionally disable pgvector or do
/// not have an embedding endpoint available yet.
async fn search_lexical(
    state: &AppState,
    query: &str,
    top_k: i64,
    scope: &Scope<'_>,
) -> Result<Vec<Value>, AppError> {
    let mut qb = QueryBuilder::<sqlx::Postgres>::default();
    qb.push("SELECT kc.id, kc.knowledge_document_id, kc.chunk_index, kc.content, kc.token_count, kc.metadata, kd.title AS document_title, kd.source_type, 0.0::float8 AS similarity_score, ts_rank_cd(to_tsvector('english', kc.content), websearch_to_tsquery('english', ");
    qb.push_bind(query.to_string());
    qb.push(")) AS keyword_score FROM knowledge_chunks kc JOIN knowledge_documents kd ON kd.id = kc.knowledge_document_id WHERE kd.status <> 'archived' AND to_tsvector('english', kc.content) @@ websearch_to_tsquery('english', ");
    qb.push_bind(query.to_string());
    qb.push(")");
    push_scope(&mut qb, scope);
    qb.push(" ORDER BY keyword_score DESC LIMIT ");
    qb.push_bind(top_k);
    let rows = qb.build().fetch_all(&state.pool).await?;
    Ok(rows.iter().map(|r| json!({
        "id": r.try_get::<uuid::Uuid, _>("id").map(|u| u.to_string()).unwrap_or_default(),
        "knowledge_document_id": r.try_get::<uuid::Uuid, _>("knowledge_document_id").map(|u| u.to_string()).unwrap_or_default(),
        "chunk_index": r.try_get::<i32, _>("chunk_index").unwrap_or(0),
        "content": r.try_get::<String, _>("content").unwrap_or_default(),
        "token_count": r.try_get::<i32, _>("token_count").unwrap_or(0),
        "metadata": r.try_get::<Value, _>("metadata").unwrap_or(Value::Null),
        "document_title": r.try_get::<Option<String>, _>("document_title").ok(),
        "source_type": r.try_get::<Option<String>, _>("source_type").ok(),
        "similarity_score": 0.0,
        "keyword_score": r.try_get::<f32, _>("keyword_score").unwrap_or(0.0),
    })).collect())
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
    jurisdiction: Option<String>,
    effective_on: Option<String>,
    organization_id: Option<uuid::Uuid>,
    /// The claim's payer ID (835 N1*PR / 837 NM1*PR), tried as an alias too.
    payer_id_number: Option<String>,
}

#[derive(Deserialize)]
struct IngestDocumentRequest {
    document_id: String,
    content: Option<String>,
    #[serde(default)]
    sections: Vec<IngestSection>,
}

#[derive(Deserialize)]
struct IngestSection {
    content: String,
    page: Option<usize>,
    section: Option<String>,
}

#[derive(Deserialize)]
struct PromptRequest {
    /// A payer that pays after this one, when the claim names one.
    #[serde(default)]
    next_payer_name: Option<String>,
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
    /// Optional per-request override of the configured disclosure level, set by
    /// the gateway from the admin-configured value. Falls back to the
    /// service's configured level when absent or unparseable.
    #[serde(default)]
    phi_disclosure_level: Option<String>,
}

// ── Handlers ──

/// Dimensions of `knowledge_chunks.embedding`. A provider returning any other
/// length would fail every chunk insert and every search.
const EMBEDDING_DIMENSIONS: usize = 768;
const EMBEDDING_CHECK_TTL: Duration = Duration::from_secs(30);

static EMBEDDING_CHECK: OnceLock<Mutex<Option<(Instant, Value)>>> = OnceLock::new();

/// The vector length the provider returns, or why it cannot be used.
fn check_embedding(result: Result<Vec<Vec<f64>>, AppError>) -> Result<usize, String> {
    let vectors = result.map_err(|error| error.to_string())?;
    let dimensions = vectors
        .first()
        .map(Vec::len)
        .ok_or("embedding provider returned no vector")?;
    if dimensions == EMBEDDING_DIMENSIONS {
        Ok(dimensions)
    } else {
        Err(format!(
            "embedding provider returned {dimensions} dimensions; the index needs {EMBEDDING_DIMENSIONS}"
        ))
    }
}

/// Embeds a probe string, so a stopped server, a wrong URL or a model with the
/// wrong dimensions is reported here instead of when a document is uploaded.
async fn embedding_status(state: &AppState) -> Value {
    if !state.cfg.vector_search_enabled {
        return json!({"status": "disabled", "detail": "VECTOR_SEARCH_ENABLED=false; lexical search only"});
    }
    let cache = EMBEDDING_CHECK.get_or_init(|| Mutex::new(None));
    if let Some((at, value)) = cache.lock().unwrap().as_ref() {
        if at.elapsed() < EMBEDDING_CHECK_TTL {
            return value.clone();
        }
    }
    let input = ["health check".to_string()];
    let probe = state.embeddings.embed(EmbedKind::Query, &input);
    let result = match tokio::time::timeout(Duration::from_secs(3), probe).await {
        Ok(result) => check_embedding(result),
        Err(_) => Err("embedding provider did not answer within 3 seconds".into()),
    };
    let value = match result {
        Ok(dimensions) => json!({
            "status": "ok",
            "detail": format!("{dimensions}-dimension vectors from {}", state.cfg.embedding_model),
        }),
        Err(reason) => json!({"status": "down", "detail": reason}),
    };
    *cache.lock().unwrap() = Some((Instant::now(), value.clone()));
    value
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let embedding = embedding_status(&state).await;
    Json(json!({
        "status": "healthy",
        "embedding": embedding,
        "embedding_model": state.cfg.embedding_model,
        "embedding_prefixes": {
            "query": state.cfg.embed_prefixes.query,
            "document": state.cfg.embed_prefixes.document,
        },
        "vector_search_enabled": state.cfg.vector_search_enabled,
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
    let organization_id = filters.organization_id.ok_or(AppError::Forbidden)?;
    let scope = Scope {
        organization_id,
        source_type: filters.source_type.as_deref(),
        payer: filters.payer.as_deref(),
        payer_id_number: filters.payer_id_number.as_deref(),
        jurisdiction: filters.jurisdiction.as_deref(),
        effective_on: filters.effective_on.as_deref(),
    };
    let results = if state.cfg.vector_search_enabled {
        search_similar(&state, &req.query, req.top_k, &scope).await?
    } else {
        search_lexical(&state, &req.query, req.top_k, &scope).await?
    };
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

    let input_sections = if req.sections.is_empty() {
        vec![IngestSection {
            content: req.content.unwrap_or_default(),
            page: None,
            section: None,
        }]
    } else {
        req.sections
    };
    let mut chunks = Vec::new();
    for input in input_sections {
        for content in chunk_text(
            &input.content,
            state.cfg.chunk_chars,
            state.cfg.chunk_overlap,
        ) {
            chunks.push((content, input.page, input.section.clone()));
        }
    }
    if chunks.is_empty() {
        return Err(AppError::BadRequest("Document content is empty".into()));
    }

    let vectors = if state.cfg.vector_search_enabled {
        tracing::info!("Embedding {} chunks for document {doc_id}", chunks.len());
        let vectors = state
            .embeddings
            .embed(
                EmbedKind::Document,
                &chunks
                    .iter()
                    .map(|(content, _, _)| content.clone())
                    .collect::<Vec<_>>(),
            )
            .await?;
        if vectors.len() != chunks.len() {
            return Err(AppError::Upstream(format!(
                "Embedding backend returned {} vectors for {} chunks",
                vectors.len(),
                chunks.len()
            )));
        }
        Some(vectors)
    } else {
        tracing::info!(
            "Indexing {} chunks without embeddings for document {doc_id}",
            chunks.len()
        );
        None
    };

    let mut tx = state.pool.begin().await?;
    // Re-ingesting a document replaces its chunks rather than duplicating them.
    sqlx::query("DELETE FROM knowledge_chunks WHERE knowledge_document_id = $1")
        .bind(doc_id)
        .execute(&mut *tx)
        .await?;
    for (i, (chunk, page, section)) in chunks.iter().enumerate() {
        let metadata =
            json!({ "chars": chunk.chars().count(), "page": page, "section": section }).to_string();
        let token_count = chunk.split_whitespace().count() as i32;
        sqlx::query(
            "INSERT INTO knowledge_chunks \
             (knowledge_document_id, chunk_index, content, embedding, metadata, token_count) \
             VALUES ($1, $2, $3, $4::vector, $5::jsonb, $6)",
        )
        .bind(doc_id)
        .bind(i as i32)
        .bind(chunk)
        .bind(vectors.as_ref().map(|items| format_vector(&items[i])))
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
        "dimensions": vectors.as_ref().and_then(|items| items.first()).map(Vec::len),
        "vector_search_enabled": state.cfg.vector_search_enabled,
    })))
}

async fn build_prompt(
    State(state): State<AppState>,
    Json(req): Json<PromptRequest>,
) -> Json<Value> {
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
        next_payer_name: req.next_payer_name,
        phi_disclosure_level: req
            .phi_disclosure_level
            .as_deref()
            .and_then(PhiDisclosureLevel::parse)
            .unwrap_or(state.cfg.phi_disclosure_level),
    };
    let (system, user) = build_denial_prompt(&input);
    Json(json!({ "system": system, "user": user }))
}

// ── Router / main ──

fn build_router(state: AppState) -> Router {
    let key = state.cfg.internal_service_api_key.clone();
    let protected = Router::new()
        .route("/embed", post(embed_gone))
        .route("/search", post(search_knowledge))
        .route("/ingest-document", post(ingest_document))
        .route("/prompt/denial-analysis", post(build_prompt))
        .layer(axum::middleware::from_fn_with_state(
            key,
            denial_common::internal_auth::require_internal_key,
        ));
    let mut app = Router::new().route("/health", get(health)).merge(protected);

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
    let embeddings = OpenAiCompatibleEmbeddingProvider::new(
        reqwest::Client::new(),
        &cfg.embed_base_url,
        &cfg.embedding_model,
        cfg.embed_batch,
        cfg.embed_prefixes.clone(),
    );
    let state = AppState {
        cfg,
        pool,
        embeddings,
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

#[cfg(test)]
mod health_tests {
    use super::{check_embedding, EMBEDDING_DIMENSIONS};
    use denial_common::error::AppError;

    #[test]
    fn a_768_dimension_vector_is_healthy() {
        assert_eq!(
            check_embedding(Ok(vec![vec![0.0; EMBEDDING_DIMENSIONS]])),
            Ok(EMBEDDING_DIMENSIONS)
        );
    }

    #[test]
    fn wrong_dimensions_name_both_sizes() {
        assert_eq!(
            check_embedding(Ok(vec![vec![0.0; 384]])),
            Err("embedding provider returned 384 dimensions; the index needs 768".into())
        );
    }

    #[test]
    fn provider_errors_and_empty_results_are_unhealthy() {
        assert!(check_embedding(Err(AppError::Upstream(
            "embedding server 404 Not Found".into()
        )))
        .is_err());
        assert!(check_embedding(Ok(vec![])).is_err());
    }
}
