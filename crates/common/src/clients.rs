//! HTTP clients for the downstream sibling services.
//!
//! Ported from `api-gateway/services/__init__.py`. Base URLs come from the
//! environment, matching the original defaults.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::{multipart, Client};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{env_required_secret, env_u64};
use crate::error::AppError;

fn env_url(var: &str, default: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| default.to_string())
}

async fn check(resp: reqwest::Response) -> Result<Value, AppError> {
    let status = resp.status();
    // Read the body as bytes first. A sibling service can fail *before* its
    // own handler runs - a tower layer rejecting an oversized body returns
    // 413 with a plain-text body, for instance - and parsing as JSON up front
    // turned every such failure into a bare "error decoding response body",
    // hiding both the status and the reason.
    let body = resp
        .bytes()
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let parsed: Option<Value> = serde_json::from_slice(&body).ok();

    if status.is_success() {
        return parsed.ok_or_else(|| {
            AppError::Upstream(format!(
                "upstream {status} returned a non-JSON body: {}",
                snippet(&body)
            ))
        });
    }

    let msg = parsed
        .as_ref()
        .and_then(|b| b.get("detail"))
        .and_then(|d| d.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| snippet(&body));
    Err(AppError::Upstream(format!("upstream {status}: {msg}")))
}

/// A short, single-line rendering of a non-JSON upstream body, for error text.
fn snippet(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let text = text.trim();
    if text.is_empty() {
        return "<empty body>".to_string();
    }
    let mut out: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(200)
        .collect();
    if text.chars().count() > 200 {
        out.push_str("...");
    }
    out
}

/// A small, process-local circuit breaker for a sibling service. It prevents
/// request pile-ups when a provider is already known to be unavailable.
struct CircuitBreaker {
    consecutive_failures: u32,
    open_until: Option<Instant>,
}

struct Resilience {
    retries: u32,
    timeout: Duration,
    failure_threshold: u32,
    cooldown: Duration,
    circuit: Mutex<CircuitBreaker>,
}

impl Resilience {
    fn new(timeout_secs: u64) -> Self {
        Self {
            retries: env_u64("UPSTREAM_MAX_RETRIES", 2) as u32,
            timeout: Duration::from_secs(timeout_secs),
            failure_threshold: env_u64("UPSTREAM_CIRCUIT_FAILURE_THRESHOLD", 3) as u32,
            cooldown: Duration::from_secs(env_u64("UPSTREAM_CIRCUIT_COOLDOWN_SECS", 30)),
            circuit: Mutex::new(CircuitBreaker {
                consecutive_failures: 0,
                open_until: None,
            }),
        }
    }

    fn allow_request(&self) -> bool {
        let mut circuit = self.circuit.lock().expect("circuit breaker mutex poisoned");
        match circuit.open_until {
            Some(until) if until > Instant::now() => false,
            Some(_) => {
                circuit.open_until = None;
                circuit.consecutive_failures = 0;
                true
            }
            None => true,
        }
    }

    fn record_success(&self) {
        let mut circuit = self.circuit.lock().expect("circuit breaker mutex poisoned");
        circuit.consecutive_failures = 0;
        circuit.open_until = None;
    }

    fn record_failure(&self) {
        let mut circuit = self.circuit.lock().expect("circuit breaker mutex poisoned");
        circuit.consecutive_failures += 1;
        if circuit.consecutive_failures >= self.failure_threshold.max(1) {
            circuit.open_until = Some(Instant::now() + self.cooldown);
        }
    }

    async fn send<F>(&self, request: F) -> Result<Value, AppError>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        if !self.allow_request() {
            return Err(AppError::Upstream(
                "upstream circuit breaker is open; retry after cooldown".into(),
            ));
        }
        for attempt in 0..=self.retries {
            let (result, retryable) = match request().timeout(self.timeout).send().await {
                Ok(resp) => {
                    let retryable = resp.status().is_server_error();
                    (check(resp).await, retryable)
                }
                Err(error) => (Err(AppError::Upstream(error.to_string())), true),
            };
            match result {
                Ok(value) => {
                    self.record_success();
                    return Ok(value);
                }
                Err(error) if retryable && attempt < self.retries => {
                    tokio::time::sleep(Duration::from_millis(100 * (1_u64 << attempt))).await;
                    tracing::warn!(attempt, "retrying failed upstream request: {error}");
                }
                Err(error) => {
                    self.record_failure();
                    return Err(error);
                }
            }
        }
        unreachable!("retry loop always returns")
    }
}

/// Client for the EDI Parser service.
#[derive(Clone)]
pub struct EDIParserClient {
    client: Client,
    base_url: String,
    internal_service_api_key: String,
}

impl EDIParserClient {
    pub fn new(base_url: Option<&str>) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url
                .map(|s| s.to_string())
                .unwrap_or_else(|| env_url("EDIPARSER_SERVICE_URL", "http://localhost:8001")),
            internal_service_api_key: env_required_secret("EDIPARSER_INTERNAL_API_KEY"),
        }
    }

    pub async fn parse_file(&self, file_data: &[u8]) -> Result<Value, AppError> {
        let form = multipart::Form::new().part(
            "file",
            multipart::Part::bytes(file_data.to_vec())
                .file_name("upload.835")
                .mime_str("text/plain")?,
        );
        let resp = self
            .client
            .post(format!("{}/ingest", self.base_url))
            .header("X-Internal-Service-Key", &self.internal_service_api_key)
            .timeout(Duration::from_secs(60))
            .multipart(form)
            .send()
            .await?;
        check(resp).await
    }

    /// Forward an S3-compatible bucket's event notification payload so the
    /// matching object is picked up immediately instead of waiting for the
    /// next poll. The gateway has already authenticated the external caller;
    /// this call reuses the same internal service credential as `parse_file`.
    pub async fn notify_s3_event(&self, payload: &Value) -> Result<Value, AppError> {
        let resp = self
            .client
            .post(format!("{}/s3-events", self.base_url))
            .header("X-Internal-Service-Key", &self.internal_service_api_key)
            .timeout(Duration::from_secs(30))
            .json(payload)
            .send()
            .await?;
        check(resp).await
    }

    pub async fn ingest_dropzone(&self) -> Result<Value, AppError> {
        let resp = self
            .client
            .post(format!("{}/ingest-dropzone", self.base_url))
            .timeout(Duration::from_secs(60))
            .send()
            .await?;
        check(resp).await
    }
}

impl Default for EDIParserClient {
    fn default() -> Self {
        Self::new(None)
    }
}

/// Client for the RAG Engine service.
#[derive(Clone)]
pub struct RAGEngineClient {
    client: Client,
    base_url: String,
    internal_service_api_key: String,
    resilience: Arc<Resilience>,
}

/// A separately-addressable source region for knowledge indexing. PDF uploads
/// use one section per page so retrieval results can cite their source page.
/// Deserializable too: a batched ingest stages the parsed sections in object
/// storage and reads them back one batch per request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeSection {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
}

impl RAGEngineClient {
    pub fn new(base_url: Option<&str>) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url
                .map(|s| s.to_string())
                .unwrap_or_else(|| env_url("RAG_ENGINE_URL", "http://localhost:8002")),
            internal_service_api_key: env_required_secret("RAG_INTERNAL_API_KEY"),
            resilience: Arc::new(Resilience::new(env_u64("RAG_REQUEST_TIMEOUT_SECS", 30))),
        }
    }

    pub async fn search(&self, query: &str, top_k: u32, filters: Value) -> Result<Value, AppError> {
        let body = serde_json::json!({ "query": query, "top_k": top_k, "filters": filters });
        self.resilience
            .send(|| {
                self.client
                    .post(format!("{}/search", self.base_url))
                    .header("X-Internal-Service-Key", &self.internal_service_api_key)
                    .json(&body)
            })
            .await
    }

    /// Embedding a long document is slow; give it real time.
    pub async fn ingest_document(
        &self,
        document_id: &str,
        content: &str,
    ) -> Result<Value, AppError> {
        let body = serde_json::json!({ "document_id": document_id, "content": content });
        let resp = self
            .client
            .post(format!("{}/ingest-document", self.base_url))
            .header("X-Internal-Service-Key", &self.internal_service_api_key)
            .timeout(Duration::from_secs(600))
            .json(&body)
            .send()
            .await?;
        check(resp).await
    }

    pub async fn ingest_sections(
        &self,
        document_id: &str,
        sections: &[KnowledgeSection],
    ) -> Result<Value, AppError> {
        let body = serde_json::json!({ "document_id": document_id, "sections": sections });
        let resp = self
            .client
            .post(format!("{}/ingest-document", self.base_url))
            .header("X-Internal-Service-Key", &self.internal_service_api_key)
            .timeout(Duration::from_secs(600))
            .json(&body)
            .send()
            .await?;
        check(resp).await
    }

    /// Embeds one batch of a document's sections, appending to whatever
    /// earlier batches already indexed. Only the last batch may `finalize`,
    /// so an interrupted run leaves the document `pending` instead of
    /// looking complete with half its chunks.
    pub async fn ingest_sections_batch(
        &self,
        document_id: &str,
        sections: &[KnowledgeSection],
        chunk_index_offset: i32,
        replace_existing: bool,
        finalize: bool,
    ) -> Result<Value, AppError> {
        let body = serde_json::json!({
            "document_id": document_id,
            "sections": sections,
            "chunk_index_offset": chunk_index_offset,
            "replace_existing": replace_existing,
            "finalize": finalize,
        });
        let resp = self
            .client
            .post(format!("{}/ingest-document", self.base_url))
            .header("X-Internal-Service-Key", &self.internal_service_api_key)
            .timeout(Duration::from_secs(600))
            .json(&body)
            .send()
            .await?;
        check(resp).await
    }

    /// Re-embeds one batch of an organization's chunks with stale embedding
    /// provenance (FB-13). Re-embedding is slow, like `ingest_document`.
    pub async fn reindex_batch(
        &self,
        organization_id: uuid::Uuid,
        limit: i64,
    ) -> Result<Value, AppError> {
        let body = serde_json::json!({ "organization_id": organization_id, "limit": limit });
        let resp = self
            .client
            .post(format!("{}/reindex-batch", self.base_url))
            .header("X-Internal-Service-Key", &self.internal_service_api_key)
            .timeout(Duration::from_secs(600))
            .json(&body)
            .send()
            .await?;
        check(resp).await
    }

    pub async fn build_prompt(&self, payload: &Value) -> Result<Value, AppError> {
        self.resilience
            .send(|| {
                self.client
                    .post(format!("{}/prompt/denial-analysis", self.base_url))
                    .header("X-Internal-Service-Key", &self.internal_service_api_key)
                    .json(payload)
            })
            .await
    }
}

impl Default for RAGEngineClient {
    fn default() -> Self {
        Self::new(None)
    }
}

/// Client for the LLM Service.
#[derive(Clone)]
pub struct LLMServiceClient {
    client: Client,
    base_url: String,
    internal_service_api_key: String,
    resilience: Arc<Resilience>,
}

impl LLMServiceClient {
    pub fn new(base_url: Option<&str>) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url
                .map(|s| s.to_string())
                .unwrap_or_else(|| env_url("LLM_SERVICE_URL", "http://localhost:8003")),
            internal_service_api_key: env_required_secret("LLM_INTERNAL_API_KEY"),
            resilience: Arc::new(Resilience::new(env_u64("LLM_REQUEST_TIMEOUT_SECS", 180))),
        }
    }

    pub async fn chat(
        &self,
        system: &str,
        user: &str,
        temperature: f32,
    ) -> Result<Value, AppError> {
        let body = serde_json::json!({
            "system": system,
            "user": user,
            "temperature": temperature,
        });
        let resp = self
            .client
            .post(format!("{}/chat", self.base_url))
            .header("X-Internal-Service-Key", &self.internal_service_api_key)
            .timeout(Duration::from_secs(120))
            .json(&body)
            .send()
            .await?;
        check(resp).await
    }

    pub async fn analyze_denial(&self, request: &Value) -> Result<Value, AppError> {
        self.resilience
            .send(|| {
                self.client
                    .post(format!("{}/analyze-denial", self.base_url))
                    .header("X-Internal-Service-Key", &self.internal_service_api_key)
                    .json(request)
            })
            .await
    }
}

impl Default for LLMServiceClient {
    fn default() -> Self {
        Self::new(None)
    }
}
