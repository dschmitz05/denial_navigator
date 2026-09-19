//! Client for the self-hosted llama.cpp OpenAI-compatible API, plus model
//! resolution. Ported from `llm-service/main.py`.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use denial_ai::chat::AiProvider;

/// True if `model` matches a loaded model exactly or as a prefix.
pub async fn check_model_available(provider: &impl AiProvider, model: &str) -> bool {
    match provider.list_models().await {
        Ok(available) => available
            .iter()
            .any(|name| name == model || model.starts_with(name)),
        Err(error) => {
            tracing::warn!("could not list models for availability check: {error}");
            false
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
pub async fn resolve_model(provider: &impl AiProvider, configured: &str) -> String {
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
    match provider.list_models().await {
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
