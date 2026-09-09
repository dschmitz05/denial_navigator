//! EDI Parser Service — X12 835/837 parser. Ported from `ediparser/main.py`.
//!
//! Monitors the dropzone, parses X12 835 (remittance) and 837 (claim
//! submission) files, and stores results via the API gateway.

mod common;
mod schema;
mod watch;
mod x835;
mod x837;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Multipart, Path as AxumPath, State},
    http::HeaderValue,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

use denial_common::config::{env_or, env_u64};
use denial_common::error::AppError;
use denial_common::ratelimit::SlidingWindowLimiter;

use crate::schema::Parsed835Response;
use crate::watch::Watcher;
use crate::x835::parse;

const MAX_FILE_SIZE: usize = 25 * 1024 * 1024;
const ALLOWED_EXTENSIONS: &[&str] = &[".835", ".837", ".edi"];
const DROPZONE_EXTS: &[&str] = &["835", "837", "edi", "txt"];

// ── Configuration ──

#[derive(Clone)]
struct Config {
    dropzone_path: String,
    output_path: String,
    api_base: String,
    service_api_key: String,
    cors_origins: Vec<String>,
    parsed_retention_days: u64,
}

impl Config {
    fn from_env() -> Self {
        Self {
            dropzone_path: env_or("DROPZONE_PATH", "/app/dropzone"),
            output_path: env_or("OUTPUT_PATH", "/app/output"),
            api_base: env_or("API_BASE", "http://api:8000"),
            service_api_key: std::env::var("SERVICE_API_KEY").unwrap_or_default(),
            cors_origins: env_or("CORS_ORIGINS", "")
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            parsed_retention_days: env_u64("PARSED_RETENTION_DAYS", 30),
        }
    }
}

#[derive(Clone)]
struct AppState {
    cfg: Config,
    http: reqwest::Client,
    limiter: Arc<SlidingWindowLimiter>,
}

// ── Helpers ──

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

fn dropzone_files(dropzone: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = dropzone.read_dir() {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if DROPZONE_EXTS.contains(&ext.as_str()) {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn parsed_files_count(output_path: &str) -> usize {
    let dir = Path::new(output_path);
    if !dir.is_dir() {
        return 0;
    }
    dir.read_dir()
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .ends_with(".json")
                })
                .count()
        })
        .unwrap_or(0)
}

// ── Storage ──

/// Store parsed results via the API gateway. Returns (http_status, body) or
/// None on transport failure.
async fn store_parsed_data(
    state: &AppState,
    parsed: &Parsed835Response,
    file_hash: &str,
    file_name: &str,
    file_size: usize,
) -> Option<(u16, Value)> {
    let claims: Vec<Value> = parsed
        .claims
        .iter()
        .map(|c| serde_json::to_value(c).unwrap_or(Value::Null))
        .collect();
    let denials: Vec<Value> = parsed
        .denials
        .iter()
        .map(|d| serde_json::to_value(d).unwrap_or(Value::Null))
        .collect();

    let payload = json!({
        "file_name": file_name,
        "file_hash": file_hash,
        "file_size": file_size,
        "transaction_type": parsed.metadata.transaction_set_identifier,
        "claims": claims,
        "denials": denials,
    });

    let result = state
        .http
        .post(format!("{}/api/v1/ingestion/store", state.cfg.api_base))
        .timeout(Duration::from_secs(30))
        .header("X-Service-Key", &state.cfg.service_api_key)
        .header("X-Service-Name", "ediparser")
        .json(&payload)
        .send()
        .await;

    match result {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let body = resp.json::<Value>().await.unwrap_or(Value::Null);
            if status != 200 {
                tracing::error!("Failed to store parsed data: HTTP {status} {body:?}");
            }
            Some((status, body))
        }
        Err(e) => {
            tracing::error!("Store error for {file_name}: {e}");
            None
        }
    }
}

// ── File processing ──

/// Process a single 835 or 837 file. Returns a JSON object matching the
/// Python `process_file` return dict.
async fn process_file(state: &AppState, file_path: &Path) -> Value {
    let file_name = file_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    tracing::info!("Processing file: {file_name}");

    let content = match std::fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Cannot read {file_name}: {e}");
            return json!({
                "file_name": file_name,
                "status": "error",
                "error": e.to_string(),
            });
        }
    };
    let content_bytes = content.as_bytes();
    let file_hash = sha256_hex(content_bytes);

    let parsed = match parse(&content) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Parse error in {file_name}: {e}");
            return json!({
                "file_name": file_name,
                "status": "error",
                "error": e,
            });
        }
    };

    let store_result = store_parsed_data(state, &parsed, &file_hash, &file_name, content_bytes.len())
        .await;

    let (http_status, body) = match store_result {
        Some(s) => s,
        None => (0u16, json!({ "error": "transport failure" })),
    };

    // Save output for inspection.
    let output_file =
        PathBuf::from(&state.cfg.output_path).join(format!("{file_name}.json"));
    let json_str = serde_json::to_string_pretty(&parsed).unwrap_or_default();
    if let Err(e) = std::fs::write(&output_file, json_str) {
        tracing::error!("Failed to write output for {file_name}: {e}");
    }

    let store_status = body.get("status").and_then(|v| v.as_str());
    let stored_ok = http_status == 200 && store_status == Some("stored");
    let status = if stored_ok { "completed" } else { "store_failed" };

    let error = body
        .get("error")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("detail").and_then(|v| v.as_str()))
        .map(String::from);

    tracing::info!(
        "Processed {file_name}: {} claims, {} denials, store={status}",
        parsed.claims.len(),
        parsed.denials.len()
    );

    json!({
        "file_name": file_name,
        "file_hash": file_hash,
        "claims_count": parsed.claims.len(),
        "denials_count": parsed.denials.len(),
        "status": status,
        "store_result": store_status,
        "error": error,
        "processed_at": chrono::Utc::now().to_rfc3339(),
    })
}

// ── Handlers ──

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    version: String,
    dropzone_path: String,
    parsed_files_count: usize,
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy".into(),
        version: "1.0.0".into(),
        dropzone_path: state.cfg.dropzone_path.clone(),
        parsed_files_count: parsed_files_count(&state.cfg.output_path),
    })
}

#[derive(Serialize)]
struct IngestResponse {
    file_name: String,
    transaction_type: Option<String>,
    file_size: usize,
    file_hash: String,
    claims_parsed: usize,
    denials_parsed: usize,
    status: String,
    message: String,
    claims: Vec<Value>,
    denials: Vec<Value>,
}

async fn ingest_file(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<IngestResponse>, AppError> {
    // Only the gateway calls this service, so every request shares one
    // address. The gateway already limits per user upstream; this is a
    // backstop against a runaway loop, not a per-user control.
    if !state.limiter.allow("gateway") {
        return Err(AppError::RateLimited {
            retry_after: state.limiter.retry_after("gateway"),
        });
    }

    let field = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
        .ok_or_else(|| AppError::BadRequest("No file provided".into()))?;

    let file_name = field
        .file_name()
        .map(|s| s.to_string())
        .ok_or_else(|| AppError::BadRequest("No file name provided".into()))?;

    let ext = if file_name.contains('.') {
        let last = file_name.rsplit('.').next().unwrap_or("");
        format!(".{last}").to_lowercase()
    } else {
        String::new()
    };
    if !ALLOWED_EXTENSIONS.contains(&ext.as_str()) {
        let allowed: Vec<&str> = ALLOWED_EXTENSIONS.to_vec();
        return Err(AppError::BadRequest(format!(
            "Invalid file type. Allowed: {}",
            allowed.join(", ")
        )));
    }

    let bytes = field
        .bytes()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    if bytes.len() > MAX_FILE_SIZE {
        return Err(AppError::BadRequest(format!(
            "File too large: max {MAX_FILE_SIZE} bytes"
        )));
    }

    if !bytes.starts_with(b"ISA") {
        return Err(AppError::BadRequest(
            "File does not appear to be a valid X12 file (missing ISA segment)".into(),
        ));
    }

    let content = String::from_utf8_lossy(&bytes).to_string();
    let file_hash = sha256_hex(&bytes);

    let parsed = match parse(&content) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Parse rejected for {file_name}: {e}");
            return Err(AppError::BadRequest(format!("Parse failed: {e}")));
        }
    };

    // Save output for inspection.
    let stamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let stamped_name = format!("ingest_{stamp}_{file_name}");
    let output_file =
        PathBuf::from(&state.cfg.output_path).join(format!("{stamped_name}.json"));
    let json_str = serde_json::to_string_pretty(&parsed).unwrap_or_default();
    std::fs::write(&output_file, json_str)
        .map_err(|e| AppError::Internal(format!("Failed to write output: {e}")))?;

    tracing::info!(
        "Processed {file_name}: {} claims, {} denials",
        parsed.claims.len(),
        parsed.denials.len()
    );

    let claims: Vec<Value> = parsed
        .claims
        .iter()
        .map(|c| serde_json::to_value(c).unwrap_or(Value::Null))
        .collect();
    let denials: Vec<Value> = parsed
        .denials
        .iter()
        .map(|d| serde_json::to_value(d).unwrap_or(Value::Null))
        .collect();

    let message = format!(
        "Parsed {} claims with {} denials{}",
        parsed.claims.len(),
        parsed.denials.len(),
        if parsed.warnings.is_empty() {
            String::new()
        } else {
            format!(" ({} warnings)", parsed.warnings.len())
        }
    );

    Ok(Json(IngestResponse {
        file_name,
        transaction_type: parsed.metadata.transaction_set_identifier.clone(),
        file_size: bytes.len(),
        file_hash,
        claims_parsed: parsed.claims.len(),
        denials_parsed: parsed.denials.len(),
        status: "completed".into(),
        message,
        claims,
        denials,
    }))
}

async fn ingest_dropzone(
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    let dropzone = Path::new(&state.cfg.dropzone_path);
    let files = dropzone_files(dropzone);
    let mut results = Vec::with_capacity(files.len());
    for f in &files {
        let result = process_file(&state, f).await;
        results.push(result);
    }
    Ok(Json(json!({
        "processed": results.len(),
        "results": results,
    })))
}

#[derive(Serialize)]
struct FileInfo {
    file_name: String,
    file_size: u64,
    file_hash: String,
    status: String,
    claims_count: usize,
    denials_count: usize,
    processed_at: Option<String>,
}

async fn list_files(State(state): State<AppState>) -> Json<Vec<FileInfo>> {
    let dropzone = Path::new(&state.cfg.dropzone_path);
    let output = Path::new(&state.cfg.output_path);
    let mut files = Vec::new();
    for f in dropzone_files(dropzone) {
        let fname = f
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
        let output_exists = output.join(format!("{fname}.json")).exists();
        files.push(FileInfo {
            file_name: fname,
            file_size: size,
            file_hash: String::new(),
            status: if output_exists { "processed" } else { "pending" }.into(),
            claims_count: 0,
            denials_count: 0,
            processed_at: None,
        });
    }
    Json(files)
}

async fn get_output(
    State(state): State<AppState>,
    AxumPath(file_name): AxumPath<String>,
) -> Result<Json<Value>, AppError> {
    if file_name.contains("..") || file_name.contains('/') {
        return Err(AppError::BadRequest("Invalid file name".into()));
    }
    let output_file =
        PathBuf::from(&state.cfg.output_path).join(format!("{file_name}.json"));
    let content = std::fs::read_to_string(&output_file)
        .map_err(|_| AppError::NotFound)?;
    let value: Value = serde_json::from_str(&content)
        .map_err(|_| AppError::NotFound)?;
    Ok(Json(value))
}

// ── Retention ──

/// Remove dropzone and output files past the retention window.
fn prune_old_files(cfg: &Config) -> u64 {
    if cfg.parsed_retention_days == 0 {
        return 0;
    }
    let cutoff = std::time::SystemTime::now()
        - Duration::from_secs(cfg.parsed_retention_days * 86400);
    let mut removed = 0;
    for dir in [&cfg.dropzone_path, &cfg.output_path] {
        let path = Path::new(dir);
        if !path.is_dir() {
            continue;
        }
        if let Ok(entries) = path.read_dir() {
            for entry in entries.flatten() {
                if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                let mtime = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::now());
                if mtime < cutoff && entry.path().exists() {
                    match std::fs::remove_file(entry.path()) {
                        Ok(()) => removed += 1,
                        Err(e) => {
                            tracing::warn!(
                                "Could not prune {}: {e}",
                                entry.path().display()
                            )
                        }
                    }
                }
            }
        }
    }
    if removed > 0 {
        tracing::info!(
            "Pruned {removed} file(s) older than {} days",
            cfg.parsed_retention_days
        );
    }
    removed
}

// ── Background loops ──

async fn watch_loop(mut watcher: Watcher, state: AppState) {
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    loop {
        interval.tick().await;
        let ready = watcher.ready_files();
        for path in ready {
            let fname = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            tracing::info!("New file detected: {fname}");
            let result = process_file(&state, &path).await;
            if let Some(err) = result.get("error").and_then(|v| v.as_str()) {
                tracing::warn!("Processing {fname}: {err}");
            }
            watcher.mark_processed(&fname);
        }
    }
}

async fn retention_loop(cfg: Config) {
    let mut interval = tokio::time::interval(Duration::from_secs(86400));
    loop {
        interval.tick().await;
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            prune_old_files(&cfg)
        })) {
            Ok(_) => {}
            Err(_) => tracing::error!("Retention sweep panicked"),
        }
    }
}

// ── Router / main ──

fn build_router(state: AppState) -> Router {
    let mut app = Router::new()
        .route("/health", get(health))
        .route("/ingest", post(ingest_file))
        .route("/ingest-dropzone", post(ingest_dropzone))
        .route("/files", get(list_files))
        .route("/output/{file_name}", get(get_output));

    // This service is reached by the gateway over the Docker network, never
    // by a browser, so it needs no CORS by default. Add it only if configured.
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
        tracing::error!(
            "SERVICE_API_KEY is empty; parsed data will NOT be \
             persisted. Set SERVICE_API_KEY to the shared service credential."
        );
    }

    // Create directories and prune before seeding the watcher, so the
    // watcher does not treat a just-pruned output as proof that its
    // dropzone file still needs parsing.
    std::fs::create_dir_all(&cfg.dropzone_path)
        .expect("failed to create dropzone directory");
    std::fs::create_dir_all(&cfg.output_path)
        .expect("failed to create output directory");
    prune_old_files(&cfg);

    let state = AppState {
        cfg: cfg.clone(),
        http: reqwest::Client::new(),
        limiter: Arc::new(SlidingWindowLimiter::new(60, Duration::from_secs(60), "ediparser")),
    };

    let mut watcher = Watcher::new(cfg.dropzone_path.clone());
    watcher.seed_from_outputs(&cfg.output_path);

    let watch_state = state.clone();
    tokio::spawn(watch_loop(watcher, watch_state));

    let retention_cfg = cfg.clone();
    tokio::spawn(retention_loop(retention_cfg));

    tracing::info!(
        "File watcher started on {}; retention {} days",
        cfg.dropzone_path,
        cfg.parsed_retention_days
    );

    let port = env_or("PORT", "8000");
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind ediparser port");
    tracing::info!("EDI parser listening on {addr}");
    axum::serve(listener, build_router(state)).await.expect("ediparser error");
}
