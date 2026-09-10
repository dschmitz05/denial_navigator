//! Embedding generation via the llama.cpp OpenAI-compatible API.
//! Ported from `rag-engine/main.py::generate_embeddings`.

use serde_json::Value;

use denial_common::error::AppError;

/// Provider boundary for producing dense vectors from text.
///
/// Implementations can target a local model server, a hosted API, or an
/// offline test double without coupling retrieval code to HTTP details.
#[allow(async_fn_in_trait)]
pub trait EmbeddingProvider: Send + Sync {
    fn provider_name(&self) -> &'static str;
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>, AppError>;
}

/// OpenAI-compatible `/v1/embeddings` provider used by llama.cpp and Ollama.
#[derive(Clone)]
pub struct OpenAiCompatibleEmbeddingProvider {
    client: reqwest::Client,
    base_url: String,
    model: String,
    batch: usize,
}

impl OpenAiCompatibleEmbeddingProvider {
    pub fn new(
        client: reqwest::Client,
        base_url: impl Into<String>,
        model: impl Into<String>,
        batch: usize,
    ) -> Self {
        Self {
            client,
            base_url: base_url.into(),
            model: model.into(),
            batch: batch.max(1),
        }
    }
}

impl EmbeddingProvider for OpenAiCompatibleEmbeddingProvider {
    fn provider_name(&self) -> &'static str {
        "openai_compatible"
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>, AppError> {
        generate_embeddings(&self.client, &self.base_url, &self.model, texts, self.batch).await
    }
}

/// Generate embeddings for `texts` in batches of `batch`, using the OpenAI
/// compatible `/v1/embeddings` endpoint. The request cap keeps a very large
/// document from building a request the server rejects outright.
pub async fn generate_embeddings(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    texts: &[String],
    batch: usize,
) -> Result<Vec<Vec<f64>>, AppError> {
    if texts.is_empty() {
        return Ok(vec![]);
    }
    let base_url = base_url.trim_end_matches('/');
    let mut all: Vec<Vec<f64>> = Vec::new();
    for chunk in texts.chunks(batch) {
        let body = serde_json::json!({ "model": model, "input": chunk });
        let resp = client
            .post(format!("{base_url}/v1/embeddings"))
            .timeout(std::time::Duration::from_secs(180))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let data: Value = resp.json().await?;
        if !status.is_success() {
            return Err(AppError::Upstream(format!("embedding server {status}")));
        }
        let items = data
            .get("data")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();
        if items.len() != chunk.len() {
            return Err(AppError::Upstream(format!(
                "embedding server returned {} vectors for {} inputs",
                items.len(),
                chunk.len()
            )));
        }
        // Order is not promised by the API, only the index field is.
        let mut sorted = items;
        sorted.sort_by_key(|d| d.get("index").and_then(|i| i.as_u64()).unwrap_or(0));
        for item in &sorted {
            let embedding = item
                .get("embedding")
                .and_then(|e| e.as_array())
                .ok_or_else(|| {
                    AppError::Upstream("embedding server returned an empty vector".into())
                })?;
            let vec: Vec<f64> = embedding
                .iter()
                .map(|v| {
                    v.as_f64()
                        .ok_or(AppError::Upstream("non-numeric embedding value".into()))
                })
                .collect::<Result<_, _>>()?;
            all.push(vec);
        }
    }
    Ok(all)
}

/// Format an embedding as a pgvector literal: `[0.1,0.2,0.3]`.
pub fn format_vector(vec: &[f64]) -> String {
    let parts: Vec<String> = vec.iter().map(|x| x.to_string()).collect();
    format!("[{parts}]", parts = parts.join(","))
}

#[cfg(test)]
mod tests {
    use super::{EmbeddingProvider, OpenAiCompatibleEmbeddingProvider};

    #[test]
    fn identifies_the_openai_compatible_provider() {
        let provider = OpenAiCompatibleEmbeddingProvider::new(
            reqwest::Client::new(),
            "http://localhost:8081",
            "test-model",
            0,
        );

        assert_eq!(provider.provider_name(), "openai_compatible");
    }
}
