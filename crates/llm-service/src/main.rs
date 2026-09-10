//! LLM Reasoning Service — self-hosted llama.cpp integration for denial
//! analysis and appeal generation. Ported from `llm-service/main.py`.

mod llama;

use std::time::Duration;

use axum::{
    extract::State,
    http::HeaderValue,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

use denial_common::config::{env_or, env_required_secret, env_u64};
use denial_common::error::AppError;
use llama::{cached_model, resolve_model, LlamaClient};

/// Environment configuration for this service.
#[derive(Clone)]
struct Config {
    llama_base_url: String,
    llm_model: String,
    api_base: String,
    service_api_key: String,
    internal_service_api_key: String,
    llm_max_tokens: u32,
    llm_disable_thinking: bool,
    cors_origins: Vec<String>,
}

impl Config {
    fn from_env() -> Self {
        Self {
            llama_base_url: env_or("LLAMA_BASE_URL", "http://localhost:8080"),
            llm_model: env_or("LLM_MODEL", "qwen2.5:7b"),
            api_base: env_or("API_BASE", "http://api:8000"),
            service_api_key: env_required_secret("LLM_SERVICE_API_KEY"),
            internal_service_api_key: env_required_secret("LLM_INTERNAL_API_KEY"),
            llm_max_tokens: env_u64("LLM_MAX_TOKENS", 2048) as u32,
            llm_disable_thinking: !matches!(
                env_or("LLM_DISABLE_THINKING", "true")
                    .to_lowercase()
                    .as_str(),
                "0" | "false" | "no"
            ),
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
    llama: LlamaClient,
    http: reqwest::Client,
}

fn word_count(s: &str) -> i64 {
    s.split_whitespace().count() as i64
}

// ── Request / response models ──

fn default_temp() -> f64 {
    0.3
}

#[derive(Deserialize)]
struct ChatRequest {
    system: String,
    user: String,
    #[serde(default = "default_temp")]
    temperature: f64,
}

#[derive(Serialize)]
struct ChatResponse {
    model: String,
    response: String,
    tokens_used: i64,
}

#[derive(Deserialize)]
struct DenialAnalysisRequest {
    denial_id: String,
    claim_id: String,
    prompt: Value,
    #[serde(default)]
    allowed_evidence_ids: Vec<String>,
    #[serde(default = "default_temp")]
    temperature: f64,
}

#[derive(Serialize)]
struct DenialAnalysisResponse {
    model: String,
    raw_response: String,
    parsed_json: Option<Value>,
    tokens_used: i64,
    stored: bool,
}

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    model: String,
    model_available: bool,
}

// ── Storage ──

/// Store an LLM analysis result via the API gateway. Returns whether the store
/// call succeeded (a failure is logged, not propagated).
async fn store_analysis(
    state: &AppState,
    denial_id: &str,
    claim_id: &str,
    raw_prompt: &str,
    raw_response: &str,
    parsed_result: &Value,
    model_name: &str,
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
) -> bool {
    let payload = serde_json::json!({
        "denial_id": denial_id,
        "claim_id": claim_id,
        "model_name": model_name,
        "provider_name": "openai_compatible",
        "provider_version": "v1",
        "prompt_template_version": "denial_analysis_v1",
        "raw_prompt": raw_prompt,
        "raw_response": raw_response,
        "parsed_result": parsed_result,
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": total_tokens,
    });
    let result = state
        .http
        .post(format!("{}/api/v1/analyses/store", state.cfg.api_base))
        .timeout(Duration::from_secs(30))
        .header("X-Service-Key", &state.cfg.service_api_key)
        .header("X-Service-Name", "llm-service")
        .json(&payload)
        .send()
        .await;
    match result {
        Ok(resp) => {
            let status = resp.status();
            // Consume the body to free the connection, but never log it:
            // provider/API responses can contain PHI or generated text.
            let _body = resp.text().await.unwrap_or_default();
            if status.is_success() {
                true
            } else {
                tracing::error!("analysis store failed: HTTP {status}");
                false
            }
        }
        Err(e) => {
            tracing::error!(
                "analysis store transport failure: {}",
                denial_common::logging::safe_error(e)
            );
            false
        }
    }
}

// ── Normalisation ──

/// Normalise `steps` to a list of `{step, action}` objects. Models routinely
/// return a plain list of strings; consumers should not have to handle both.
fn normalize_steps(mut value: Value) -> Value {
    if let Some(steps) = value.get_mut("steps").and_then(|s| s.as_array_mut()) {
        let normalised: Vec<Value> = steps
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                let i = idx + 1;
                match item {
                    Value::Object(obj) => {
                        let step = obj.get("step").cloned().unwrap_or_else(|| Value::from(i));
                        let action = {
                            let a = obj.get("action").and_then(|v| v.as_str()).unwrap_or("");
                            if !a.is_empty() {
                                a.to_string()
                            } else {
                                obj.get("text")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string()
                            }
                        };
                        serde_json::json!({ "step": step, "action": action })
                    }
                    other => {
                        let raw = other
                            .as_str()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| other.to_string());
                        let mut text = raw.trim().to_string();
                        for sep in [". ", ") ", "- "] {
                            if let Some((head, tail)) = text.split_once(sep) {
                                if !head.is_empty()
                                    && head.chars().all(|c| c.is_ascii_digit())
                                    && !tail.is_empty()
                                {
                                    text = tail.trim().to_string();
                                    break;
                                }
                            }
                        }
                        serde_json::json!({ "step": i, "action": text })
                    }
                }
            })
            .collect();
        value["steps"] = Value::Array(normalised);
    }
    value
}

// ── Handlers ──

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    // Answer inside the gateway's probe budget, always. Bounded to 2 seconds;
    // a timeout is reported as "model not confirmed" rather than the service
    // being dead, which is what it actually means.
    let probe = async {
        let resolved = resolve_model(&state.llama, &state.cfg.llm_model).await;
        let available = state.llama.check_model_available(&resolved).await;
        (resolved, available)
    };
    match tokio::time::timeout(Duration::from_secs(2), probe).await {
        Ok((resolved, available)) => Json(HealthResponse {
            status: "healthy".into(),
            model: resolved,
            model_available: available,
        }),
        Err(_) => {
            tracing::warn!("llama.cpp did not answer the health probe in time");
            Json(HealthResponse {
                status: "healthy".into(),
                model: cached_model().unwrap_or_else(|| state.cfg.llm_model.clone()),
                model_available: false,
            })
        }
    }
}

async fn chat(
    State(state): State<AppState>,
    Json(req): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, AppError> {
    let response_text = match state
        .llama
        .chat(&req.system, &req.user, req.temperature)
        .await
    {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("Chat error: {e}");
            return Err(AppError::Internal(e.to_string()));
        }
    };
    let prompt_tokens = word_count(&req.system) + word_count(&req.user);
    let completion_tokens = word_count(&response_text);
    let model = resolve_model(&state.llama, &state.cfg.llm_model).await;
    Ok(Json(ChatResponse {
        model,
        response: response_text,
        tokens_used: prompt_tokens + completion_tokens,
    }))
}

async fn analyze_denial(
    State(state): State<AppState>,
    Json(req): Json<DenialAnalysisRequest>,
) -> Result<Json<DenialAnalysisResponse>, AppError> {
    let system_prompt = req
        .prompt
        .get("system")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let user_prompt = req
        .prompt
        .get("user")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let result = async {
        let model_name = resolve_model(&state.llama, &state.cfg.llm_model).await;
        let raw_response = match state
            .llama
            .chat(&system_prompt, &user_prompt, req.temperature)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Analysis error: {e}");
                return Err(AppError::Internal(e.to_string()));
            }
        };

        // Try to parse JSON from the response, stripping markdown fences.
        let mut cleaned = raw_response.trim().to_string();
        if let Some(rest) = cleaned.strip_prefix("```json") {
            cleaned = rest.to_string();
        }
        if let Some(rest) = cleaned.strip_suffix("```") {
            cleaned = rest.to_string();
        }
        cleaned = cleaned.trim().to_string();
        let parsed_json = match serde_json::from_str::<Value>(&cleaned) {
            Ok(v) => {
                let value = normalize_steps(v);
                denial_ai::output::validate_recommendation(&value).map_err(|error| {
                    AppError::BadRequest(format!("AI response failed schema validation: {error}"))
                })?;
                if value
                    .get("evidence_ids")
                    .and_then(Value::as_array)
                    .unwrap()
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|id| !req.allowed_evidence_ids.iter().any(|allowed| allowed == id))
                {
                    return Err(AppError::BadRequest(
                        "AI response cited evidence that was not retrieved".into(),
                    ));
                }
                Some(value)
            }
            Err(_) => {
                return Err(AppError::BadRequest(
                    "LLM response was not valid JSON; falling back to deterministic guidance"
                        .into(),
                ));
            }
        };

        // Counted before the store call so the real numbers reach the database.
        let prompt_tokens = word_count(&system_prompt) + word_count(&user_prompt);
        let completion_tokens = word_count(&raw_response);

        let parsed_result = parsed_json.as_ref().unwrap_or(&Value::Null);
        let stored_ok = store_analysis(
            &state,
            &req.denial_id,
            &req.claim_id,
            &format!("{system_prompt}\n\n{user_prompt}"),
            &raw_response,
            parsed_result,
            &model_name,
            prompt_tokens,
            completion_tokens,
            prompt_tokens + completion_tokens,
        )
        .await;

        Ok(Json(DenialAnalysisResponse {
            model: model_name,
            raw_response,
            parsed_json,
            tokens_used: prompt_tokens + completion_tokens,
            stored: stored_ok,
        }))
    }
    .await;

    result
}

async fn list_models(State(state): State<AppState>) -> impl IntoResponse {
    let body = match state.llama.list_models().await {
        Ok(models) => {
            let current = resolve_model(&state.llama, &state.cfg.llm_model).await;
            serde_json::json!({
                "models": models,
                "current_model": current,
                "configured": state.cfg.llm_model,
            })
        }
        Err(e) => serde_json::json!({
            "models": [],
            "error": e.to_string(),
            "current_model": state.cfg.llm_model,
        }),
    };
    Json(body)
}

// ── Router / main ──

fn build_router(state: AppState) -> Router {
    let key = state.cfg.internal_service_api_key.clone();
    let protected = Router::new()
        .route("/chat", post(chat))
        .route("/analyze-denial", post(analyze_denial))
        .route("/models", get(list_models))
        .layer(axum::middleware::from_fn_with_state(
            key,
            denial_common::internal_auth::require_internal_key,
        ));
    let mut app = Router::new().route("/health", get(health)).merge(protected);

    // This service is reached by the gateway over the Docker network, never by
    // a browser, so it needs no CORS by default. Add it only if configured.
    if !state.cfg.cors_origins.is_empty() {
        let origins: Vec<HeaderValue> = state
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
    if cfg.service_api_key.is_empty() {
        // The gateway refuses unauthenticated store calls, so an empty key
        // means /analyze-denial results are never persisted. Fail loudly.
        tracing::error!(
            "SERVICE_API_KEY is empty; /analyze-denial results will NOT be \
             persisted. Set SERVICE_API_KEY to the shared service credential."
        );
    }

    let llama = LlamaClient::new(
        &cfg.llama_base_url,
        &cfg.llm_model,
        cfg.llm_max_tokens,
        cfg.llm_disable_thinking,
    );
    let state = AppState {
        cfg,
        llama,
        http: reqwest::Client::new(),
    };

    let port = env_or("PORT", "8000");
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind LLM service port");
    tracing::info!("LLM service listening on {addr}");
    axum::serve(listener, build_router(state))
        .await
        .expect("LLM service error");
}
