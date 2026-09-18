//! Self-contained API documentation, mirroring the FastAPI service.
//!
//! `/openapi.json` is generated from the gateway route inventory plus detailed
//! request/response schemas in `crates/api-gateway/openapi/generate.py`.
//! `/docs` and `/redoc`
//! are thin HTML shells that load the Swagger UI / ReDoc bundles vendored
//! under `/static/docs/` - nothing here reaches a CDN, so the docs work on an
//! air-gapped host.

use std::path::PathBuf;

use axum::http::header;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tower_http::services::ServeDir;

use crate::state::AppState;

/// The generated API description, baked into the binary so `/openapi.json`
/// works with no file dependency. Regenerate with
/// `crates/api-gateway/openapi/generate.py`.
const OPENAPI_JSON: &str = include_str!("../openapi/openapi.json");

/// Directory holding the vendored Swagger UI / ReDoc bundles under `docs/`.
/// `/app/static` in the container image; the repo copy when run from source.
pub fn static_dir() -> PathBuf {
    std::env::var("STATIC_DIR")
        .unwrap_or_else(|_| "crates/api-gateway/static".to_string())
        .into()
}

const SWAGGER_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Denial Navigator API — Swagger UI</title>
  <link rel="stylesheet" href="/static/docs/swagger-ui.css">
  <link rel="shortcut icon" href="/static/docs/favicon.svg">
</head>
<body>
  <div id="swagger-ui"></div>
  <script src="/static/docs/swagger-ui-bundle.js"></script>
  <script>
    window.ui = SwaggerUIBundle({
      url: "/openapi.json",
      dom_id: "#swagger-ui",
      deepLinking: true,
      persistAuthorization: true
    });
  </script>
</body>
</html>
"##;

const REDOC_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Denial Navigator API — API Reference</title>
  <link rel="shortcut icon" href="/static/docs/favicon.svg">
  <style>body { margin: 0; padding: 0; }</style>
</head>
<body>
  <redoc spec-url="/openapi.json"></redoc>
  <script src="/static/docs/redoc.standalone.js"></script>
</body>
</html>
"##;

async fn swagger_ui() -> Html<&'static str> {
    Html(SWAGGER_HTML)
}

async fn redoc_ui() -> Html<&'static str> {
    Html(REDOC_HTML)
}

async fn openapi_json() -> Response {
    ([(header::CONTENT_TYPE, "application/json")], OPENAPI_JSON).into_response()
}

/// The doc routes, mounted at the app root (outside `/api/v1`). All are
/// treated as public by the access-control layer.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/docs", get(swagger_ui))
        .route("/redoc", get(redoc_ui))
        .route("/openapi.json", get(openapi_json))
        .nest_service("/static", ServeDir::new(static_dir()))
}
