mod docs;
mod middleware;
mod routes;
mod state;

use std::net::SocketAddr;

use axum::http::{HeaderValue, StatusCode};
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
        .merge(docs::routes())
        .nest("/api/v1", routes::api_router())
        // Uploads (EDI files, reference-code CSVs, policy PDFs) stream through
        // handlers that each enforce their own byte cap; axum's 2 MB default
        // would reject them long before that. Keep a generous ceiling rather
        // than disabling the limit outright.
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
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
            denial_common::audit::AuditState {
                pool: state.pool.clone(),
                config: state.config.clone(),
                trusted: state.config.trusted_proxies.clone(),
            },
            denial_common::audit::audit,
        ))
        .with_state(state);

    let addr: SocketAddr = std::env::var("API_GATEWAY_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8000".to_string())
        .parse()?;

    tracing::info!("api-gateway listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
