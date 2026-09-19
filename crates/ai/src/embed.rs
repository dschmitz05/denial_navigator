//! Embedding generation via the llama.cpp OpenAI-compatible API.
//! Ported from `rag-engine/main.py::generate_embeddings`.

use serde_json::Value;

use denial_common::error::AppError;

/// What a text is embedded for. Asymmetric retrieval models are trained with
/// a different task prefix on queries than on stored passages, and retrieve
/// markedly worse when the prefix is missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbedKind {
    Query,
    Document,
}

/// Task prefixes prepended to every input before it is embedded.
///
/// Changing these changes the vector space: chunks stored under one pair do
/// not match queries embedded under another, so re-embed after a change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskPrefixes {
    pub query: String,
    pub document: String,
}

impl TaskPrefixes {
    /// The prefixes `model` was trained with; none for a model not known to
    /// use them, so a model switch never inherits another model's prefixes.
    pub fn for_model(model: &str) -> Self {
        if model.to_ascii_lowercase().contains("nomic-embed") {
            Self {
                query: "search_query: ".into(),
                document: "search_document: ".into(),
            }
        } else {
            Self::default()
        }
    }

    fn apply(&self, kind: EmbedKind, texts: &[String]) -> Vec<String> {
        let prefix = match kind {
            EmbedKind::Query => &self.query,
            EmbedKind::Document => &self.document,
        };
        texts.iter().map(|text| format!("{prefix}{text}")).collect()
    }
}

/// Provider boundary for producing dense vectors from text.
///
/// Implementations can target a local model server, a hosted API, or an
/// offline test double without coupling retrieval code to HTTP details.
#[allow(async_fn_in_trait)]
pub trait EmbeddingProvider: Send + Sync {
    fn provider_name(&self) -> &'static str;
    async fn embed(&self, kind: EmbedKind, texts: &[String]) -> Result<Vec<Vec<f64>>, AppError>;
}

/// OpenAI-compatible `/v1/embeddings` provider used by llama.cpp and Ollama.
#[derive(Clone)]
pub struct OpenAiCompatibleEmbeddingProvider {
    client: reqwest::Client,
    base_url: String,
    model: String,
    batch: usize,
    prefixes: TaskPrefixes,
}

impl OpenAiCompatibleEmbeddingProvider {
    pub fn new(
        client: reqwest::Client,
        base_url: impl Into<String>,
        model: impl Into<String>,
        batch: usize,
        prefixes: TaskPrefixes,
    ) -> Self {
        Self {
            client,
            base_url: base_url.into(),
            model: model.into(),
            batch: batch.max(1),
            prefixes,
        }
    }
}

impl EmbeddingProvider for OpenAiCompatibleEmbeddingProvider {
    fn provider_name(&self) -> &'static str {
        "openai_compatible"
    }

    async fn embed(&self, kind: EmbedKind, texts: &[String]) -> Result<Vec<Vec<f64>>, AppError> {
        let inputs = self.prefixes.apply(kind, texts);
        generate_embeddings(
            &self.client,
            &self.base_url,
            &self.model,
            &inputs,
            self.batch,
        )
        .await
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
    use super::{EmbedKind, EmbeddingProvider, OpenAiCompatibleEmbeddingProvider, TaskPrefixes};

    #[test]
    fn identifies_the_openai_compatible_provider() {
        let provider = OpenAiCompatibleEmbeddingProvider::new(
            reqwest::Client::new(),
            "http://localhost:8081",
            "test-model",
            0,
            TaskPrefixes::default(),
        );

        assert_eq!(provider.provider_name(), "openai_compatible");
    }

    #[test]
    fn nomic_models_get_their_trained_task_prefixes() {
        let prefixes = TaskPrefixes::for_model("nomic-embed-text");
        let texts = vec!["timely filing".to_string()];

        assert_eq!(
            prefixes.apply(EmbedKind::Query, &texts),
            ["search_query: timely filing"]
        );
        assert_eq!(
            prefixes.apply(EmbedKind::Document, &texts),
            ["search_document: timely filing"]
        );
        assert_eq!(
            TaskPrefixes::for_model("Nomic-Embed-Text-v1.5.f16"),
            prefixes
        );
    }

    #[test]
    fn unknown_models_are_sent_text_unchanged() {
        let prefixes = TaskPrefixes::for_model("bge-small-en");
        let texts = vec!["timely filing".to_string()];

        assert_eq!(prefixes, TaskPrefixes::default());
        assert_eq!(prefixes.apply(EmbedKind::Query, &texts), texts);
    }
}
