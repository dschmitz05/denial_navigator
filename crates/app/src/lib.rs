use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use denial_common::clients::{EDIParserClient, LLMServiceClient, RAGEngineClient};
use denial_common::config::GatewayConfig;
use denial_common::ratelimit::SlidingWindowLimiter;
use denial_storage::{LocalObjectStorage, ObjectStorage, S3ObjectStorage};
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
    pub object_storage: Arc<dyn ObjectStorage>,
    pub login_user_limiter: Arc<SlidingWindowLimiter>,
    pub login_ip_limiter: Arc<SlidingWindowLimiter>,
    pub login_attempts: Arc<SlidingWindowLimiter>,
    pub ingestion_limiter: Arc<SlidingWindowLimiter>,
    pub knowledge_limiter: Arc<SlidingWindowLimiter>,
    pub analyses_limiter: Arc<SlidingWindowLimiter>,
    pub digest_horizon_days: u64,
    pub default_retention_days: u64,
    pub min_retention_days: u64,
    pub metrics: Arc<GatewayMetrics>,
}

/// Low-cardinality, Prometheus-compatible gateway telemetry. Paths are not
/// labels, preventing claim IDs or query content from entering metrics.
pub struct GatewayMetrics {
    requests: AtomicU64,
    errors: AtomicU64,
    duration_micros: AtomicU64,
}

impl GatewayMetrics {
    pub fn record(&self, status: u16, micros: u64) {
        self.requests.fetch_add(1, Ordering::Relaxed);
        if status >= 500 {
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
        self.duration_micros.fetch_add(micros, Ordering::Relaxed);
    }
    pub fn prometheus(&self) -> String {
        let requests = self.requests.load(Ordering::Relaxed);
        let errors = self.errors.load(Ordering::Relaxed);
        let seconds = self.duration_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        format!("# HELP openclaim_http_requests_total HTTP requests handled\n# TYPE openclaim_http_requests_total counter\nopenclaim_http_requests_total {requests}\n# HELP openclaim_http_server_errors_total HTTP 5xx responses\n# TYPE openclaim_http_server_errors_total counter\nopenclaim_http_server_errors_total {errors}\n# HELP openclaim_http_request_duration_seconds_total Total HTTP request duration\n# TYPE openclaim_http_request_duration_seconds_total counter\nopenclaim_http_request_duration_seconds_total {seconds}\n")
    }
}

impl AppState {
    pub async fn from_env() -> anyhow::Result<Self> {
        let config = GatewayConfig::from_env();
        let pool = denial_db::db::connect(&config.database).await?;
        denial_db::migrations::migrate(&pool).await?;

        let ediparser = EDIParserClient::new(None);
        let rag = RAGEngineClient::new(None);
        let llm = LLMServiceClient::new(None);
        let object_storage = object_storage_from_env()?;

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
            object_storage,
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
            metrics: Arc::new(GatewayMetrics {
                requests: AtomicU64::new(0),
                errors: AtomicU64::new(0),
                duration_micros: AtomicU64::new(0),
            }),
        })
    }
}

fn object_storage_from_env() -> anyhow::Result<Arc<dyn ObjectStorage>> {
    match std::env::var("OBJECT_STORAGE_BACKEND")
        .unwrap_or_else(|_| "local".to_string())
        .to_lowercase()
        .as_str()
    {
        "local" => {
            let root = std::env::var("OBJECT_STORAGE_LOCAL_PATH")
                .unwrap_or_else(|_| "./data/objects".to_string());
            Ok(Arc::new(LocalObjectStorage::new(root)?))
        }
        "s3" => {
            let required = |key: &str| {
                std::env::var(key)
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!("{key} must be set when OBJECT_STORAGE_BACKEND=s3")
                    })
            };
            Ok(Arc::new(S3ObjectStorage::new(
                &required("OBJECT_STORAGE_S3_ENDPOINT")?,
                &required("OBJECT_STORAGE_S3_BUCKET")?,
                &std::env::var("OBJECT_STORAGE_S3_REGION")
                    .unwrap_or_else(|_| "us-east-1".to_string()),
                &required("OBJECT_STORAGE_S3_ACCESS_KEY_ID")?,
                &required("OBJECT_STORAGE_S3_SECRET_ACCESS_KEY")?,
            )?))
        }
        _ => Err(anyhow::anyhow!(
            "OBJECT_STORAGE_BACKEND must be either local or s3"
        )),
    }
}
