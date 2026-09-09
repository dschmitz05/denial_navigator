//! Application error type and its HTTP rendering.
//!
//! Error bodies mirror the FastAPI `HTTPException` shape (`{"detail": ...}`) so
//! the existing frontend keeps working unchanged during the rewrite.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Unprocessable(String),
    #[error("rate limited")]
    RateLimited { retry_after: u64 },
    #[error("{0}")]
    Upstream(String),
    #[error("{0}")]
    Internal(String),
    #[error("database error")]
    Db(#[from] sqlx::Error),
}

impl AppError {
    /// HTTP status for the error variant.
    pub fn status(&self) -> StatusCode {
        match self {
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::Forbidden => StatusCode::FORBIDDEN,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            AppError::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            AppError::Upstream(_) => StatusCode::BAD_GATEWAY,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The `detail` string sent to the client.
    pub fn detail(&self) -> String {
        match self {
            AppError::Db(_) => "internal error".to_string(),
            other => other.to_string(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let detail = self.detail();
        let body = json!({ "detail": detail });
        let headers: Vec<(axum::http::HeaderName, String)> = match &self {
            AppError::RateLimited { retry_after } => vec![(
                axum::http::HeaderName::from_static("retry-after"),
                retry_after.to_string(),
            )],
            _ => vec![],
        };

        let mut response = (status, Json(body)).into_response();
        for (name, value) in headers {
            if let Ok(v) = value.parse() {
                response.headers_mut().insert(name, v);
            }
        }
        response
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        AppError::Upstream(e.to_string())
    }
}
