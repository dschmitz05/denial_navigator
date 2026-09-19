//! OpenAI-compatible chat-completion provider boundary.

use std::time::Duration;

use denial_common::error::AppError;
use serde_json::Value;

/// Request sent to a chat-completions provider.
#[derive(Clone, Debug)]
pub struct ChatRequest {
    pub model: String,
    pub system: String,
    pub user: String,
    pub temperature: f64,
}

/// Provider boundary for generating chat completions and discovering models.
#[allow(async_fn_in_trait)]
pub trait AiProvider: Send + Sync {
    fn provider_name(&self) -> &'static str;
    async fn chat(&self, request: ChatRequest) -> Result<String, AppError>;
    async fn list_models(&self) -> Result<Vec<String>, AppError>;
}

/// OpenAI-compatible `/v1/chat/completions` provider used by llama.cpp and vLLM.
#[derive(Clone)]
pub struct OpenAiCompatibleAiProvider {
    client: reqwest::Client,
    base_url: String,
    max_tokens: u32,
    disable_thinking: bool,
    api_key: Option<String>,
}

impl OpenAiCompatibleAiProvider {
    pub fn new(
        client: reqwest::Client,
        base_url: impl Into<String>,
        max_tokens: u32,
        disable_thinking: bool,
        api_key: Option<&str>,
    ) -> Self {
        Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            max_tokens,
            disable_thinking,
            api_key: api_key.map(str::to_owned),
        }
    }

    fn request(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(api_key) => request.bearer_auth(api_key),
            None => request,
        }
    }
}

impl AiProvider for OpenAiCompatibleAiProvider {
    fn provider_name(&self) -> &'static str {
        "openai_compatible"
    }

    async fn chat(&self, request: ChatRequest) -> Result<String, AppError> {
        let mut body = serde_json::json!({
            "model": request.model,
            "messages": [
                { "role": "system", "content": request.system },
                { "role": "user", "content": request.user },
            ],
            "stream": false,
            "temperature": request.temperature,
            "max_tokens": self.max_tokens,
        });
        if self.disable_thinking {
            body["chat_template_kwargs"] = serde_json::json!({ "enable_thinking": false });
        }
        let response = self
            .request(
                self.client
                    .post(format!("{}/v1/chat/completions", self.base_url))
                    .timeout(Duration::from_secs(120))
                    .json(&body),
            )
            .send()
            .await?;
        let status = response.status();
        let data: Value = response.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(upstream_error(status, &data));
        }
        let message = data
            .get("choices")
            .and_then(|choices| choices.get(0))
            .and_then(|choice| choice.get("message"))
            .ok_or_else(|| AppError::Upstream("llama.cpp: no message in response".into()))?;
        let content = message
            .get("content")
            .and_then(Value::as_str)
            .filter(|content| !content.trim().is_empty())
            .or_else(|| message.get("reasoning_content").and_then(Value::as_str))
            .unwrap_or("")
            .to_string();
        Ok(content)
    }

    async fn list_models(&self) -> Result<Vec<String>, AppError> {
        let response = self
            .request(
                self.client
                    .get(format!("{}/v1/models", self.base_url))
                    .timeout(Duration::from_secs(10)),
            )
            .send()
            .await?;
        let status = response.status();
        let data: Value = response.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(upstream_error(status, &data));
        }
        Ok(data
            .get("data")
            .and_then(Value::as_array)
            .map(|models| {
                models
                    .iter()
                    .filter_map(|model| model.get("id").and_then(Value::as_str).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    }
}

/// Convert an OpenAI-compatible error response into the application error.
pub fn upstream_error(status: reqwest::StatusCode, data: &Value) -> AppError {
    let detail = data
        .pointer("/error/message")
        .or_else(|| data.get("error"))
        .or_else(|| data.get("detail"))
        .and_then(Value::as_str)
        .map(|detail| detail.chars().take(300).collect::<String>());
    AppError::Upstream(match detail {
        Some(detail) => format!("LLM server {status}: {detail}"),
        None => format!("LLM server {status}"),
    })
}

#[cfg(test)]
mod tests {
    use super::{upstream_error, AiProvider, OpenAiCompatibleAiProvider};
    use reqwest::StatusCode;
    use serde_json::json;

    #[test]
    fn identifies_the_openai_compatible_provider() {
        let provider = OpenAiCompatibleAiProvider::new(
            reqwest::Client::new(),
            "http://localhost:8080/",
            2048,
            true,
            None,
        );

        assert_eq!(provider.provider_name(), "openai_compatible");
    }

    #[test]
    fn upstream_error_carries_the_servers_reason() {
        let vllm = json!({"error": {"message": "The model `auto` does not exist.", "code": 404}});
        assert_eq!(
            upstream_error(StatusCode::NOT_FOUND, &vllm).to_string(),
            "LLM server 404 Not Found: The model `auto` does not exist."
        );
    }
}
