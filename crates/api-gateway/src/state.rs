use std::sync::Arc;
use std::time::Duration;

use denial_common::clients::{EDIParserClient, LLMServiceClient, RAGEngineClient};
use denial_common::config::GatewayConfig;
use denial_common::ratelimit::SlidingWindowLimiter;
use sqlx::PgPool;

fn env_usize(var: &str, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn env_u64(var: &str, default: u64) -> u64 {
    std::env::var(var)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: GatewayConfig,
    pub ediparser: EDIParserClient,
    pub rag: RAGEngineClient,
    pub llm: LLMServiceClient,
    pub login_user_limiter: Arc<SlidingWindowLimiter>,
    pub login_ip_limiter: Arc<SlidingWindowLimiter>,
    pub login_attempts: Arc<SlidingWindowLimiter>,
    pub ingestion_limiter: Arc<SlidingWindowLimiter>,
    pub knowledge_limiter: Arc<SlidingWindowLimiter>,
    pub analyses_limiter: Arc<SlidingWindowLimiter>,
    pub digest_horizon_days: u64,
    pub default_retention_days: u64,
    pub min_retention_days: u64,
}

impl AppState {
    pub async fn from_env() -> anyhow::Result<Self> {
        let config = GatewayConfig::from_env();
        let pool = denial_common::db::connect(&config.database).await?;

        let ediparser = EDIParserClient::new(None);
        let rag = RAGEngineClient::new(None);
        let llm = LLMServiceClient::new(None);

        let window_min = env_usize("LOGIN_WINDOW_MINUTES", 5);
        let window = Duration::from_secs(window_min as u64 * 60);

        let login_user_limit = env_usize("LOGIN_USER_LIMIT", 6);
        let login_ip_limit = env_usize("LOGIN_IP_LIMIT", 20);
        let login_attempts_limit = env_usize("LOGIN_ATTEMPTS_LIMIT", 30);
        let ingestion_rate_limit = env_usize("INGESTION_RATE_LIMIT", 30);
        let knowledge_rate_limit = env_usize("KNOWLEDGE_RATE_LIMIT", 10);
        let analyses_rate_limit = env_usize("ANALYSES_RATE_LIMIT", 20);

        let digest_horizon_days = env_u64("DIGEST_HORIZON_DAYS", 14);
        let default_retention_days = env_u64("DEFAULT_RETENTION_DAYS", 2190);
        let min_retention_days = env_u64("MINIMUM_RETENTION_DAYS", 365);

        Ok(Self {
            pool,
            config,
            ediparser,
            rag,
            llm,
            login_user_limiter: Arc::new(SlidingWindowLimiter::new(
                login_user_limit,
                window,
                "login-user",
            )),
            login_ip_limiter: Arc::new(SlidingWindowLimiter::new(
                login_ip_limit,
                window,
                "login-ip",
            )),
            login_attempts: Arc::new(SlidingWindowLimiter::new(
                login_attempts_limit,
                window,
                "login-attempts",
            )),
            ingestion_limiter: Arc::new(SlidingWindowLimiter::new(
                ingestion_rate_limit,
                Duration::from_secs(60),
                "ingestion",
            )),
            knowledge_limiter: Arc::new(SlidingWindowLimiter::new(
                knowledge_rate_limit,
                Duration::from_secs(60),
                "knowledge",
            )),
            analyses_limiter: Arc::new(SlidingWindowLimiter::new(
                analyses_rate_limit,
                Duration::from_secs(60),
                "analyses",
            )),
            digest_horizon_days,
            default_retention_days,
            min_retention_days,
        })
    }
}
