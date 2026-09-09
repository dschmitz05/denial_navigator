use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, Method};
use axum::middleware::Next;
use axum::response::Response;
use denial_common::config::GatewayConfig;
use denial_common::rbac::{client_ip_from, resolve_from, Principal, PrincipalKind};
use denial_common::AppError;

const PUBLIC_EXACT: &[&str] = &[
    "/",
    "/health",
    "/docs",
    "/redoc",
    "/openapi.json",
    "/favicon.ico",
    "/api/v1/auth/login",
];

const PUBLIC_PREFIXES: &[&str] = &["/static"];

const MFA_ONLY_PATHS: &[&str] = &[
    "/api/v1/auth/totp/enroll",
    "/api/v1/auth/totp/confirm",
    "/api/v1/auth/login/totp",
    "/api/v1/auth/me",
];

/// Access-control middleware state.
#[derive(Clone)]
pub struct AccessState {
    pub config: GatewayConfig,
    pub trusted: Vec<IpAddr>,
}

/// Access-control middleware. Mirrors `api-gateway/services/access.py`.
pub async fn access(
    State(state): State<AccessState>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let config = &state.config;
    let trusted = &state.trusted;

    let method = req.method().clone();
    let path = req.uri().path().to_string();

    if method == Method::OPTIONS
        || PUBLIC_EXACT.contains(&path.as_str())
        || PUBLIC_PREFIXES.iter().any(|p| path.starts_with(p))
        || !path.starts_with("/api/")
    {
        return Ok(next.run(req).await);
    }

    let authorization = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let service_name = req
        .headers()
        .get("x-service-name")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let service_key = req
        .headers()
        .get("x-service-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let principal = resolve_from(
        authorization.as_deref(),
        service_name.as_deref(),
        service_key.as_deref(),
        config,
    );

    if matches!(principal.kind, PrincipalKind::Anonymous) {
        return Err(AppError::Unauthorized);
    }

    // A token that has only cleared the password step (scope "mfa") is confined
    // to the endpoints that complete the second factor. A full-scope token is
    // free to use those same endpoints and everything else. Mirrors
    // `access.py`: `if who.scope == "mfa" and path not in MFA_ONLY_PATHS`.
    if principal.kind == PrincipalKind::User
        && principal.scope.as_deref() == Some("mfa")
        && !MFA_ONLY_PATHS.contains(&path.as_str())
    {
        return Err(AppError::Unauthorized);
    }

    let peer: Option<SocketAddr> =
        req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0);
    let x_real_ip = req
        .headers()
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let x_forwarded_for = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let ip = client_ip_from(
        peer,
        x_real_ip.as_deref(),
        x_forwarded_for.as_deref(),
        trusted,
    );
    let principal = Principal {
        ip,
        ..principal
    };

    let mut req = req;
    req.extensions_mut().insert(principal);
    Ok(next.run(req).await)
}
