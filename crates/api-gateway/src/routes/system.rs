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
            (
                status < 500,
                json!({"status": status, "detail": body.get("detail").unwrap_or(&body)}),
            )
        }
        Err(e) => (false, json!({"error": e.to_string()})),
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
    let (llama_ok, llama_detail) = probe(&format!("{llama_url}/v1/models")).await;

    let embed_url = env_or("EMBED_BASE_URL", "http://10.10.10.98:11434");
    let (embed_ok, embed_detail) = probe(&format!("{embed_url}/api/tags")).await;

    let essential_ok = db_ok && ediparser_ok && rag_ok && llm_ok;
    let optional_ok = llama_ok && embed_ok;
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
            { "name": "LLM provider", "status": status(llama_ok), "essential": false,
              "detail": detail(&llama_detail, "Reachable") },
            { "name": "Embedding provider", "status": status(embed_ok), "essential": false,
              "detail": detail(&embed_detail, "Reachable") }
        ]
    }))
}

fn env_or(var: &str, default: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| default.to_string())
}

pub fn router() -> Router<AppState> {
    Router::new().route("/health", get(health))
}
