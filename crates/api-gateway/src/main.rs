mod docs;
mod middleware;
mod pdf;
mod reprocessing;
mod routes;
mod state;

use std::net::SocketAddr;
use std::time::Instant;

use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use state::AppState;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

async fn root() -> impl IntoResponse {
    Json(serde_json::json!({
        "service": "denial-navigator-api-gateway",
        "docs": "/docs"
    }))
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({"status": "ok"}))
}

async fn health_live() -> impl IntoResponse {
    Json(serde_json::json!({"status": "live"}))
}

async fn metrics(axum::extract::State(state): axum::extract::State<AppState>) -> Response {
    Response::builder()
        .header("content-type", "text/plain; version=0.0.4")
        .body(state.metrics.prometheus().into())
        .expect("static metrics response")
}

async fn metrics_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let started = Instant::now();
    let response = next.run(req).await;
    state.metrics.record(
        response.status().as_u16(),
        started.elapsed().as_micros() as u64,
    );
    response
}

async fn health_ready(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> impl IntoResponse {
    match sqlx::query("SELECT 1").execute(&state.pool).await {
        Ok(_) => (StatusCode::OK, Json(serde_json::json!({"status": "ready"}))),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"status": "not_ready"})),
        ),
    }
}

/// Browser-facing hardening headers. The React SPA and API are same-origin,
/// so this can be restrictive without breaking normal workflows.
async fn security_headers(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    for (name, value) in [
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "same-origin"),
        ("permissions-policy", "camera=(), microphone=(), geolocation=()"),
        ("content-security-policy", "default-src 'self'; connect-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'self'; frame-ancestors 'none'; form-action 'self'"),
    ] {
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    response
}

/// Attach an opaque correlation ID even when authorization rejects the
/// request. Caller-supplied IDs are intentionally ignored to avoid log
/// injection and cross-system identifier confusion.
async fn request_id(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let id = uuid::Uuid::new_v4().to_string();
    let mut response = next.run(req).await;
    response.headers_mut().insert(
        HeaderName::from_static("x-request-id"),
        HeaderValue::from_str(&id).expect("UUID is a valid header value"),
    );
    tracing::debug!(request_id = %id, status = %response.status(), "request completed");
    response
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let state = AppState::from_env().await?;

    let cors = CorsLayer::new()
        .allow_origin(
            state
                .config
                .cors_origins
                .iter()
                .filter_map(|s| HeaderValue::from_str(s).ok())
                .collect::<Vec<_>>(),
        )
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/health/live", get(health_live))
        .route("/health/ready", get(health_ready))
        .route("/metrics", get(metrics))
        .merge(docs::routes())
        .nest("/api/v1", routes::api_router())
        // Uploads (EDI files, reference-code CSVs, policy PDFs) stream through
        // handlers that each enforce their own byte cap; axum's 2 MB default
        // would reject them long before that. Keep a generous ceiling rather
        // than disabling the limit outright.
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            metrics_middleware,
        ))
        .layer(axum::middleware::from_fn(security_headers))
        .layer(cors)
        .layer(axum::middleware::from_fn_with_state(
            middleware::AccessState {
                pool: state.pool.clone(),
                config: state.config.clone(),
                trusted: state.config.trusted_proxies.clone(),
            },
            middleware::access,
        ))
        .layer(axum::middleware::from_fn_with_state(
            denial_audit::AuditState {
                pool: state.pool.clone(),
                config: state.config.clone(),
                trusted: state.config.trusted_proxies.clone(),
            },
            denial_audit::audit,
        ))
        .layer(axum::middleware::from_fn(request_id))
        .with_state(state);

    let addr: SocketAddr = std::env::var("API_GATEWAY_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8000".to_string())
        .parse()?;

    tracing::info!("api-gateway listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}
