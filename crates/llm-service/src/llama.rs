//! Client for the self-hosted llama.cpp OpenAI-compatible API, plus model
//! resolution. Ported from `llm-service/main.py`.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use denial_ai::chat::AiProvider;

/// The model requests will use, chosen from what the server serves, or the
/// reason no configured model fits. `auto` takes the first served model;
/// anything else must be served, exactly or as a prefix of the configured name.
pub fn select_model(configured: &str, served: &[String]) -> Result<String, String> {
    if served.is_empty() {
        return Err("LLM server lists no models".into());
    }
    if configured.eq_ignore_ascii_case("auto") {
        return Ok(served[0].clone());
    }
    if served
        .iter()
        .any(|name| name == configured || configured.starts_with(name.as_str()))
    {
        Ok(configured.to_string())
    } else {
        Err(format!(
            "LLM_MODEL '{configured}' is not served; served: {}",
            served.join(", ")
        ))
    }
}

/// Whether analyses can run: the model they will use, or why they will fail
/// (unreachable server, rejected key, or a model the server does not serve).
pub async fn model_status(provider: &impl AiProvider, configured: &str) -> Result<String, String> {
    let served = provider.list_models().await.map_err(|e| e.to_string())?;
    select_model(configured, &served)
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

#[cfg(test)]
mod tests {
    use super::select_model;

    fn served() -> Vec<String> {
        vec!["Qwen3.8".into(), "Qwen3.6".into()]
    }

    #[test]
    fn auto_uses_the_first_served_model() {
        assert_eq!(select_model("auto", &served()), Ok("Qwen3.8".into()));
    }

    #[test]
    fn a_served_model_is_used_as_configured() {
        assert_eq!(select_model("Qwen3.6", &served()), Ok("Qwen3.6".into()));
    }

    #[test]
    fn an_unserved_model_names_what_is_served() {
        assert_eq!(
            select_model("qwen2.5:7b", &served()),
            Err("LLM_MODEL 'qwen2.5:7b' is not served; served: Qwen3.8, Qwen3.6".into())
        );
    }

    #[test]
    fn an_empty_model_list_is_an_error() {
        assert!(select_model("auto", &[]).is_err());
    }
}
