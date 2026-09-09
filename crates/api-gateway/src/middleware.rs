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

    if MFA_ONLY_PATHS.contains(&path.as_str()) {
        let claims = match &authorization {
            Some(auth) if auth.starts_with("Bearer ") => {
                let token = &auth["Bearer ".len()..];
                denial_common::auth::decode_token(token, &config.jwt_secret)
                    .map_err(|_| AppError::Unauthorized)?
            }
            _ => return Err(AppError::Unauthorized),
        };
        if claims.scope != "mfa" {
            return Err(AppError::Forbidden);
        }
        let principal = Principal {
            kind: PrincipalKind::User,
            user_id: Some(claims.sub.clone()),
            username: claims.username.clone(),
            role: Some(claims.role.clone()),
            reason: None,
            issued_at: Some(claims.iat),
            scope: Some(claims.scope.clone()),
            ip: None,
        };
        let mut req = req;
        req.extensions_mut().insert(principal);
        return Ok(next.run(req).await);
    }

    let principal = resolve_from(
        authorization.as_deref(),
        service_name.as_deref(),
        service_key.as_deref(),
        config,
    );

    if matches!(principal.kind, PrincipalKind::Anonymous) {
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
