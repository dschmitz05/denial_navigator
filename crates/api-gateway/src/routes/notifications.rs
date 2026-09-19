//! Deadline notifications.
//!
//! Ported from `api-gateway/routes/notifications.py`. Two kinds: a
//! `deadline_digest` to the person who owns the work, and an
//! `overdue_escalation` to managers for overdue work nobody owns. Generation
//! is idempotent by `(organization, user, kind, day)` - the unique index, not an assumption
//! about how often the cron calls this, is what makes "once a day" hold.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, NaiveDate, Utc};
use denial_auth::rbac::{Principal, PrincipalKind};
use denial_common::error::AppError;
use serde::Deserialize;
use sqlx::Row;
use uuid::Uuid;

use crate::state::AppState;

/// system/revenue-cycle administrators - the roles an unowned overdue
/// denial is escalated to.
const MANAGER_UP: &[&str] = &["revenue_cycle_manager", "system_admin"];

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

fn organization_id(principal: &Principal) -> Result<Uuid, AppError> {
    principal
        .organization_id
        .as_deref()
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(AppError::Forbidden)
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
    let organization_id = organization_id(&principal)?;
    let limit = params.limit.clamp(1, 200);

    let sql = format!(
        "SELECT id, kind, for_date, title, body, payload, read_at, created_at \
           FROM notifications \
          WHERE organization_id = $1 AND user_id = $2 {} \
           ORDER BY created_at DESC \
           LIMIT $3",
        if params.unread_only {
            "AND read_at IS NULL"
        } else {
            ""
        }
    );

    let rows = sqlx::query(&sql)
        .bind(organization_id)
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
    let organization_id = organization_id(&principal)?;
    let row = sqlx::query(
        "UPDATE notifications SET read_at = NOW() \
          WHERE id = $1 AND organization_id = $2 AND user_id = $3 AND read_at IS NULL \
        RETURNING id",
    )
    .bind(notification_id)
    .bind(organization_id)
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
    let organization_id = organization_id(&principal)?;
    let result = sqlx::query(
        "UPDATE notifications SET read_at = NOW() \
          WHERE organization_id = $1 AND user_id = $2 AND read_at IS NULL",
    )
    .bind(organization_id)
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
        || matches!(
            principal.role.as_deref(),
            Some("system_admin" | "security_admin")
        );
    if !allowed {
        return Err(AppError::Forbidden);
    }

    let horizon = state.digest_horizon_days as i64;
    let mut created: i64 = 0;
    let mut escalated: i64 = 0;

    // ── per-owner digests ──
    let owners = sqlx::query(&format!(
        "SELECT c.organization_id, aq.assigned_user_id AS user_id, \
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
           GROUP BY c.organization_id, aq.assigned_user_id"
    ))
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;

    for row in &owners {
        let org_id: Uuid = row
            .try_get("organization_id")
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let owner_id: Uuid = row
            .try_get("user_id")
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let overdue: i64 = row.try_get("overdue").unwrap_or(0);
        let upcoming: i64 = row.try_get("upcoming").unwrap_or(0);
        let soonest: Option<NaiveDate> = row.try_get("soonest").ok().flatten();
        let amount: f64 = row
            .try_get::<Option<f64>, _>("amount")
            .ok()
            .flatten()
            .unwrap_or(0.0);
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
            "INSERT INTO notifications (organization_id, user_id, kind, title, body, payload) \
             VALUES ($1, $2, 'deadline_digest', $3, $4, $5::jsonb) \
             ON CONFLICT (organization_id, user_id, kind, for_date) DO NOTHING",
        )
        .bind(org_id)
        .bind(owner_id)
        .bind(&title)
        .bind(&body)
        .bind(payload.to_string())
        .execute(&state.pool)
        .await
        .map_err(AppError::Db)?;
        created += result.rows_affected() as i64;
    }

    // ── unowned overdue work goes to that org's managers ──
    // Scoping the aggregate and the fan-out per organization keeps a manager's
    // escalation from mixing another tenant's overdue counts and amounts.
    let orgs = sqlx::query("SELECT id FROM organizations WHERE is_active")
        .fetch_all(&state.pool)
        .await
        .map_err(AppError::Db)?;

    for org in &orgs {
        let org_id: Uuid = org
            .try_get("id")
            .map_err(|e| AppError::Internal(e.to_string()))?;

        let orphan = sqlx::query(
            "SELECT COUNT(*) AS n, SUM(d.charge_amount)::float8 AS amount, MIN(d.appeal_deadline) AS oldest \
               FROM denials d \
               JOIN claims c ON c.id = d.claim_id \
               LEFT JOIN appeals_queue aq ON aq.denial_id = d.id \
                    AND (aq.outcome_status IS NULL \
                         OR aq.outcome_status NOT IN ('approved','overruled','resolved','denied_again','cancelled')) \
              WHERE c.organization_id = $1 \
                AND d.appeal_deadline IS NOT NULL \
                AND d.appeal_deadline < CURRENT_DATE \
                AND d.status IN ('open', 'analyzed') \
                AND (aq.id IS NULL OR aq.assigned_user_id IS NULL)",
        )
        .bind(org_id)
        .fetch_one(&state.pool)
        .await
        .map_err(AppError::Db)?;

        let orphan_n: i64 = orphan.try_get("n").unwrap_or(0);
        if orphan_n > 0 {
            let oldest: Option<NaiveDate> = orphan.try_get("oldest").ok().flatten();
            let amount: f64 = orphan
                .try_get::<Option<f64>, _>("amount")
                .ok()
                .flatten()
                .unwrap_or(0.0);
            let managers = sqlx::query(
                "SELECT om.user_id AS id \
                       FROM organization_memberships om \
                      WHERE om.organization_id = $1 \
                        AND om.role = ANY($2::text[]) \
                        AND om.user_id IN (SELECT id FROM users WHERE is_active)",
            )
            .bind(org_id)
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
                let mid: Uuid = m
                    .try_get("id")
                    .map_err(|e| AppError::Internal(e.to_string()))?;
                let result = sqlx::query(
                    "INSERT INTO notifications (organization_id, user_id, kind, title, body, payload) \
                     VALUES ($1, $2, 'overdue_escalation', $3, $4, $5::jsonb) \
                     ON CONFLICT (organization_id, user_id, kind, for_date) DO NOTHING",
                )
                .bind(org_id)
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

        // ── overpayments past their refund deadline (FB-08) ──
        let overdue_refunds = sqlx::query(
            "SELECT COUNT(*) AS n, SUM(amount)::float8 AS amount, MIN(due_date) AS oldest \
               FROM overpayments \
              WHERE organization_id = $1 AND status = 'identified' AND due_date < CURRENT_DATE",
        )
        .bind(org_id)
        .fetch_one(&state.pool)
        .await
        .map_err(AppError::Db)?;
        let refund_n: i64 = overdue_refunds.try_get("n").unwrap_or(0);
        if refund_n > 0 {
            let oldest: Option<NaiveDate> = overdue_refunds.try_get("oldest").ok().flatten();
            let amount: f64 = overdue_refunds
                .try_get::<Option<f64>, _>("amount")
                .ok()
                .flatten()
                .unwrap_or(0.0);
            let title = format!("{refund_n} overpayment(s) past their refund deadline");
            let body = format!(
                "{} identified but not refunded, recouped or disputed; oldest due {}.",
                money(amount),
                oldest.map(|d| d.to_string()).unwrap_or_default(),
            );
            let payload = serde_json::json!({
                "count": refund_n,
                "amount": amount,
                "oldest": oldest.map(|d| d.to_string()),
            });
            let result = sqlx::query(
                "INSERT INTO notifications (organization_id, user_id, kind, title, body, payload) \
                 SELECT $1, om.user_id, 'overpayment_due', $3, $4, $5::jsonb \
                   FROM organization_memberships om \
                  WHERE om.organization_id = $1 AND om.role = ANY($2::text[]) \
                    AND om.user_id IN (SELECT id FROM users WHERE is_active) \
                 ON CONFLICT (organization_id, user_id, kind, for_date) DO NOTHING",
            )
            .bind(org_id)
            .bind(MANAGER_UP.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .bind(&title)
            .bind(&body)
            .bind(payload.to_string())
            .execute(&state.pool)
            .await
            .map_err(AppError::Db)?;
            escalated += result.rows_affected() as i64;
        }
    }

    denial_audit::record(
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
        principal.organization_id.as_deref(),
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
