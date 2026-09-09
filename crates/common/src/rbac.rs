//! Access control: identity resolution, role-based authorisation, and the
//! per-request account-currency check.
//!
//! Ported faithfully from `api-gateway/services/access.py`. Two kinds of caller
//! are legitimate: a person carrying a JWT from `/auth/login`, and a sibling
//! service presenting the shared `SERVICE_API_KEY`. Everything else is refused
//! 401 before a handler runs.
//!
//! Roles come from `database/init.sql`:
//!   billing_specialist  queue operations, view claims
//!   billing_manager     policy management, bulk operations
//!   rcm_director        full data access, reporting
//!   admin               system configuration

use std::net::{IpAddr, SocketAddr};

use ipnet::IpNet;

use axum::extract::Request;
use axum::http::header;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use sqlx::PgPool;
use sqlx::Row;

use crate::config::GatewayConfig;

/// The class of caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrincipalKind {
    User,
    Service,
    Anonymous,
}

/// Who is making this request.
#[derive(Clone, Debug)]
pub struct Principal {
    pub kind: PrincipalKind,
    pub user_id: Option<String>,
    pub username: String,
    pub role: Option<String>,
    /// Why authentication failed, if it did.
    pub reason: Option<String>,
    /// JWT `iat`, epoch seconds.
    pub issued_at: Option<i64>,
    /// `Some("mfa")` for a half-authenticated token.
    pub scope: Option<String>,
    /// Resolved client IP, set by the middleware for audit/logging.
    pub ip: Option<String>,
}

impl Principal {
    pub fn authenticated(&self) -> bool {
        matches!(self.kind, PrincipalKind::User | PrincipalKind::Service)
    }

    pub fn is_service(&self) -> bool {
        self.kind == PrincipalKind::Service
    }

    pub fn anonymous() -> Self {
        Self {
            kind: PrincipalKind::Anonymous,
            user_id: None,
            username: "anonymous".to_string(),
            role: None,
            reason: None,
            issued_at: None,
            scope: None,
            ip: None,
        }
    }
}

const SPECIALIST: &str = "billing_specialist";
const MANAGER: &str = "billing_manager";
const DIRECTOR: &str = "rcm_director";
const ADMIN: &str = "admin";

const ALL_ROLES: &[&str] = &[SPECIALIST, MANAGER, DIRECTOR, ADMIN];
const MANAGER_UP: &[&str] = &[MANAGER, DIRECTOR, ADMIN];
const ADMIN_ONLY: &[&str] = &[ADMIN];
const NOBODY: &[&str] = &[];

/// A token that has only cleared the password step may only touch these.
const MFA_ONLY_PATHS: &[&str] = &[
    "/api/v1/auth/totp/enroll",
    "/api/v1/auth/totp/confirm",
    "/api/v1/auth/login/totp",
    "/api/v1/auth/me",
];

/// Reachable without credentials.
const PUBLIC_EXACT: &[&str] = &[
    "/",
    "/health",
    "/docs",
    "/redoc",
    "/openapi.json",
    "/favicon.ico",
    "/api/v1/auth/login",
];

const WRITE_METHODS: &[&str] = &["POST", "PUT", "PATCH", "DELETE"];

/// POST endpoints that only read.
const READ_ONLY_POSTS: &[&str] = &["/api/v1/knowledge/search", "/api/v1/ingestion/log"];

/// Resolve the caller from the request. Never raises, never rejects — that is
/// the caller's job.
pub fn resolve_principal(req: &Request, config: &GatewayConfig) -> Principal {
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
    resolve_from(authorization.as_deref(), service_name.as_deref(), service_key.as_deref(), config)
}

/// Resolve the caller from already-extracted headers. Used by the audit
/// middleware, which runs after the request has been consumed by the handler.
pub fn resolve_from(
    authorization: Option<&str>,
    service_name: Option<&str>,
    service_key: Option<&str>,
    config: &GatewayConfig,
) -> Principal {
    // 1. Sibling service: X-Service-Key.
    if let Some(key) = service_key {
        let ok = match &config.service_api_key {
            Some(expected) => constant_time_eq(key.as_bytes(), expected.as_bytes()),
            // No shared key configured: service auth is disabled, refuse.
            None => false,
        };
        if ok {
            return Principal {
                kind: PrincipalKind::Service,
                user_id: None,
                username: format!("service:{}", service_name.unwrap_or("unknown")),
                role: None,
                reason: None,
                issued_at: None,
                scope: None,
                ip: None,
            };
        }
        return Principal {
            kind: PrincipalKind::Anonymous,
            user_id: None,
            username: "anonymous".to_string(),
            role: None,
            reason: Some("invalid_service_key".to_string()),
            issued_at: None,
            scope: None,
            ip: None,
        };
    }

    // 2. Bearer JWT.
    let header = authorization.unwrap_or("");
    let token = if header.to_lowercase().starts_with("bearer ") {
        &header[7..]
    } else {
        return Principal {
            kind: PrincipalKind::Anonymous,
            user_id: None,
            username: "anonymous".to_string(),
            role: None,
            reason: Some("no_credentials".to_string()),
            issued_at: None,
            scope: None,
            ip: None,
        };
    };

    let claims = match crate::auth::decode_token(token, &config.jwt_secret) {
        Ok(c) => c,
        Err(e) => {
            let reason = if e.to_string().to_lowercase().contains("expired") {
                "expired_token"
            } else {
                "invalid_token"
            };
            return Principal {
                kind: PrincipalKind::Anonymous,
                user_id: None,
                username: "anonymous".to_string(),
                role: None,
                reason: Some(reason.to_string()),
                issued_at: None,
                scope: None,
                ip: None,
            };
        }
    };

    // The subject must be a well-formed UUID.
    if claims.sub.parse::<uuid::Uuid>().is_err() {
        return Principal {
            kind: PrincipalKind::Anonymous,
            user_id: None,
            username: "anonymous".to_string(),
            role: None,
            reason: Some("malformed_subject".to_string()),
            issued_at: None,
            scope: None,
            ip: None,
        };
    }

    Principal {
        kind: PrincipalKind::User,
        user_id: Some(claims.sub),
        username: claims.username,
        role: Some(claims.role),
        reason: None,
        issued_at: Some(claims.iat),
        scope: Some(claims.scope),
        ip: None,
    }
}

/// Is this path reachable without credentials?
pub fn is_public(path: &str, method: &str) -> bool {
    if method == "OPTIONS" {
        return true;
    }
    if PUBLIC_EXACT.contains(&path) {
        return true;
    }
    // Only the API is protected; anything else is static/infrastructure.
    !path.starts_with("/api/")
}

/// Replace UUID segments with `{id}` so a path rule matches any record.
fn normalise(path: &str) -> String {
    let mut out = Vec::with_capacity(path.matches('/').count() + 1);
    for segment in path.split('/') {
        if segment.parse::<uuid::Uuid>().is_ok() {
            out.push("{id}".to_string());
        } else {
            out.push(segment.to_string());
        }
    }
    out.join("/")
}

/// The first non-empty segment after the `/api/v1` prefix.
fn resource_of(path: &str) -> String {
    let trimmed = path.strip_prefix("/api/v1").unwrap_or(path);
    trimmed
        .split('/')
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string()
}

/// `(read_roles, write_roles)` for a resource, or `None` when unknown.
fn permissions(resource: &str) -> Option<(&'static [&'static str], &'static [&'static str])> {
    Some(match resource {
        "claims" => (ALL_ROLES, MANAGER_UP),
        "denials" => (ALL_ROLES, ALL_ROLES),
        "appeals" => (ALL_ROLES, ALL_ROLES),
        "analyses" => (ALL_ROLES, ALL_ROLES),
        "feedback" => (ALL_ROLES, ALL_ROLES),
        "knowledge" => (ALL_ROLES, MANAGER_UP),
        "ingestion" => (ALL_ROLES, MANAGER_UP),
        "reference" => (ALL_ROLES, MANAGER_UP),
        "audit" => (MANAGER_UP, NOBODY),
        "users" => (ADMIN_ONLY, ADMIN_ONLY),
        "auth" => (ALL_ROLES, ALL_ROLES),
        "system" => (ALL_ROLES, NOBODY),
        "retention" => (ADMIN_ONLY, ADMIN_ONLY),
        "notifications" => (ALL_ROLES, ALL_ROLES),
        _ => return None,
    })
}

/// Path-level exceptions, checked before the resource rules.
fn path_permission(method: &str, norm: &str) -> Option<&'static [&'static str]> {
    Some(match (method, norm) {
        ("GET", "/api/v1/users/assignable") => MANAGER_UP,
        ("POST", "/api/v1/appeals/{id}/assign") => MANAGER_UP,
        ("POST", "/api/v1/users/{id}/totp") => ADMIN_ONLY,
        ("POST", "/api/v1/users/{id}/totp/reset") => ADMIN_ONLY,
        ("PUT", "/api/v1/denials/appeal-windows") => MANAGER_UP,
        _ => return None,
    })
}

/// Decide whether `who` may perform `method` on `path`.
///
/// Returns `Ok(())` when allowed, or `Err(reason)` when denied.
pub fn authorize(who: &Principal, method: &str, path: &str) -> Result<(), String> {
    // Sibling services are not people and hold no role.
    if who.kind == PrincipalKind::Service {
        return Ok(());
    }
    // A half-authenticated token is confined to MFA_ONLY_PATHS by the
    // middleware; nothing further to decide.
    if who.scope.as_deref() == Some("mfa") {
        return Ok(());
    }

    let role = who.role.clone().unwrap_or_default();

    if let Some(allowed) = path_permission(method, &normalise(path)) {
        return if allowed.iter().any(|r| *r == role) {
            Ok(())
        } else {
            Err(format!("role '{role}' may not use {}", normalise(path)))
        };
    }

    let resource = resource_of(path);
    let rules = permissions(&resource)
        .ok_or_else(|| format!("no permission rule for '{resource}'"))?;
    let is_write = WRITE_METHODS.contains(&method) && !READ_ONLY_POSTS.contains(&path);
    let allowed = if is_write { rules.1 } else { rules.0 };

    if allowed.iter().any(|r| *r == role) {
        Ok(())
    } else {
        Err(format!(
            "role '{role}' may not {} {resource}",
            if is_write { "write" } else { "read" }
        ))
    }
}

/// Is the account behind this token still entitled to use it?
///
/// One indexed primary-key lookup per request, deliberately not cached: a cache
/// TTL is exactly the window in which a revoked session still works.
pub async fn account_is_current(pool: &PgPool, who: &Principal) -> Result<(), String> {
    let user_id = match &who.user_id {
        Some(id) => id.clone(),
        None => return Err("no_user_id".to_string()),
    };

    let row = sqlx::query(
        // EXTRACT(EPOCH FROM ...) is NUMERIC in Postgres; cast so it decodes
        // into f64 rather than panicking the worker on every request.
        "SELECT is_active, EXTRACT(EPOCH FROM sessions_valid_from)::float8 AS valid_from, role \
         FROM users WHERE id = $1::uuid",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await;

    let row = match row {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(
                "account check failed for {}: {e}",
                who.username
            );
            return Err("account_check_unavailable".to_string());
        }
    };

    let Some(row) = row else {
        return Err("account_deleted".to_string());
    };

    // Never panic in the access path: a decode error here would drop the
    // connection and surface as a 502, not a 401.
    let (Ok(is_active), Ok(valid_from), Ok(role)) = (
        row.try_get::<bool, _>("is_active"),
        row.try_get::<Option<f64>, _>("valid_from"),
        row.try_get::<String, _>("role"),
    ) else {
        return Err("account_check_unavailable".to_string());
    };

    if !is_active {
        return Err("account_deactivated".to_string());
    }
    if who.role.as_deref() != Some(role.as_str()) {
        return Err("role_changed".to_string());
    }
    if let (Some(vf), Some(iat)) = (valid_from, who.issued_at) {
        if (iat as f64) < vf {
            return Err("credentials_changed".to_string());
        }
    }
    Ok(())
}

/// The outcome of running the full access-control decision for one request.
pub enum Decision {
    /// No credentials required; pass straight through.
    Public,
    /// 401 with the given detail.
    Unauthorized(String),
    /// 403 with the given detail.
    Forbidden(String),
    /// Authenticated and authorised; the principal is ready to attach.
    Authorized(Principal),
}

/// The parts of a request the access decision needs, lifted out of the
/// `axum::Request` before any `.await`. `axum::body::Body` is `!Sync`, so a
/// middleware future that held `&Request` across the account-currency query
/// would not be `Send` - hence this owned snapshot.
#[derive(Clone, Debug, Default)]
pub struct RequestCtx {
    pub method: String,
    pub path: String,
    pub authorization: Option<String>,
    pub service_name: Option<String>,
    pub service_key: Option<String>,
    pub x_real_ip: Option<String>,
    pub x_forwarded_for: Option<String>,
    pub peer: Option<SocketAddr>,
}

impl RequestCtx {
    /// Extract everything the decision needs. Pure and synchronous.
    pub fn from_request(req: &Request, peer: Option<SocketAddr>) -> Self {
        let h = |name: &str| {
            req.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        };
        Self {
            method: req.method().to_string(),
            path: req.uri().path().to_string(),
            authorization: h("authorization"),
            service_name: h("x-service-name"),
            service_key: h("x-service-key"),
            x_real_ip: h("x-real-ip"),
            x_forwarded_for: h("x-forwarded-for"),
            peer,
        }
    }
}

/// Run the complete decision for one request, in order:
/// public → identity → MFA confinement → account currency → role authorisation.
pub async fn decide(
    pool: &PgPool,
    ctx: &RequestCtx,
    config: &GatewayConfig,
    trusted_proxies: &[IpNet],
) -> Decision {
    if is_public(&ctx.path, &ctx.method) {
        return Decision::Public;
    }

    let mut who = resolve_from(
        ctx.authorization.as_deref(),
        ctx.service_name.as_deref(),
        ctx.service_key.as_deref(),
        config,
    );
    who.ip = client_ip_from(
        ctx.peer,
        ctx.x_real_ip.as_deref(),
        ctx.x_forwarded_for.as_deref(),
        trusted_proxies,
    );

    // A token that has only cleared the password step is confined to the
    // endpoints that complete the second factor.
    if who.kind == PrincipalKind::User
        && who.scope.as_deref() == Some("mfa")
        && !MFA_ONLY_PATHS.contains(&ctx.path.as_str())
    {
        return Decision::Unauthorized(
            "Two-factor authentication has not been completed".to_string(),
        );
    }

    // A valid signature is not enough; the account behind it must still be
    // entitled to it. Services hold no account, so they skip this.
    if who.kind == PrincipalKind::User {
        if let Err(why) = account_is_current(pool, &who).await {
            return Decision::Unauthorized(format!("Not authenticated: {why}"));
        }
    }

    if !who.authenticated() {
        return Decision::Unauthorized("Not authenticated".to_string());
    }

    match authorize(&who, &ctx.method, &ctx.path) {
        Ok(()) => Decision::Authorized(who),
        Err(reason) => Decision::Forbidden(format!("Access denied: {reason}")),
    }
}

/// The end user's IP, not the proxy's — and not one the caller invented.
///
/// Forwarded headers are honoured only when the request actually arrived from
/// a trusted proxy, and validated before reaching an INET column either way.
/// `X-Real-IP` is preferred (nginx sets it to `$remote_addr`, which the caller
/// cannot influence). `X-Forwarded-For` is read from the right-most entry, the
/// one our own proxy appended; the left-most is attacker-controlled.
pub fn client_ip(peer: Option<SocketAddr>, req: &Request, trusted: &[IpNet]) -> Option<String> {
    let real = req.headers().get("x-real-ip").and_then(|v| v.to_str().ok()).map(|s| s.to_string());
    let fwd = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    client_ip_from(peer, real.as_deref(), fwd.as_deref(), trusted)
}

/// Same as [`client_ip`] but from already-extracted headers, for the audit
/// middleware that runs after the request has been consumed.
pub fn client_ip_from(
    peer: Option<SocketAddr>,
    x_real_ip: Option<&str>,
    x_forwarded_for: Option<&str>,
    trusted: &[IpNet],
) -> Option<String> {
    let peer_ip: Option<IpAddr> = peer.map(|p| p.ip());

    let is_trusted = peer_ip
        .map(|ip| trusted.iter().any(|net| net.contains(&ip)))
        .unwrap_or(false);
    if is_trusted {
        if let Some(real) = x_real_ip {
            if let Ok(ip) = real.trim().parse::<IpAddr>() {
                return Some(ip.to_string());
            }
        }
        if let Some(fwd) = x_forwarded_for {
            if let Some(candidate) = fwd.rsplit(',').next() {
                if let Ok(ip) = candidate.trim().parse::<IpAddr>() {
                    return Some(ip.to_string());
                }
            }
        }
    }

    peer_ip.map(|ip| ip.to_string())
}

/// Attach a principal to the request so handlers can read it via
/// `Extension<Principal>`.
pub fn attach_principal(req: &mut Request, principal: Principal) {
    req.extensions_mut().insert(principal);
}

/// A 403 JSON response with a short reason.
pub fn forbidden(reason: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        axum::Json(serde_json::json!({ "detail": reason })),
    )
        .into_response()
}

/// A 401 JSON response with a short reason.
pub fn unauthorized(detail: &str) -> Response {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        header::WWW_AUTHENTICATE,
        axum::http::HeaderValue::from_static("Bearer"),
    );
    (
        StatusCode::UNAUTHORIZED,
        headers,
        axum::Json(serde_json::json!({ "detail": detail })),
    )
        .into_response()
}

/// Constant-time byte comparison (avoids a timing oracle on the service key).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}
