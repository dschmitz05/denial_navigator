//! HIPAA access auditing.
//!
//! Ported from `api-gateway/services/audit.py`. Auditing is done as middleware
//! rather than a call in each handler, because a log with holes is worse than
//! no log: it reads as evidence of absence. A route added later is audited
//! without anyone remembering to wire it up.
//!
//! The audit layer sits OUTSIDE the access-control middleware, so it also
//! records the 401/403 attempts that access control rejects.

use std::net::SocketAddr;

use ipnet::IpNet;
use std::time::Instant;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::Response;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use denial_auth::rbac::{client_ip_from, resolve_from};
use denial_common::config::GatewayConfig;

/// Infrastructure, not PHI. Auditing these adds noise without recording a
/// single access to patient data.
const SKIP_EXACT: &[&str] = &[
    "/",
    "/health",
    "/docs",
    "/redoc",
    "/openapi.json",
    "/favicon.ico",
];

/// POST /auth/login writes its own entry; the Settings page polls health.
const SKIP_PREFIXES: &[&str] = &["/api/v1/auth/login", "/api/v1/system/health"];

/// path segment -> the `audit_log.resource_type` it represents.
const RESOURCE_TYPES: &[(&str, &str)] = &[
    ("claims", "claim"),
    ("denials", "denial"),
    ("appeals", "appeal"),
    ("analyses", "analysis"),
    ("knowledge", "knowledge_doc"),
    ("ingestion", "file"),
    ("feedback", "feedback"),
    ("users", "user"),
    ("auth", "user"),
    ("audit", "audit_log"),
];

/// Specific endpoints whose meaning is not captured by "<verb> <resource>".
/// Keyed by (method, path suffix).
const NAMED_ACTIONS: &[(&str, &str, &str)] = &[
    ("POST", "/analyses/generate", "generate_analysis"),
    ("POST", "/analyses/store", "store_analysis"),
    ("POST", "/ingestion/ingest", "ingest_file"),
    ("POST", "/ingestion/upload", "ingest_file"),
    ("POST", "/ingestion/store", "ingest_file"),
    ("POST", "/appeals", "submit_appeal"),
    ("POST", "/knowledge/documents", "update_knowledge"),
    ("POST", "/knowledge/documents/upload", "update_knowledge"),
    ("POST", "/knowledge/search", "search_knowledge"),
    ("GET", "/audit", "view_audit_log"),
    ("GET", "/audit/stats", "view_audit_log"),
];

/// (action, resource_type, resource_id) for one request.
pub fn classify(method: &str, path: &str) -> (String, String, Option<String>) {
    let trimmed = path.strip_prefix("/api/v1").unwrap_or(path);
    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();

    let resource_type = if segments.is_empty() {
        "system".to_string()
    } else {
        RESOURCE_TYPES
            .iter()
            .find(|(k, _)| *k == segments[0])
            .map(|(_, v)| (*v).to_string())
            .unwrap_or_else(|| segments[0].to_string())
    };

    // A trailing UUID is the record that was touched.
    let mut resource_id: Option<String> = None;
    for segment in segments.iter().rev() {
        if segment.parse::<Uuid>().is_ok() {
            resource_id = Some((*segment).to_string());
            break;
        }
    }

    let norm = trimmed.trim_end_matches('/');
    if let Some((_, _, action)) = NAMED_ACTIONS
        .iter()
        .find(|(m, p, _)| *m == method && *p == norm)
    {
        return (action.to_string(), resource_type, resource_id);
    }

    let (verb, verb_is_view) = match method {
        "GET" => ("view", true),
        "POST" => ("create", false),
        "PATCH" | "PUT" => ("edit", false),
        "DELETE" => ("delete", false),
        other => return (other.to_lowercase(), resource_type, resource_id),
    };
    // "view_claim" for one record, "list_claim" for a collection.
    let verb = if verb_is_view && resource_id.is_none() {
        "list"
    } else {
        verb
    };
    (
        format!("{verb}_{resource_type}"),
        resource_type,
        resource_id,
    )
}

const LABEL_COLUMNS: &[(&str, &[&str])] = &[
    ("claim", &["claim_number", "patient_name"]),
    (
        "denial",
        &["claim_number", "patient_name", "cpt_code", "carc_code"],
    ),
    (
        "appeal",
        &["claim_number", "patient_name", "resolution_type"],
    ),
    ("analysis", &["claim_number", "patient_name"]),
    ("user", &["target_username"]),
    ("knowledge_doc", &["document_title"]),
];

fn label_query(resource_type: &str) -> Option<&'static str> {
    match resource_type {
        "claim" => Some(
            "SELECT claim_number AS claim_number, patient_name \
             FROM claims WHERE id = $1::uuid",
        ),
        "denial" => Some(
            "SELECT c.claim_number, c.patient_name, d.cpt_code, d.carc_code \
             FROM denials d JOIN claims c ON c.id = d.claim_id \
             WHERE d.id = $1::uuid",
        ),
        "appeal" => Some(
            "SELECT c.claim_number, c.patient_name, aq.resolution_type \
             FROM appeals_queue aq \
             JOIN denials d ON d.id = aq.denial_id \
             JOIN claims c ON c.id = d.claim_id \
             WHERE aq.id = $1::uuid",
        ),
        "analysis" => Some(
            "SELECT c.claim_number, c.patient_name \
             FROM ai_analyses aa JOIN claims c ON c.id = aa.claim_id \
             WHERE aa.id = $1::uuid",
        ),
        "user" => Some("SELECT username AS target_username FROM users WHERE id = $1::uuid"),
        "knowledge_doc" => {
            Some("SELECT title AS document_title FROM knowledge_documents WHERE id = $1::uuid")
        }
        _ => None,
    }
}

/// Human identifiers for the record being touched, or `{}` if unavailable.
/// Never raises: an audit entry with a missing label is worth far more than a
/// request that failed because the lookup did.
async fn label_for(
    pool: &PgPool,
    resource_type: &str,
    resource_id: Option<&str>,
) -> serde_json::Value {
    let Some(id) = resource_id else {
        return serde_json::json!({});
    };
    let Some(query) = label_query(resource_type) else {
        return serde_json::json!({});
    };

    let row = match sqlx::query(query).bind(id).fetch_optional(pool).await {
        Ok(Some(row)) => row,
        Ok(None) => return serde_json::json!({"record": "no longer exists"}),
        Err(e) => {
            tracing::warn!("audit label lookup failed for {resource_type} {id}: {e}");
            return serde_json::json!({});
        }
    };

    let Some(cols) = LABEL_COLUMNS
        .iter()
        .find(|(t, _)| *t == resource_type)
        .map(|(_, c)| *c)
    else {
        return serde_json::json!({});
    };

    let mut obj = serde_json::Map::new();
    for c in cols {
        match row.try_get::<String, _>(*c) {
            Ok(v) => {
                obj.insert((*c).to_string(), serde_json::Value::String(v));
            }
            Err(_) => {
                // Numeric column (e.g. cpt_code).
                if let Ok(n) = row.try_get::<i32, _>(*c) {
                    obj.insert((*c).to_string(), serde_json::Value::String(n.to_string()));
                }
            }
        }
    }
    serde_json::Value::Object(obj)
}

/// Write one audit row. Never raises into the caller.
pub async fn record(
    pool: &PgPool,
    action: &str,
    resource_type: &str,
    resource_id: Option<&str>,
    user_id: Option<&str>,
    details: &serde_json::Value,
    ip_address: Option<&str>,
    user_agent: Option<&str>,
) {
    let uid = user_id.and_then(|s| Uuid::parse_str(s).ok());
    let rid = resource_id.and_then(|s| Uuid::parse_str(s).ok());

    if let Err(e) = sqlx::query(
        "INSERT INTO audit_log \
         (organization_id, user_id, action, resource_type, resource_id, details, ip_address, user_agent) \
         VALUES ((SELECT organization_id FROM organization_memberships \
                  WHERE user_id = $1 ORDER BY created_at ASC LIMIT 1), \
                 $1, $2, $3, $4, $5::jsonb, $6::inet, $7)",
    )
    .bind(uid)
    .bind(action)
    .bind(resource_type)
    .bind(rid)
    .bind(details)
    .bind(ip_address)
    .bind(user_agent)
    .execute(pool)
    .await
    {
        // An audit failure must be loud, but it must not take down the request
        // that was already served - the access happened either way.
        tracing::error!("AUDIT WRITE FAILED action={action} resource={resource_type}: {e}");
    }
}

/// A tower [`Layer`] recording every API request against the resource it
/// touched. Mount it so it runs OUTSIDE the access-control layer.
/// Audit middleware state.
#[derive(Clone)]
pub struct AuditState {
    pub pool: PgPool,
    pub config: GatewayConfig,
    pub trusted: Vec<IpNet>,
}

/// Audit middleware. Sits outside the access-control layer so it also records
/// rejected (401/403) attempts.
pub async fn audit(State(state): State<AuditState>, req: Request, next: Next) -> Response {
    let pool = &state.pool;
    let config = &state.config;
    let trusted = &state.trusted;

    {
        let method = req.method().to_string();
        let path = req.uri().path().to_string();
        let query = req.uri().query().map(|s| s.to_string());
        let user_agent = req
            .headers()
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
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
        let peer: Option<SocketAddr> = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0);

        let skip = method == "OPTIONS"
            || SKIP_EXACT.contains(&path.as_str())
            || SKIP_PREFIXES.iter().any(|p| path.starts_with(p))
            || !path.starts_with("/api/");
        if skip {
            return next.run(req).await;
        }

        let started = Instant::now();
        let response = next.run(req).await;
        let duration_ms = started.elapsed().as_millis() as u64;
        let status: u16 = response.status().as_u16();

        let (action, resource_type, resource_id) = classify(&method, &path);
        let principal = resolve_from(
            authorization.as_deref(),
            service_name.as_deref(),
            service_key.as_deref(),
            &config,
        );
        let user_id = principal.user_id.clone();
        let username = principal.username.clone();

        let mut details = serde_json::Map::new();
        details.insert("method".into(), serde_json::json!(method));
        details.insert("path".into(), serde_json::json!(path));
        details.insert("status_code".into(), serde_json::json!(status));
        details.insert("duration_ms".into(), serde_json::json!(duration_ms));
        details.insert(
            "outcome".into(),
            serde_json::json!(if status < 400 { "success" } else { "failure" }),
        );

        if resource_id.is_some() {
            if let serde_json::Value::Object(m) =
                label_for(&pool, &resource_type, resource_id.as_deref()).await
            {
                for (k, v) in m {
                    details.insert(k, v);
                }
            }
        }
        if let Some(q) = query {
            details.insert("query".into(), serde_json::json!(q));
        }
        if username != "anonymous" {
            details.insert("username".into(), serde_json::json!(username));
        } else if user_id.is_none() {
            details.insert("username".into(), serde_json::json!("anonymous"));
        }

        let ip = client_ip_from(
            peer,
            x_real_ip.as_deref(),
            x_forwarded_for.as_deref(),
            &trusted,
        );
        record(
            &pool,
            &action,
            &resource_type,
            resource_id.as_deref(),
            user_id.as_deref(),
            &serde_json::Value::Object(details),
            ip.as_deref(),
            user_agent.as_deref(),
        )
        .await;

        response
    }
}

#[cfg(test)]
mod tests {
    use super::classify;

    #[test]
    fn classifies_a_single_resource_view() {
        let id = "2d809714-2f0a-4d20-8dc7-1daab697ce8d";
        let (action, resource_type, resource_id) = classify("GET", &format!("/api/v1/claims/{id}"));

        assert_eq!(action, "view_claim");
        assert_eq!(resource_type, "claim");
        assert_eq!(resource_id.as_deref(), Some(id));
    }

    #[test]
    fn uses_the_named_action_for_analysis_generation() {
        let (action, resource_type, resource_id) = classify("POST", "/api/v1/analyses/generate");

        assert_eq!(action, "generate_analysis");
        assert_eq!(resource_type, "analysis");
        assert_eq!(resource_id, None);
    }
}
