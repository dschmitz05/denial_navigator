//! Authentication for private, service-to-service HTTP endpoints.

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::AppError;

/// Refuse requests without the deployment's internal service credential.
/// Health endpoints are deliberately kept outside this middleware so Docker
/// can probe them without exposing mutating or data-returning routes.
pub async fn require_internal_key(
    State(expected): State<String>,
    req: Request,
    next: Next,
) -> Response {
    let supplied = req
        .headers()
        .get("x-internal-service-key")
        .and_then(|v| v.to_str().ok());
    match supplied {
        Some(value) if constant_time_eq(value.as_bytes(), expected.as_bytes()) => {
            next.run(req).await
        }
        _ => AppError::Unauthorized.into_response(),
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut different = 0u8;
    for (x, y) in a.iter().zip(b) {
        different |= x ^ y;
    }
    different == 0
}
