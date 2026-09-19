//! Environment configuration with fail-fast validation.
//!
//! Secrets that gate access control (`JWT_SECRET`, `SERVICE_API_KEY`,
//! `TOTP_FERNET_KEY`) are read once at startup and the process refuses to
//! boot if they are unset or use a known placeholder value. No silent
//! in-memory fallbacks.

use std::net::IpAddr;

use crate::totp;
use ipnet::IpNet;
use std::sync::OnceLock;
use std::time::Duration;

/// Read an environment variable, returning `default` when unset or empty.
pub fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Read an environment variable as `u64`, returning `default` when unset.
pub fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Read an environment variable as `usize`, returning `default` when unset.
pub fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Read an environment variable as `f64`, returning `default` when unset.
pub fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Read a boolean environment variable. Invalid values retain the supplied
/// default so an optional feature cannot be accidentally enabled by a typo.
pub fn env_bool(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(value)
            if matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "true" | "1" | "yes" | "on"
            ) =>
        {
            true
        }
        Ok(value)
            if matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "false" | "0" | "no" | "off"
            ) =>
        {
            false
        }
        _ => default,
    }
}

/// Parse one `TRUSTED_PROXY_NETWORKS` entry. Accepts a CIDR ("10.0.0.0/8")
/// or a bare address ("192.168.1.5", taken as a host route). The old code
/// parsed every entry as an `IpAddr`, which silently dropped every CIDR - so
/// the trusted list came out empty and no forwarded-IP header was ever
/// honoured.
fn parse_trusted_net(s: &str) -> Option<IpNet> {
    if s.is_empty() {
        return None;
    }
    if let Ok(net) = s.parse::<IpNet>() {
        return Some(net);
    }
    s.parse::<IpAddr>().ok().map(IpNet::from)
}

/// Placeholder values that must never be accepted as a real secret.
fn is_placeholder(value: &str) -> bool {
    matches!(
        value.to_lowercase().as_str(),
        "change-me"
            | "changeme"
            | "change_me"
            | "dev-secret"
            | "dev-secret-key"
            | "dev-service-api-key"
            | "fernet-key"
            | "fernet_key"
            | "secret"
            | "dev-service-key"
            | "fernet"
            | "placeholder"
            | "todo"
            | "xxx"
            | "dummy"
            | "insecure"
    ) || value.len() < 16
}

/// Read a required secret. Panics (failing the boot) if unset, empty, or a
/// known placeholder. Returns the trimmed value.
pub fn env_required_secret(key: &str) -> String {
    let value = std::env::var(key).unwrap_or_default();
    let value = value.trim().to_string();
    if value.is_empty() || is_placeholder(&value) {
        panic!(
            "FATAL: {key} is not set to a real value. Refusing to start with an \
             insecure or placeholder secret. Set it in the environment."
        );
    }
    value
}

/// Read an optional secret. Returns `None` when unset/empty/placeholder.
pub fn env_optional_secret(key: &str) -> Option<String> {
    let value = std::env::var(key).unwrap_or_default();
    let value = value.trim().to_string();
    if value.is_empty() || is_placeholder(&value) {
        None
    } else {
        Some(value)
    }
}

/// Shared configuration used by every service that touches the database.
#[derive(Clone, Debug)]
pub struct DbConfig {
    pub url: String,
    pub max_connections: u32,
}

impl DbConfig {
    pub fn from_env() -> Self {
        Self {
            url: env_or(
                "DATABASE_URL",
                "postgresql://denial_nav:denial_nav_pass@localhost:5432/denial_navigator",
            ),
            max_connections: env_usize("DB_MAX_CONNECTIONS", 10) as u32,
        }
    }
}

/// Configuration shared by the services that enforce auth (the API gateway).
#[derive(Clone, Debug)]
pub struct GatewayConfig {
    pub database: DbConfig,
    pub jwt_secret: String,
    pub ediparser_service_api_key: String,
    pub llm_service_api_key: String,
    pub service_name: String,
    pub totp_fernet_key: String,
    pub totp_issuer: String,
    pub jwt_expire_minutes: i64,
    pub audit_enabled: bool,
    /// When false (the default) the raw AI prompt/response text is never written
    /// to `ai_analyses`; only the structured, redacted result is kept. Enable
    /// only for debugging, and pair it with a short `ai_analyses` retention.
    pub store_raw_ai_artifacts: bool,
    pub trusted_proxies: Vec<IpNet>,
    pub public_base_url: String,
    pub rate_limit: RateLimitConfig,
    pub cors_origins: Vec<String>,
    pub oidc_userinfo_url: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RateLimitConfig {
    pub enabled: bool,
    pub max_requests: u32,
    pub window: Duration,
}

impl GatewayConfig {
    pub fn from_env() -> Self {
        let trusted = env_or(
            "TRUSTED_PROXY_NETWORKS",
            "127.0.0.1/32,172.16.0.0/12,10.0.0.0/8",
        )
        .split(',')
        .filter_map(|s| parse_trusted_net(s.trim()))
        .collect();

        let totp_fernet_key = env_required_secret("TOTP_FERNET_KEY");
        if let Err(e) = totp::validate_fernet_key(&totp_fernet_key) {
            panic!("FATAL: TOTP_FERNET_KEY is invalid: {e}");
        }
        let ediparser_service_api_key = env_required_secret("EDIPARSER_SERVICE_API_KEY");
        let llm_service_api_key = env_required_secret("LLM_SERVICE_API_KEY");
        if subtle_constant_time_eq(
            ediparser_service_api_key.as_bytes(),
            llm_service_api_key.as_bytes(),
        ) {
            panic!("FATAL: EDIPARSER_SERVICE_API_KEY and LLM_SERVICE_API_KEY must differ");
        }
        Self {
            database: DbConfig::from_env(),
            jwt_secret: env_required_secret("JWT_SECRET"),
            ediparser_service_api_key,
            llm_service_api_key,
            service_name: env_or("SERVICE_NAME", "denial-nav-api-gateway"),
            totp_fernet_key,
            totp_issuer: env_or("TOTP_ISSUER", "Denial Navigator"),
            jwt_expire_minutes: env_usize("JWT_EXPIRE_MINUTES", 30) as i64,
            audit_enabled: env_or("AUDIT_LOG_ENABLED", "true").eq("true"),
            store_raw_ai_artifacts: env_bool("AI_STORE_RAW_ARTIFACTS", false),
            trusted_proxies: trusted,
            public_base_url: env_or("PUBLIC_BASE_URL", "http://localhost:3000"),
            rate_limit: RateLimitConfig {
                enabled: env_or("RATE_LIMIT_ENABLED", "true").eq("true"),
                max_requests: env_u64("RATE_LIMIT_MAX_REQUESTS", 300) as u32,
                window: Duration::from_secs(env_u64("RATE_LIMIT_WINDOW_SECONDS", 60)),
            },
            cors_origins: {
                let list = env_or("CORS_ORIGINS", "");
                if list.is_empty() {
                    vec![env_or("PUBLIC_BASE_URL", "http://localhost:3000")]
                } else {
                    list.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                }
            },
            oidc_userinfo_url: std::env::var("OIDC_USERINFO_URL")
                .ok()
                .filter(|value| !value.trim().is_empty()),
        }
    }
}

fn subtle_constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut different = 0u8;
    for (x, y) in a.iter().zip(b) {
        different |= x ^ y;
    }
    different == 0
}

/// Configuration for the LLM service.
#[derive(Clone, Debug)]
pub struct LlmConfig {
    pub llm_api_base: String,
    pub llm_api_key: String,
    pub llm_model: String,
    pub llm_embedding_model: String,
    pub llm_api_timeout_secs: u64,
    pub llm_max_retries: u32,
    pub llm_temperature: f64,
}

impl LlmConfig {
    pub fn from_env() -> Self {
        Self {
            llm_api_base: env_or("LLM_API_BASE", "http://localhost:8080/v1"),
            llm_api_key: env_or("LLM_API_KEY", ""),
            llm_model: env_or("LLM_MODEL", "local-model"),
            llm_embedding_model: env_or("LLM_EMBEDDING_MODEL", "local-embedding"),
            llm_api_timeout_secs: env_u64("LLM_API_TIMEOUT_SECS", 120),
            llm_max_retries: env_u64("LLM_MAX_RETRIES", 2) as u32,
            llm_temperature: env_f64("LLM_TEMPERATURE", 0.3),
        }
    }
}

/// Configuration for the RAG engine.
#[derive(Clone, Debug)]
pub struct RagConfig {
    pub database: DbConfig,
    pub llm_api_base: String,
    pub llm_api_key: String,
    pub llm_embedding_model: String,
    pub embedding_batch_size: u32,
    pub embedding_max_retries: u32,
    pub embedding_retry_backoff_secs: f64,
    pub llm_api_timeout_secs: u64,
}

impl RagConfig {
    pub fn from_env() -> Self {
        Self {
            database: DbConfig::from_env(),
            llm_api_base: env_or("LLM_API_BASE", "http://localhost:8080/v1"),
            llm_api_key: env_or("LLM_API_KEY", ""),
            llm_embedding_model: env_or("LLM_EMBEDDING_MODEL", "local-embedding"),
            embedding_batch_size: env_u64("EMBEDDING_BATCH_SIZE", 10) as u32,
            embedding_max_retries: env_u64("EMBEDDING_MAX_RETRIES", 5) as u32,
            embedding_retry_backoff_secs: env_f64("EMBEDDING_RETRY_BACKOFF_SECS", 2.0),
            llm_api_timeout_secs: env_u64("LLM_API_TIMEOUT_SECS", 120),
        }
    }
}

/// Cached, process-wide gateway config (parsed once).
pub static GATEWAY_CONFIG: OnceLock<GatewayConfig> = OnceLock::new();

pub fn gateway_config() -> &'static GatewayConfig {
    GATEWAY_CONFIG.get_or_init(GatewayConfig::from_env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cidr_and_bare_ip_trusted_entries() {
        assert!(parse_trusted_net("10.0.0.0/8")
            .unwrap()
            .contains(&"10.20.30.40".parse::<IpAddr>().unwrap()));
        assert!(parse_trusted_net("127.0.0.1/32")
            .unwrap()
            .contains(&"127.0.0.1".parse::<IpAddr>().unwrap()));
        // Bare address -> host route.
        let host = parse_trusted_net("192.168.1.5").unwrap();
        assert!(host.contains(&"192.168.1.5".parse::<IpAddr>().unwrap()));
        assert!(!host.contains(&"192.168.1.6".parse::<IpAddr>().unwrap()));
        assert!(parse_trusted_net("").is_none());
        assert!(parse_trusted_net("not-an-ip").is_none());
    }

    #[test]
    fn the_default_trusted_list_is_not_empty() {
        let nets: Vec<_> = "127.0.0.1/32,172.16.0.0/12,10.0.0.0/8"
            .split(',')
            .filter_map(|s| parse_trusted_net(s.trim()))
            .collect();
        assert_eq!(nets.len(), 3, "the shipped default must all parse");
    }
}
