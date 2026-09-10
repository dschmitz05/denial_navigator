//! EDI Parser Service — X12 835/837 parser. Ported from `ediparser/main.py`.
//!
//! Monitors the dropzone, parses X12 835 (remittance) and 837 (claim
//! submission) files, and stores results via the API gateway.

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
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use url::Url;
use uuid::Uuid;

use denial_common::config::{env_bool, env_optional_secret, env_or, env_required_secret, env_u64};
use denial_common::error::AppError;
use denial_common::ratelimit::SlidingWindowLimiter;
use denial_edi_835::parse;
use denial_edi_core::schema::Parsed835Response;
use denial_jobs::Watcher;
use denial_storage::{ObjectKey, ObjectStorage, S3ObjectStorage};

const MAX_FILE_SIZE: usize = 25 * 1024 * 1024;
const ALLOWED_EXTENSIONS: &[&str] = &[".835", ".837", ".edi"];
const DROPZONE_EXTS: &[&str] = &["835", "837", "edi", "txt"];
const DEVELOPMENT_ORGANIZATION_ID: &str = "00000000-0000-0000-0000-000000000001";

// ── Configuration ──

#[derive(Clone)]
struct Config {
    dropzone_path: String,
    output_path: String,
    api_base: String,
    service_api_key: String,
    internal_service_api_key: String,
    cors_origins: Vec<String>,
    parsed_retention_days: u64,
    ingestion_organization_id: Uuid,
    sftp: Option<SftpConfig>,
    s3: Option<S3ImportConfig>,
}

#[derive(Clone)]
struct SftpConfig {
    host: String,
    port: u16,
    username: String,
    remote_path: String,
    private_key_path: Option<String>,
    password: Option<String>,
    host_public_key_sha256: String,
    poll_seconds: u64,
}

#[derive(Clone)]
struct S3ImportConfig {
    endpoint: String,
    bucket: String,
    region: String,
    access_key_id: String,
    secret_access_key: String,
    prefix: String,
    poll_seconds: u64,
}

impl S3ImportConfig {
    fn from_env() -> Option<Self> {
        if !env_bool("S3_IMPORT_ENABLED", false) {
            return None;
        }

        let endpoint = env_or("S3_IMPORT_ENDPOINT", "");
        let bucket = env_or("S3_IMPORT_BUCKET", "");
        let region = env_or("S3_IMPORT_REGION", "us-east-1");
        let access_key_id = env_or("S3_IMPORT_ACCESS_KEY_ID", "");
        let secret_access_key = env_optional_secret("S3_IMPORT_SECRET_ACCESS_KEY");
        assert!(
            !endpoint.is_empty() && !bucket.is_empty() && !access_key_id.is_empty(),
            "S3_IMPORT_ENDPOINT, S3_IMPORT_BUCKET, and S3_IMPORT_ACCESS_KEY_ID are required when S3_IMPORT_ENABLED=true"
        );
        let secret_access_key = secret_access_key
            .expect("S3_IMPORT_SECRET_ACCESS_KEY is required when S3_IMPORT_ENABLED=true");

        Some(Self {
            endpoint,
            bucket,
            region,
            access_key_id,
            secret_access_key,
            prefix: env_or("S3_IMPORT_PREFIX", "")
                .trim_start_matches('/')
                .to_string(),
            poll_seconds: env_u64("S3_IMPORT_POLL_SECONDS", 60).max(10),
        })
    }

    fn storage(&self) -> Result<S3ObjectStorage, AppError> {
        S3ObjectStorage::new(
            &self.endpoint,
            &self.bucket,
            &self.region,
            &self.access_key_id,
            &self.secret_access_key,
        )
        .map_err(|_| AppError::Internal("Invalid S3 import configuration".into()))
    }
}

impl SftpConfig {
    fn from_env() -> Option<Self> {
        if !env_bool("SFTP_ENABLED", false) {
            return None;
        }

        let host = env_or("SFTP_HOST", "");
        let username = env_or("SFTP_USERNAME", "");
        let remote_path = env_or("SFTP_REMOTE_PATH", "");
        let host_public_key_sha256 = env_or("SFTP_HOST_PUBLIC_KEY_SHA256", "");
        let private_key_path = env_or("SFTP_PRIVATE_KEY_PATH", "");
        let password = env_optional_secret("SFTP_PASSWORD");
        let port = env_u64("SFTP_PORT", 22);

        assert!(
            !host.is_empty(),
            "SFTP_HOST is required when SFTP_ENABLED=true"
        );
        assert!(
            !username.is_empty(),
            "SFTP_USERNAME is required when SFTP_ENABLED=true"
        );
        assert!(
            !remote_path.is_empty(),
            "SFTP_REMOTE_PATH is required when SFTP_ENABLED=true"
        );
        assert!(
            !host_public_key_sha256.is_empty(),
            "SFTP_HOST_PUBLIC_KEY_SHA256 is required when SFTP_ENABLED=true"
        );
        assert!(
            port <= u16::MAX as u64,
            "SFTP_PORT must be a valid TCP port"
        );
        assert!(
            !private_key_path.is_empty() || password.is_some(),
            "Set SFTP_PRIVATE_KEY_PATH or SFTP_PASSWORD when SFTP_ENABLED=true"
        );

        Some(Self {
            host,
            port: port as u16,
            username,
            remote_path: remote_path.trim_matches('/').to_string(),
            private_key_path: (!private_key_path.is_empty()).then_some(private_key_path),
            password,
            host_public_key_sha256,
            poll_seconds: env_u64("SFTP_POLL_SECONDS", 60).max(10),
        })
    }

    fn directory_url(&self) -> Result<Url, AppError> {
        let mut url = Url::parse("sftp://placeholder/")
            .map_err(|_| AppError::Internal("Could not construct SFTP URL".into()))?;
        url.set_host(Some(&self.host))
            .map_err(|_| AppError::BadRequest("Invalid SFTP_HOST".into()))?;
        url.set_port(Some(self.port))
            .map_err(|_| AppError::BadRequest("Invalid SFTP_PORT".into()))?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| AppError::Internal("Could not construct SFTP path".into()))?;
            segments.clear();
            for part in self.remote_path.split('/').filter(|part| !part.is_empty()) {
                segments.push(part);
            }
            segments.push("");
        }
        Ok(url)
    }

    fn file_url(&self, file_name: &str) -> Result<Url, AppError> {
        let mut url = self.directory_url()?;
        url.path_segments_mut()
            .map_err(|_| AppError::Internal("Could not construct SFTP path".into()))?
            .push(file_name);
        Ok(url)
    }
}

impl Config {
    fn from_env() -> Self {
        Self {
            dropzone_path: env_or("DROPZONE_PATH", "/app/dropzone"),
            output_path: env_or("OUTPUT_PATH", "/app/output"),
            api_base: env_or("API_BASE", "http://api:8000"),
            service_api_key: env_required_secret("EDIPARSER_SERVICE_API_KEY"),
            internal_service_api_key: env_required_secret("EDIPARSER_INTERNAL_API_KEY"),
            cors_origins: env_or("CORS_ORIGINS", "")
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            parsed_retention_days: env_u64("PARSED_RETENTION_DAYS", 30),
            ingestion_organization_id: Uuid::parse_str(&env_or(
                "INGESTION_ORGANIZATION_ID",
                DEVELOPMENT_ORGANIZATION_ID,
            ))
            .expect("INGESTION_ORGANIZATION_ID must be a UUID"),
            sftp: SftpConfig::from_env(),
            s3: S3ImportConfig::from_env(),
        }
    }
}

#[derive(Clone)]
struct AppState {
    cfg: Config,
    http: reqwest::Client,
    limiter: Arc<SlidingWindowLimiter>,
    source_health: Arc<tokio::sync::RwLock<Vec<SourceHealth>>>,
}

#[derive(Clone, Serialize)]
struct SourceHealth {
    name: String,
    enabled: bool,
    status: String,
    detail: String,
    last_checked_at: Option<String>,
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
                .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
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
    file_path: &str,
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
        "organization_id": state.cfg.ingestion_organization_id,
        "file_name": file_name,
        "file_path": file_path,
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
                // The gateway response may include a validation echo. Never
                // render it: parsed EDI payloads are PHI-bearing input.
                tracing::error!("parsed-ingestion store failed: HTTP {status}");
            }
            Some((status, body))
        }
        Err(e) => {
            tracing::error!(
                "parsed-ingestion store transport failure: {}",
                denial_common::logging::safe_error(&e)
            );
            None
        }
    }
}

// ── File processing ──

/// Process a single 835 or 837 file. Returns a JSON object matching the
/// Python `process_file` return dict.
async fn process_file(state: &AppState, file_path: &Path) -> Value {
    process_file_from_source(state, file_path, &file_path.to_string_lossy(), None).await
}

async fn process_file_from_source(
    state: &AppState,
    file_path: &Path,
    source_path: &str,
    source_file_name: Option<&str>,
) -> Value {
    let file_name = source_file_name.map(str::to_owned).unwrap_or_else(|| {
        file_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    });
    tracing::info!("processing queued EDI file");

    let content = match std::fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(
                "queued EDI file could not be read: {}",
                denial_common::logging::safe_error(&e)
            );
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
            tracing::error!("queued EDI file was rejected by parser");
            return json!({
                "file_name": file_name,
                "status": "error",
                "error": e,
            });
        }
    };

    let store_result = store_parsed_data(
        state,
        &parsed,
        &file_hash,
        &file_name,
        content_bytes.len(),
        source_path,
    )
    .await;

    let (http_status, body) = match store_result {
        Some(s) => s,
        None => (0u16, json!({ "error": "transport failure" })),
    };

    // Save output for inspection.
    let output_file = PathBuf::from(&state.cfg.output_path).join(format!("{file_name}.json"));
    let json_str = serde_json::to_string_pretty(&parsed).unwrap_or_default();
    if let Err(e) = std::fs::write(&output_file, json_str) {
        tracing::error!(
            "parsed EDI output could not be persisted: {}",
            denial_common::logging::safe_error(e)
        );
    }

    let store_status = body.get("status").and_then(|v| v.as_str());
    let stored_ok =
        http_status == 200 && matches!(store_status, Some("stored") | Some("skipped_duplicate"));
    let status = if stored_ok {
        "completed"
    } else {
        "store_failed"
    };

    let error = body
        .get("error")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("detail").and_then(|v| v.as_str()))
        .map(String::from);

    tracing::info!(
        "processed EDI file: {} claims, {} denials, store={status}",
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
    sources: Vec<SourceHealth>,
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy".into(),
        version: "1.0.0".into(),
        dropzone_path: state.cfg.dropzone_path.clone(),
        parsed_files_count: parsed_files_count(&state.cfg.output_path),
        sources: state.source_health.read().await.clone(),
    })
}

async fn set_source_health(state: &AppState, name: &str, status: &str, detail: String) {
    let mut sources = state.source_health.write().await;
    if let Some(source) = sources.iter_mut().find(|source| source.name == name) {
        source.status = status.to_string();
        source.detail = detail;
        source.last_checked_at = Some(chrono::Utc::now().to_rfc3339());
    }
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
            tracing::warn!("uploaded EDI file rejected by parser");
            return Err(AppError::BadRequest(format!("Parse failed: {e}")));
        }
    };

    // Save output for inspection.
    let stamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let stamped_name = format!("ingest_{stamp}_{file_name}");
    let output_file = PathBuf::from(&state.cfg.output_path).join(format!("{stamped_name}.json"));
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

async fn ingest_dropzone(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
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
            status: if output_exists {
                "processed"
            } else {
                "pending"
            }
            .into(),
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
    let output_file = PathBuf::from(&state.cfg.output_path).join(format!("{file_name}.json"));
    let content = std::fs::read_to_string(&output_file).map_err(|_| AppError::NotFound)?;
    let value: Value = serde_json::from_str(&content).map_err(|_| AppError::NotFound)?;
    Ok(Json(value))
}

// ── Retention ──

/// Remove dropzone and output files past the retention window.
fn prune_old_files(cfg: &Config) -> u64 {
    if cfg.parsed_retention_days == 0 {
        return 0;
    }
    let cutoff =
        std::time::SystemTime::now() - Duration::from_secs(cfg.parsed_retention_days * 86400);
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
                                "could not prune expired EDI storage item: {}",
                                denial_common::logging::safe_error(e)
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
            tracing::info!("new EDI file detected");
            let result = process_file(&state, &path).await;
            if result.get("error").is_some() {
                tracing::warn!("watched EDI file processing failed");
            }
            watcher.mark_processed(&fname);
        }
    }
}

struct SftpCredentialsFile(Option<PathBuf>);

impl SftpCredentialsFile {
    fn create(cfg: &SftpConfig) -> Result<Self, AppError> {
        let Some(password) = cfg.password.as_ref() else {
            return Ok(Self(None));
        };
        if [cfg.host.as_str(), cfg.username.as_str(), password.as_str()]
            .iter()
            .any(|value| value.chars().any(char::is_whitespace))
        {
            return Err(AppError::BadRequest(
                "SFTP credentials cannot contain whitespace when password authentication is used"
                    .into(),
            ));
        }

        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let path = std::env::temp_dir().join(format!("ediparser-sftp-{}.netrc", Uuid::new_v4()));
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| AppError::Internal(format!("Could not prepare SFTP credentials: {e}")))?;
        write!(
            file,
            "machine {} login {} password {}\n",
            cfg.host, cfg.username, password
        )
        .map_err(|e| AppError::Internal(format!("Could not prepare SFTP credentials: {e}")))?;
        Ok(Self(Some(path)))
    }

    fn path(&self) -> Option<&Path> {
        self.0.as_deref()
    }
}

impl Drop for SftpCredentialsFile {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

async fn run_sftp_curl(
    cfg: &SftpConfig,
    url: &Url,
    credentials: Option<&Path>,
    output: Option<&Path>,
    list_only: bool,
) -> Result<Vec<u8>, AppError> {
    let mut command = tokio::process::Command::new("curl");
    command.args([
        "--fail",
        "--silent",
        "--show-error",
        "--disable",
        "--proto",
        "=sftp",
        "--connect-timeout",
        "15",
        "--max-time",
        "120",
        "--hostpubsha256",
        &cfg.host_public_key_sha256,
    ]);
    if list_only {
        command.arg("--list-only");
    }
    if let Some(private_key_path) = &cfg.private_key_path {
        command.args(["--user", &cfg.username, "--key", private_key_path]);
    } else if let Some(credentials) = credentials {
        command.args(["--netrc-file", credentials.to_string_lossy().as_ref()]);
    }
    if let Some(output) = output {
        command.args(["--output", output.to_string_lossy().as_ref()]);
    }
    command.arg(url.as_str());

    let result = command
        .output()
        .await
        .map_err(|e| AppError::Internal(format!("Could not start SFTP client: {e}")))?;
    if !result.status.success() {
        return Err(AppError::Internal("SFTP transfer failed".into()));
    }
    Ok(result.stdout)
}

fn is_sftp_edi_file(file_name: &str) -> bool {
    !file_name.is_empty()
        && !file_name.starts_with('.')
        && !file_name.contains('/')
        && !file_name.contains('\\')
        && file_name
            .rsplit('.')
            .next()
            .map(|extension| DROPZONE_EXTS.contains(&extension.to_lowercase().as_str()))
            .unwrap_or(false)
}

async fn poll_sftp(state: &AppState, cfg: &SftpConfig) -> Result<usize, AppError> {
    let credentials = SftpCredentialsFile::create(cfg)?;
    let directory_url = cfg.directory_url()?;
    let listed = run_sftp_curl(cfg, &directory_url, credentials.path(), None, true).await?;
    let names: Vec<String> = String::from_utf8_lossy(&listed)
        .lines()
        .map(str::trim)
        .filter(|name| is_sftp_edi_file(name))
        .map(str::to_owned)
        .collect();

    let mut processed = 0;
    for file_name in names {
        let source_url = cfg.file_url(&file_name)?;
        let download_path = PathBuf::from(&state.cfg.output_path)
            .join(format!(".sftp-download-{}", Uuid::new_v4()));
        let download = run_sftp_curl(
            cfg,
            &source_url,
            credentials.path(),
            Some(&download_path),
            false,
        )
        .await;
        if let Err(error) = download {
            let _ = std::fs::remove_file(&download_path);
            tracing::warn!(
                "SFTP file download failed: {}",
                denial_common::logging::safe_error(error)
            );
            continue;
        }

        let result =
            process_file_from_source(state, &download_path, source_url.as_str(), Some(&file_name))
                .await;
        let _ = std::fs::remove_file(&download_path);
        if result.get("status").and_then(Value::as_str) == Some("completed") {
            processed += 1;
        } else {
            tracing::warn!("SFTP file processing failed");
        }
    }
    Ok(processed)
}

async fn sftp_loop(state: AppState, cfg: SftpConfig) {
    let mut interval = tokio::time::interval(Duration::from_secs(cfg.poll_seconds));
    loop {
        interval.tick().await;
        match poll_sftp(&state, &cfg).await {
            Ok(processed) => {
                set_source_health(
                    &state,
                    "SFTP",
                    "ok",
                    format!("Connected; {processed} file(s) processed in the last poll"),
                )
                .await;
                if processed > 0 {
                    tracing::info!("SFTP poll processed {processed} file(s)")
                }
            }
            Err(error) => {
                set_source_health(
                    &state,
                    "SFTP",
                    "degraded",
                    "The most recent poll failed; verify partner connectivity and credentials"
                        .into(),
                )
                .await;
                tracing::warn!(
                    "SFTP poll failed: {}",
                    denial_common::logging::safe_error(error)
                );
            }
        }
    }
}

fn is_edi_object(key: &ObjectKey) -> bool {
    key.as_str()
        .rsplit('/')
        .next()
        .is_some_and(is_sftp_edi_file)
}

async fn poll_s3(state: &AppState, cfg: &S3ImportConfig) -> Result<usize, AppError> {
    let list_config = cfg.clone();
    let keys = tokio::task::spawn_blocking(move || {
        let storage = list_config.storage()?;
        storage
            .list_prefix(&list_config.prefix)
            .map_err(|_| AppError::Internal("S3 object listing failed".into()))
    })
    .await
    .map_err(|_| AppError::Internal("S3 import task failed".into()))??;

    let mut processed = 0;
    for key in keys.into_iter().filter(is_edi_object) {
        let get_config = cfg.clone();
        let get_key = key.clone();
        let bytes = tokio::task::spawn_blocking(move || {
            let storage = get_config.storage()?;
            storage
                .get(&get_key)
                .map_err(|_| AppError::Internal("S3 object download failed".into()))
        })
        .await
        .map_err(|_| AppError::Internal("S3 import task failed".into()))?;
        let bytes = match bytes {
            Ok(bytes) if bytes.len() <= MAX_FILE_SIZE => bytes,
            Ok(_) => {
                tracing::warn!("S3 object skipped because it exceeds the EDI size limit");
                continue;
            }
            Err(error) => {
                tracing::warn!(
                    "S3 object download failed: {}",
                    denial_common::logging::safe_error(error)
                );
                continue;
            }
        };

        let file_name = key.as_str().rsplit('/').next().unwrap_or_default();
        let download_path =
            PathBuf::from(&state.cfg.output_path).join(format!(".s3-download-{}", Uuid::new_v4()));
        if std::fs::write(&download_path, bytes).is_err() {
            tracing::warn!("S3 object could not be staged for parsing");
            continue;
        }
        let source_path = format!("s3://{}/{}", cfg.bucket, key.as_str());
        let result =
            process_file_from_source(state, &download_path, &source_path, Some(file_name)).await;
        let _ = std::fs::remove_file(&download_path);
        if result.get("status").and_then(Value::as_str) == Some("completed") {
            processed += 1;
        } else {
            tracing::warn!("S3 object processing failed");
        }
    }
    Ok(processed)
}

async fn s3_loop(state: AppState, cfg: S3ImportConfig) {
    let mut interval = tokio::time::interval(Duration::from_secs(cfg.poll_seconds));
    loop {
        interval.tick().await;
        match poll_s3(&state, &cfg).await {
            Ok(processed) => {
                set_source_health(
                    &state,
                    "S3-compatible bucket",
                    "ok",
                    format!("Connected; {processed} file(s) processed in the last poll"),
                )
                .await;
                if processed > 0 {
                    tracing::info!("S3 import poll processed {processed} file(s)")
                }
            }
            Err(error) => {
                set_source_health(
                    &state,
                    "S3-compatible bucket",
                    "degraded",
                    "The most recent poll failed; verify bucket access and connectivity".into(),
                )
                .await;
                tracing::warn!(
                    "S3 import poll failed: {}",
                    denial_common::logging::safe_error(error)
                );
            }
        }
    }
}

async fn retention_loop(cfg: Config) {
    let mut interval = tokio::time::interval(Duration::from_secs(86400));
    loop {
        interval.tick().await;
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| prune_old_files(&cfg))) {
            Ok(_) => {}
            Err(_) => tracing::error!("Retention sweep panicked"),
        }
    }
}

// ── Router / main ──

fn build_router(state: AppState) -> Router {
    let key = state.cfg.internal_service_api_key.clone();
    let protected = Router::new()
        .route("/ingest", post(ingest_file))
        .route("/ingest-dropzone", post(ingest_dropzone))
        .route("/files", get(list_files))
        .route("/output/{file_name}", get(get_output))
        .layer(axum::middleware::from_fn_with_state(
            key,
            denial_common::internal_auth::require_internal_key,
        ));
    let mut app = Router::new().route("/health", get(health)).merge(protected);

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
    std::fs::create_dir_all(&cfg.dropzone_path).expect("failed to create dropzone directory");
    std::fs::create_dir_all(&cfg.output_path).expect("failed to create output directory");
    prune_old_files(&cfg);

    let state = AppState {
        cfg: cfg.clone(),
        http: reqwest::Client::new(),
        limiter: Arc::new(SlidingWindowLimiter::new(
            60,
            Duration::from_secs(60),
            "ediparser",
        )),
        source_health: Arc::new(tokio::sync::RwLock::new({
            let mut sources = vec![SourceHealth {
                name: "Watched folder".into(),
                enabled: true,
                status: "ok".into(),
                detail: "Watching the configured dropzone".into(),
                last_checked_at: Some(chrono::Utc::now().to_rfc3339()),
            }];
            if cfg.sftp.is_some() {
                sources.push(SourceHealth {
                    name: "SFTP".into(),
                    enabled: true,
                    status: "starting".into(),
                    detail: "Waiting for first poll".into(),
                    last_checked_at: None,
                });
            }
            if cfg.s3.is_some() {
                sources.push(SourceHealth {
                    name: "S3-compatible bucket".into(),
                    enabled: true,
                    status: "starting".into(),
                    detail: "Waiting for first poll".into(),
                    last_checked_at: None,
                });
            }
            sources
        })),
    };

    let mut watcher = Watcher::new(cfg.dropzone_path.clone());
    watcher.seed_from_outputs(&cfg.output_path);

    let watch_state = state.clone();
    tokio::spawn(watch_loop(watcher, watch_state));

    let retention_cfg = cfg.clone();
    tokio::spawn(retention_loop(retention_cfg));

    if let Some(sftp_cfg) = cfg.sftp.clone() {
        let sftp_state = state.clone();
        tokio::spawn(sftp_loop(sftp_state, sftp_cfg));
        tracing::info!("SFTP importer enabled");
    }
    if let Some(s3_cfg) = cfg.s3.clone() {
        let s3_state = state.clone();
        tokio::spawn(s3_loop(s3_state, s3_cfg));
        tracing::info!("S3-compatible importer enabled");
    }

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
    axum::serve(listener, build_router(state))
        .await
        .expect("ediparser error");
}
