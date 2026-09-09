//! HTTP clients for the downstream sibling services.
//!
//! Ported from `api-gateway/services/__init__.py`. Base URLs come from the
//! environment, matching the original defaults.

use std::time::Duration;

use reqwest::{Client, multipart};
use serde_json::Value;

use crate::error::AppError;

fn env_url(var: &str, default: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| default.to_string())
}

async fn check(resp: reqwest::Response) -> Result<Value, AppError> {
    let status = resp.status();
    let body: Value = resp.json().await.map_err(|e| AppError::Upstream(e.to_string()))?;
    if status.is_success() {
        Ok(body)
    } else {
        let msg = body
            .get("detail")
            .and_then(|d| d.as_str())
            .unwrap_or(status.as_str())
            .to_string();
        Err(AppError::Upstream(format!("upstream {status}: {msg}")))
    }
}

/// Client for the EDI Parser service.
#[derive(Clone)]
pub struct EDIParserClient {
    client: Client,
    base_url: String,
}

impl EDIParserClient {
    pub fn new(base_url: Option<&str>) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.map(|s| s.to_string()).unwrap_or_else(|| {
                env_url("EDIPARSER_SERVICE_URL", "http://localhost:8001")
            }),
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
            .timeout(Duration::from_secs(60))
            .multipart(form)
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
}

impl RAGEngineClient {
    pub fn new(base_url: Option<&str>) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url
                .map(|s| s.to_string())
                .unwrap_or_else(|| env_url("RAG_ENGINE_URL", "http://localhost:8002")),
        }
    }

    pub async fn search(
        &self,
        query: &str,
        top_k: u32,
        filters: Value,
    ) -> Result<Value, AppError> {
        let body = serde_json::json!({ "query": query, "top_k": top_k, "filters": filters });
        let resp = self
            .client
            .post(format!("{}/search", self.base_url))
            .timeout(Duration::from_secs(30))
            .json(&body)
            .send()
            .await?;
        check(resp).await
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
            .timeout(Duration::from_secs(600))
            .json(&body)
            .send()
            .await?;
        check(resp).await
    }

    pub async fn build_prompt(&self, payload: &Value) -> Result<Value, AppError> {
        let resp = self
            .client
            .post(format!("{}/prompt/denial-analysis", self.base_url))
            .timeout(Duration::from_secs(30))
            .json(payload)
            .send()
            .await?;
        check(resp).await
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
}

impl LLMServiceClient {
    pub fn new(base_url: Option<&str>) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url
                .map(|s| s.to_string())
                .unwrap_or_else(|| env_url("LLM_SERVICE_URL", "http://localhost:8003")),
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
            .timeout(Duration::from_secs(120))
            .json(&body)
            .send()
            .await?;
        check(resp).await
    }

    pub async fn analyze_denial(&self, request: &Value) -> Result<Value, AppError> {
        let resp = self
            .client
            .post(format!("{}/analyze-denial", self.base_url))
            .timeout(Duration::from_secs(180))
            .json(request)
            .send()
            .await?;
        check(resp).await
    }
}

impl Default for LLMServiceClient {
    fn default() -> Self {
        Self::new(None)
    }
}
