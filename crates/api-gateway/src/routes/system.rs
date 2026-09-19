use axum::extract::State;
use axum::routing::get;
use axum::Json;
use axum::Router;
use serde_json::{json, Value};
use std::time::Duration;

use crate::state::AppState;

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Only a 2xx is healthy; the body comes back either way so callers can read
/// the service's own report of its dependencies.
async fn probe(url: &str) -> (bool, Value) {
    let client = reqwest::Client::new();
    match client.get(url).timeout(PROBE_TIMEOUT).send().await {
        Ok(resp) => {
            let status = resp.status();
            let body: Value = resp.json().await.unwrap_or(json!({}));
            if status.is_success() {
                (true, body)
            } else {
                let reason = body.get("detail").and_then(Value::as_str).unwrap_or("");
                (
                    false,
                    json!({"error": format!("HTTP {status} {reason}").trim_end()}),
                )
            }
        }
        Err(e) => (false, json!({"error": e.to_string()})),
    }
}

/// The LLM row, from llm-service's own check of the model it will request
/// (it holds the API key and resolves `LLM_MODEL`). Reachability alone once
/// reported "ok" while every analysis failed on an unserved model name.
fn llm_provider_row(service_ok: bool, body: &Value) -> (&'static str, String) {
    if !service_ok {
        return (
            "down",
            "Recommendation service did not report on the model".into(),
        );
    }
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if body.get("model_available").and_then(Value::as_bool) == Some(true) {
        ("ok", format!("Serving {model}"))
    } else {
        let reason = body
            .get("model_error")
            .and_then(Value::as_str)
            .unwrap_or("model not available");
        ("down", reason.to_string())
    }
}

/// The embedding row, from the RAG engine's probe embedding.
fn embedding_provider_row(service_ok: bool, body: &Value) -> (&'static str, String) {
    if !service_ok {
        return (
            "down",
            "Retrieval service did not report on embeddings".into(),
        );
    }
    let embedding = body.get("embedding").unwrap_or(&Value::Null);
    let detail = embedding
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or("no embedding check reported")
        .to_string();
    match embedding.get("status").and_then(Value::as_str) {
        Some("ok") => ("ok", detail),
        Some("disabled") => ("disabled", detail),
        _ => ("down", detail),
    }
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    let db_ok = sqlx::query("SELECT 1").execute(&state.pool).await.is_ok();

    let (ediparser_ok, ediparser_detail) = probe(&format!(
        "{}/health",
        env_or("EDIPARSER_SERVICE_URL", "http://localhost:8001")
    ))
    .await;
    let (rag_ok, rag_detail) = probe(&format!(
        "{}/health",
        env_or("RAG_ENGINE_URL", "http://localhost:8002")
    ))
    .await;
    let (llm_ok, llm_detail) = probe(&format!(
        "{}/health",
        env_or("LLM_SERVICE_URL", "http://localhost:8003")
    ))
    .await;

    let llama_url = env_or("LLAMA_BASE_URL", "http://10.10.10.98:8080");
    let (llama_status, llama_message) = llm_provider_row(llm_ok, &llm_detail);
    let (embed_status, embed_message) = embedding_provider_row(rag_ok, &rag_detail);

    let essential_ok = db_ok && ediparser_ok && rag_ok && llm_ok;
    let optional_ok = llama_status == "ok" && embed_status != "down";
    let status = |ok: bool| if ok { "ok" } else { "down" };
    let detail = |value: &Value, fallback: &str| {
        value
            .get("error")
            .and_then(Value::as_str)
            .or_else(|| value.get("detail").and_then(Value::as_str))
            .unwrap_or(fallback)
            .to_string()
    };
    Json(json!({
        "overall": if !essential_ok { "down" } else if !optional_ok { "degraded" } else { "ok" },
        "checked_at": chrono::Utc::now().to_rfc3339(),
        "urls": { "llama": llama_url },
        "services": [
            { "name": "Database", "status": status(db_ok), "essential": true,
              "detail": if db_ok { "Connected" } else { "Database check failed" } },
            { "name": "EDI parser", "status": status(ediparser_ok), "essential": true,
              "detail": detail(&ediparser_detail, "Reachable"),
              "sources": ediparser_detail.get("sources").cloned().unwrap_or_else(|| json!([])) },
            { "name": "Rules and retrieval", "status": status(rag_ok), "essential": true,
              "detail": detail(&rag_detail, "Reachable") },
            { "name": "Recommendation service", "status": status(llm_ok), "essential": true,
              "detail": detail(&llm_detail, "Reachable") },
            { "name": "LLM provider", "status": llama_status, "essential": false,
              "detail": llama_message },
            { "name": "Embedding provider", "status": embed_status, "essential": false,
              "detail": embed_message }
        ]
    }))
}

fn env_or(var: &str, default: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| default.to_string())
}

pub fn router() -> Router<AppState> {
    Router::new().route("/health", get(health))
}

#[cfg(test)]
mod tests {
    use super::{embedding_provider_row, llm_provider_row};
    use serde_json::json;

    #[test]
    fn llm_row_is_ok_only_when_the_model_is_served() {
        let served = json!({"model": "Qwen3.8", "model_available": true});
        assert_eq!(
            llm_provider_row(true, &served),
            ("ok", "Serving Qwen3.8".into())
        );

        let unserved = json!({"model": "auto", "model_available": false,
            "model_error": "LLM server 401 Unauthorized: invalid key"});
        assert_eq!(
            llm_provider_row(true, &unserved),
            ("down", "LLM server 401 Unauthorized: invalid key".into())
        );
        assert_eq!(llm_provider_row(false, &json!({})).0, "down");
    }

    #[test]
    fn embedding_row_follows_the_rag_probe() {
        let ok = json!({"embedding": {"status": "ok", "detail": "768-dimension vectors"}});
        assert_eq!(embedding_provider_row(true, &ok).0, "ok");
        let off = json!({"embedding": {"status": "disabled", "detail": "lexical only"}});
        assert_eq!(embedding_provider_row(true, &off).0, "disabled");
        assert_eq!(
            embedding_provider_row(true, &json!({"status": "healthy"})).0,
            "down"
        );
        assert_eq!(embedding_provider_row(false, &ok).0, "down");
    }
}
