use axum::extract::State;
use axum::routing::get;
use axum::Extension;
use axum::Json;
use axum::Router;
use denial_auth::rbac::Principal;
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

/// The AI analyses row, from the organization's last 24 hours of analyses.
fn ai_analyses_row(summary: Option<&Value>) -> (&'static str, String) {
    let Some(summary) = summary else {
        return ("ok", "No analysis history for this organization".into());
    };
    let total = summary.get("analyses").and_then(Value::as_i64).unwrap_or(0);
    if total == 0 {
        return ("ok", "No analyses in the last 24 hours".into());
    }
    let share = summary
        .get("fallback_share")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let reasons = summary.get("reasons").cloned().unwrap_or(Value::Null);
    let count = |key: &str| reasons.get(key).and_then(Value::as_i64).unwrap_or(0);
    let recent = if summary.get("recent_all_degraded").and_then(Value::as_bool) == Some(true) {
        "The most recent analyses all fell back. "
    } else {
        ""
    };
    let detail = format!(
        "{recent}{:.0}% of {total} degraded: {} model failures, {} retrieval failures, {} without evidence",
        share * 100.0,
        count("llm_error"),
        count("retrieval_error"),
        count("no_evidence"),
    );
    if summary.get("degraded").and_then(Value::as_bool) == Some(true) {
        ("degraded", detail)
    } else {
        ("ok", detail)
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

async fn health(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Json<Value> {
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

    // Providers can pass their probes while analyses still fall back; this
    // row reports what actually happened to the organization's analyses.
    let ai_summary = match principal
        .organization_id
        .as_deref()
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
    {
        Some(org) => crate::routes::analyses::ai_fallback_summary(&state.pool, org)
            .await
            .ok(),
        None => None,
    };
    let (ai_status, ai_message) = ai_analyses_row(ai_summary.as_ref());

    let essential_ok = db_ok && ediparser_ok && rag_ok && llm_ok;
    let optional_ok = llama_status == "ok" && embed_status != "down" && ai_status != "degraded";
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
              "detail": embed_message },
            { "name": "AI analyses (24 h)", "status": ai_status, "essential": false,
              "detail": ai_message }
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
    use super::{ai_analyses_row, embedding_provider_row, llm_provider_row};
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
    fn ai_row_is_degraded_only_when_the_summary_says_so() {
        let quiet = json!({"analyses": 0, "degraded": false});
        assert_eq!(ai_analyses_row(Some(&quiet)).0, "ok");
        let bad = json!({"analyses": 5, "fallback_share": 0.6, "degraded": true,
            "reasons": {"llm_error": 3, "retrieval_error": 0, "no_evidence": 0}});
        assert_eq!(
            ai_analyses_row(Some(&bad)),
            (
                "degraded",
                "60% of 5 degraded: 3 model failures, 0 retrieval failures, 0 without evidence"
                    .into()
            )
        );
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
