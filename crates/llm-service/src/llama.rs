//! Client for the self-hosted llama.cpp OpenAI-compatible API, plus model
//! resolution. Ported from `llm-service/main.py`.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;

use denial_common::error::AppError;

/// Client for the llama.cpp OpenAI-compatible API.
#[derive(Clone)]
pub struct LlamaClient {
    client: reqwest::Client,
    base_url: String,
    model: String,
    max_tokens: u32,
    disable_thinking: bool,
    api_key: Option<String>,
}

impl LlamaClient {
    pub fn new(
        base_url: &str,
        model: &str,
        max_tokens: u32,
        disable_thinking: bool,
        api_key: Option<&str>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            max_tokens,
            disable_thinking,
            api_key: api_key.map(str::to_owned),
        }
    }

    /// Send a chat completion request to llama.cpp; return the assistant content.
    ///
    /// A reasoning model can put everything in `reasoning_content` and leave
    /// `content` empty; fall back rather than return nothing.
    pub async fn chat(
        &self,
        system: &str,
        user: &str,
        temperature: f64,
    ) -> Result<String, AppError> {
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user },
            ],
            "stream": false,
            "temperature": temperature,
            "max_tokens": self.max_tokens,
        });
        if self.disable_thinking {
            body["chat_template_kwargs"] = serde_json::json!({ "enable_thinking": false });
        }
        let mut request = self
            .client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .timeout(Duration::from_secs(120))
            .json(&body);
        if let Some(api_key) = &self.api_key {
            request = request.bearer_auth(api_key);
        }
        let resp = request.send().await?;
        let status = resp.status();
        let data: Value = resp.json().await?;
        if !status.is_success() {
            return Err(AppError::Upstream(format!("llama.cpp {status}")));
        }
        let message = data
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .ok_or_else(|| AppError::Upstream("llama.cpp: no message in response".into()))?;
        let mut content = message
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        if content.trim().is_empty() {
            content = message
                .get("reasoning_content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
        }
        Ok(content)
    }

    /// List available models via the OpenAI-compatible API.
    pub async fn list_models(&self) -> Result<Vec<String>, AppError> {
        let mut request = self
            .client
            .get(format!("{}/v1/models", self.base_url))
            .timeout(Duration::from_secs(10));
        if let Some(api_key) = &self.api_key {
            request = request.bearer_auth(api_key);
        }
        let resp = request.send().await?;
        let status = resp.status();
        let data: Value = resp.json().await?;
        if !status.is_success() {
            return Err(AppError::Upstream(format!("llama.cpp {status}")));
        }
        let models = data
            .get("data")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Ok(models)
    }

    /// True if `model` matches a loaded model exactly or as a prefix.
    pub async fn check_model_available(&self, model: &str) -> bool {
        match self.list_models().await {
            Ok(available) => available.iter().any(|m| m == model || model.starts_with(m)),
            Err(e) => {
                tracing::warn!("could not list models for availability check: {e}");
                false
            }
        }
    }
}

#[derive(Default)]
struct ModelCache {
    name: Option<String>,
    at: Option<Instant>,
}

static MODEL_CACHE: OnceLock<Mutex<ModelCache>> = OnceLock::new();
const MODEL_TTL: Duration = Duration::from_secs(60);

fn cache() -> &'static Mutex<ModelCache> {
    MODEL_CACHE.get_or_init(|| Mutex::new(ModelCache::default()))
}

/// Which model is actually loaded.
///
/// The host runs one llama-server at a time and the model behind the endpoint
/// is swapped by systemd, so a name pinned in the environment goes stale the
/// moment someone switches. llama.cpp ignores the `model` field in a request
/// and uses whatever it has loaded, so a stale name never breaks a call — it
/// just records the wrong thing in `ai_analyses.model_name`, the exact field
/// an audit trail and the feedback loop rely on being true.
///
/// Set `LLM_MODEL` to a specific name to pin it; leave it as "auto" to follow.
pub async fn resolve_model(client: &LlamaClient, configured: &str) -> String {
    if !configured.eq_ignore_ascii_case("auto") {
        return configured.to_string();
    }
    {
        let c = cache().lock().unwrap();
        if let (Some(name), Some(at)) = (&c.name, c.at) {
            if at.elapsed() < MODEL_TTL {
                return name.clone();
            }
        }
    }
    match client.list_models().await {
        Ok(names) if !names.is_empty() => {
            let name = names[0].clone();
            {
                let mut c = cache().lock().unwrap();
                c.name = Some(name.clone());
                c.at = Some(Instant::now());
            }
            name
        }
        Ok(_) => cached_model().unwrap_or_else(|| "unknown".to_string()),
        Err(e) => {
            tracing::warn!("could not read the loaded model, falling back: {e}");
            cached_model().unwrap_or_else(|| "unknown".to_string())
        }
    }
}

/// The cached model name, if any.
pub fn cached_model() -> Option<String> {
    cache().lock().unwrap().name.clone()
}
