use axum::extract::State;
use axum::routing::get;
use axum::Json;
use axum::Router;
use serde_json::{json, Value};
use std::time::Duration;

use crate::state::AppState;

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

async fn probe(url: &str) -> (bool, Value) {
    let client = reqwest::Client::new();
    match client.get(url).timeout(PROBE_TIMEOUT).send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let body: Value = resp.json().await.unwrap_or(json!({}));
            (status < 500, json!({"status": status, "detail": body.get("detail").unwrap_or(&body)}))
        }
        Err(e) => (false, json!({"error": e.to_string()})),
    }
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    let db_ok = sqlx::query("SELECT 1")
        .execute(&state.pool)
        .await
        .is_ok();

    let (ediparser_ok, ediparser_detail) =
        probe(&format!("{}/health", env_or("EDIPARSER_SERVICE_URL", "http://localhost:8001"))).await;
    let (rag_ok, rag_detail) =
        probe(&format!("{}/health", env_or("RAG_ENGINE_URL", "http://localhost:8002"))).await;
    let (llm_ok, llm_detail) =
        probe(&format!("{}/health", env_or("LLM_SERVICE_URL", "http://localhost:8003"))).await;

    let llama_url = env_or("LLAMA_BASE_URL", "http://10.10.10.98:8080");
    let (llama_ok, llama_detail) = probe(&format!("{llama_url}/v1/models")).await;

    let embed_url = env_or("EMBED_BASE_URL", "http://10.10.10.98:11434");
    let (embed_ok, embed_detail) = probe(&format!("{embed_url}/api/tags")).await;

    Json(json!({
        "database": if db_ok { "ok" } else { "error" },
        "ediparser": if ediparser_ok { "ok" } else { "error" },
        "rag_engine": if rag_ok { "ok" } else { "error" },
        "llm_service": if llm_ok { "ok" } else { "error" },
        "llama": if llama_ok { "ok" } else { "error" },
        "embedding": if embed_ok { "ok" } else { "error" },
        "detail": {
            "ediparser": ediparser_detail,
            "rag_engine": rag_detail,
            "llm_service": llm_detail,
            "llama": llama_detail,
            "embedding": embed_detail,
        }
    }))
}

fn env_or(var: &str, default: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| default.to_string())
}

pub fn router() -> Router<AppState> {
    Router::new().route("/health", get(health))
}
