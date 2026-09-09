//! Deadline notifications.
//!
//! Ported from `api-gateway/routes/notifications.py`. Two kinds: a
//! `deadline_digest` to the person who owns the work, and an
//! `overdue_escalation` to managers for overdue work nobody owns. Generation
//! is idempotent by `(user, kind, day)` - the unique index, not an assumption
//! about how often the cron calls this, is what makes "once a day" hold.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, NaiveDate, Utc};
use denial_common::error::AppError;
use denial_common::rbac::{Principal, PrincipalKind};
use serde::Deserialize;
use sqlx::Row;
use uuid::Uuid;

use crate::state::AppState;

/// admin or billing_manager or rcm_director - the roles an unowned overdue
/// denial is escalated to.
const MANAGER_UP: &[&str] = &["billing_manager", "rcm_director", "admin"];

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub unread_only: bool,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    50
}

/// The caller's own user id, or 401. Nobody reads anyone else's notifications.
fn me(principal: &Principal) -> Result<Uuid, AppError> {
    if principal.kind != PrincipalKind::User {
        return Err(AppError::Unauthorized);
    }
    principal
        .user_id
        .as_deref()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(AppError::Unauthorized)
}

/// `12345.6` -> `"12,345.60"`, matching Python's `{:,.2f}`.
fn money(v: f64) -> String {
    let neg = v.is_sign_negative();
    let cents = format!("{:.2}", v.abs());
    let (int_part, frac) = cents.split_once('.').unwrap_or((cents.as_str(), "00"));
    let bytes = int_part.as_bytes();
    let mut grouped = String::new();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(*b as char);
    }
    format!("{}{grouped}.{frac}", if neg { "-" } else { "" })
}

pub async fn list_notifications(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<ListQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let user_id = me(&principal)?;
    let limit = params.limit.clamp(1, 200);

    let sql = format!(
        "SELECT id, kind, for_date, title, body, payload, read_at, created_at \
           FROM notifications \
          WHERE user_id = $1 {} \
          ORDER BY created_at DESC \
          LIMIT $2",
        if params.unread_only { "AND read_at IS NULL" } else { "" }
    );

    let rows = sqlx::query(&sql)
        .bind(user_id)
        .bind(limit)
        .fetch_all(&state.pool)
        .await
        .map_err(AppError::Db)?;

    let out = rows
        .iter()
        .map(|r| {
            let id: Option<Uuid> = r.try_get("id").ok().flatten();
            let for_date: Option<NaiveDate> = r.try_get("for_date").ok().flatten();
            let created_at: Option<DateTime<Utc>> = r.try_get("created_at").ok().flatten();
            let read_at: Option<DateTime<Utc>> = r.try_get("read_at").ok().flatten();
            serde_json::json!({
                "id": id.map(|u| u.to_string()),
                "kind": r.try_get::<Option<String>, _>("kind").ok().flatten(),
                "for_date": for_date.map(|d| d.to_string()),
                "title": r.try_get::<Option<String>, _>("title").ok().flatten(),
                "body": r.try_get::<Option<String>, _>("body").ok().flatten(),
                "payload": r
                    .try_get::<Option<serde_json::Value>, _>("payload")
                    .ok()
                    .flatten()
                    .unwrap_or(serde_json::Value::Null),
                "read_at": read_at.map(|t| t.to_rfc3339()),
                "created_at": created_at.map(|t| t.to_rfc3339()),
            })
        })
        .collect();

    Ok(Json(out))
}

pub async fn mark_read(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(notification_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let user_id = me(&principal)?;
    let row = sqlx::query(
        "UPDATE notifications SET read_at = NOW() \
          WHERE id = $1 AND user_id = $2 AND read_at IS NULL \
        RETURNING id",
    )
    .bind(notification_id)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?;

    Ok(Json(serde_json::json!({
        "status": if row.is_some() { "read" } else { "not_found_or_already_read" }
    })))
}

pub async fn mark_all_read(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<serde_json::Value>, AppError> {
    let user_id = me(&principal)?;
    let result = sqlx::query(
        "UPDATE notifications SET read_at = NOW() WHERE user_id = $1 AND read_at IS NULL",
    )
    .bind(user_id)
    .execute(&state.pool)
    .await
    .map_err(AppError::Db)?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "marked": result.rows_affected(),
    })))
}

pub async fn generate_digests(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<serde_json::Value>, AppError> {
    // A service credential or an admin - not something a specialist triggers,
    // since it writes to everyone's notifications.
    let allowed = principal.kind == PrincipalKind::Service
        || principal.role.as_deref() == Some("admin");
    if !allowed {
        return Err(AppError::Forbidden);
    }

    let horizon = state.digest_horizon_days as i64;
    let mut created: i64 = 0;
    let mut escalated: i64 = 0;

    // ── per-owner digests ──
    let owners = sqlx::query(&format!(
        "SELECT aq.assigned_user_id AS user_id, \
                COUNT(*) FILTER (WHERE d.appeal_deadline < CURRENT_DATE)  AS overdue, \
                COUNT(*) FILTER (WHERE d.appeal_deadline >= CURRENT_DATE) AS upcoming, \
                MIN(d.appeal_deadline) AS soonest, \
                SUM(d.charge_amount)::float8 AS amount, \
                json_agg(json_build_object( \
                    'claim_number', c.claim_number, \
                    'deadline', d.appeal_deadline, \
                    'amount', d.charge_amount \
                ) ORDER BY d.appeal_deadline) AS items \
           FROM appeals_queue aq \
           JOIN denials d ON d.id = aq.denial_id \
           JOIN claims c ON c.id = d.claim_id \
          WHERE aq.assigned_user_id IS NOT NULL \
            AND (aq.outcome_status IS NULL \
                 OR aq.outcome_status NOT IN ('approved','overruled','resolved','denied_again','cancelled')) \
            AND d.appeal_deadline IS NOT NULL \
            AND d.appeal_deadline <= CURRENT_DATE + INTERVAL '{horizon} days' \
          GROUP BY aq.assigned_user_id"
    ))
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;

    for row in &owners {
        let owner_id: Uuid = row.try_get("user_id").map_err(|e| AppError::Internal(e.to_string()))?;
        let overdue: i64 = row.try_get("overdue").unwrap_or(0);
        let upcoming: i64 = row.try_get("upcoming").unwrap_or(0);
        let soonest: Option<NaiveDate> = row.try_get("soonest").ok().flatten();
        let amount: f64 = row.try_get::<Option<f64>, _>("amount").ok().flatten().unwrap_or(0.0);
        let items: serde_json::Value = row
            .try_get::<Option<serde_json::Value>, _>("items")
            .ok()
            .flatten()
            .unwrap_or_else(|| serde_json::json!([]));

        let title = if overdue > 0 {
            format!("{overdue} overdue and {upcoming} due within {horizon} days")
        } else {
            format!("{upcoming} filing deadline(s) within {horizon} days")
        };
        let body = format!(
            "Soonest is {}. {} in denied charges is affected.",
            soonest.map(|d| d.to_string()).unwrap_or_default(),
            money(amount),
        );
        let payload = serde_json::json!({
            "overdue": overdue,
            "upcoming": upcoming,
            "items": items,
        });

        let result = sqlx::query(
            "INSERT INTO notifications (user_id, kind, title, body, payload) \
             VALUES ($1, 'deadline_digest', $2, $3, $4::jsonb) \
             ON CONFLICT (user_id, kind, for_date) DO NOTHING",
        )
        .bind(owner_id)
        .bind(&title)
        .bind(&body)
        .bind(payload.to_string())
        .execute(&state.pool)
        .await
        .map_err(AppError::Db)?;
        created += result.rows_affected() as i64;
    }

    // ── unowned overdue work goes to managers ──
    let orphan = sqlx::query(
        "SELECT COUNT(*) AS n, SUM(d.charge_amount)::float8 AS amount, MIN(d.appeal_deadline) AS oldest \
           FROM denials d \
           LEFT JOIN appeals_queue aq ON aq.denial_id = d.id \
                AND (aq.outcome_status IS NULL \
                     OR aq.outcome_status NOT IN ('approved','overruled','resolved','denied_again','cancelled')) \
          WHERE d.appeal_deadline IS NOT NULL \
            AND d.appeal_deadline < CURRENT_DATE \
            AND d.status IN ('open', 'analyzed') \
            AND (aq.id IS NULL OR aq.assigned_user_id IS NULL)",
    )
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let orphan_n: i64 = orphan.try_get("n").unwrap_or(0);
    if orphan_n > 0 {
        let oldest: Option<NaiveDate> = orphan.try_get("oldest").ok().flatten();
        let amount: f64 = orphan.try_get::<Option<f64>, _>("amount").ok().flatten().unwrap_or(0.0);
        let managers = sqlx::query("SELECT id FROM users WHERE is_active AND role = ANY($1::text[])")
            .bind(MANAGER_UP.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .fetch_all(&state.pool)
            .await
            .map_err(AppError::Db)?;

        let title = format!("{orphan_n} overdue denial(s) with nobody assigned");
        let body = format!(
            "Oldest deadline {}. {} in denied charges is unowned.",
            oldest.map(|d| d.to_string()).unwrap_or_default(),
            money(amount),
        );
        let payload = serde_json::json!({
            "count": orphan_n,
            "oldest": oldest.map(|d| d.to_string()),
        });

        for m in &managers {
            let mid: Uuid = m.try_get("id").map_err(|e| AppError::Internal(e.to_string()))?;
            let result = sqlx::query(
                "INSERT INTO notifications (user_id, kind, title, body, payload) \
                 VALUES ($1, 'overdue_escalation', $2, $3, $4::jsonb) \
                 ON CONFLICT (user_id, kind, for_date) DO NOTHING",
            )
            .bind(mid)
            .bind(&title)
            .bind(&body)
            .bind(payload.to_string())
            .execute(&state.pool)
            .await
            .map_err(AppError::Db)?;
            escalated += result.rows_affected() as i64;
        }
    }

    denial_common::audit::record(
        &state.pool,
        "deadline_digests_generated",
        "system",
        None,
        None,
        &serde_json::json!({
            "digests": created,
            "escalations": escalated,
            "horizon_days": horizon,
        }),
        None,
        None,
    )
    .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "digests_created": created,
        "escalations_created": escalated,
        "horizon_days": horizon,
    })))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_notifications))
        .route("/read-all", post(mark_all_read))
        .route("/generate-digests", post(generate_digests))
        .route("/{notification_id}/read", post(mark_read))
}
