//! Appeals queue routes.
//!
//! The appeals_queue table backs two operational views, split by resolution_type:
//!   * Appeals  — work that formally challenges the payer's decision.
//!   * Worklist — everything else: corrected claims, clinical documentation,
//!     payer phone calls, write-offs.

use std::collections::HashMap;

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, NaiveDate, Utc};
use denial_auth::rbac::{Principal, PrincipalKind};
use denial_common::error::AppError;
use denial_domain::{ALL_RESOLUTION_TYPES, APPEAL_RESOLUTION_TYPES, TERMINAL_WORK_OUTCOMES};
use serde::Deserialize;
use sqlx::{Column, Row};
use uuid::Uuid;

use crate::routes::write_offs;
use crate::state::AppState;

const SPECIALIST: &str = "billing_specialist";
const SUCCESS_OUTCOMES: &[&str] = &["approved", "overruled", "resolved"];

// ── Models ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AppealCreate {
    pub denial_id: String,
    pub ai_analysis_id: Option<String>,
    pub resolution_type: String,
    pub assigned_user_id: Option<String>,
    pub notes: Option<String>,
}

#[derive(Deserialize)]
pub struct AppealUpdate {
    pub outcome_status: Option<String>,
    pub notes: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub payer_response: Option<NaiveDate>,
    pub payer_response_text: Option<String>,
    pub final_outcome: Option<String>,
}

#[derive(Deserialize)]
pub struct ListAppealsQuery {
    pub outcome_status: Option<String>,
    pub resolution_type: Option<String>,
    pub category: Option<String>,
    pub assigned_user_id: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    50
}

#[derive(Deserialize)]
pub struct BulkQueue {
    pub denial_ids: Vec<String>,
    pub resolution_type: String,
    pub assigned_user_id: Option<String>,
    pub notes: Option<String>,
}

#[derive(Deserialize)]
pub struct AppealAssign {
    pub assigned_user_id: Option<String>,
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn queue_owner(principal: &Principal) -> Option<String> {
    if principal.kind == PrincipalKind::User
        && matches!(
            principal.role.as_deref(),
            Some(SPECIALIST | "coding_specialist")
        )
    {
        principal.user_id.clone()
    } else {
        None
    }
}

fn organization_id(principal: &Principal) -> Result<Uuid, AppError> {
    principal
        .organization_id
        .as_deref()
        .ok_or(AppError::Forbidden)
        .and_then(|id| Uuid::parse_str(id).map_err(|_| AppError::Forbidden))
}

fn assert_may_touch(principal: &Principal, assigned_user_id: Option<Uuid>) -> Result<(), AppError> {
    if let Some(owner) = queue_owner(principal) {
        if let Some(assigned) = assigned_user_id {
            if assigned.to_string() != owner {
                return Err(AppError::Forbidden);
            }
        }
    }
    Ok(())
}

async fn resolve_assignee(
    pool: &sqlx::PgPool,
    organization_id: Uuid,
    requested: Option<String>,
) -> Result<Option<Uuid>, AppError> {
    let Some(requested) = requested.filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let user_id = Uuid::parse_str(&requested)
        .map_err(|_| AppError::BadRequest("Invalid assigned_user_id".into()))?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM users u JOIN organization_memberships om ON om.user_id = u.id \
         WHERE u.id = $1 AND om.organization_id = $2 AND u.is_active = TRUE)",
    )
    .bind(user_id)
    .bind(organization_id)
    .fetch_one(pool)
    .await
    .map_err(AppError::Db)?;
    if exists {
        Ok(Some(user_id))
    } else {
        Err(AppError::NotFound)
    }
}

fn row_to_json(row: &sqlx::postgres::PgRow) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for col in row.columns().iter() {
        let name = col.name();
        let val = row
            .try_get::<Option<String>, _>(name)
            .map(|v| {
                v.map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null)
            })
            .or_else(|_| {
                row.try_get::<Option<i64>, _>(name).map(|v| {
                    v.map(serde_json::Value::from)
                        .unwrap_or(serde_json::Value::Null)
                })
            })
            .or_else(|_| {
                row.try_get::<Option<f64>, _>(name).map(|v| {
                    v.map(serde_json::Value::from)
                        .unwrap_or(serde_json::Value::Null)
                })
            })
            .or_else(|_| {
                row.try_get::<Option<bool>, _>(name).map(|v| {
                    v.map(serde_json::Value::Bool)
                        .unwrap_or(serde_json::Value::Null)
                })
            })
            .or_else(|_| {
                row.try_get::<Option<Uuid>, _>(name).map(|v| {
                    v.map(|v| serde_json::Value::String(v.to_string()))
                        .unwrap_or(serde_json::Value::Null)
                })
            })
            .or_else(|_| {
                row.try_get::<Option<NaiveDate>, _>(name).map(|v| {
                    v.map(|v| serde_json::Value::String(v.to_string()))
                        .unwrap_or(serde_json::Value::Null)
                })
            })
            .or_else(|_| {
                row.try_get::<Option<DateTime<Utc>>, _>(name).map(|v| {
                    v.map(|v| serde_json::Value::String(v.to_rfc3339()))
                        .unwrap_or(serde_json::Value::Null)
                })
            })
            .unwrap_or(serde_json::Value::Null);
        map.insert(name.to_string(), val);
    }
    serde_json::Value::Object(map)
}

fn parse_json_field(val: &mut serde_json::Value) {
    if let serde_json::Value::String(s) = val {
        if let Ok(parsed) = serde_json::from_str(s) {
            *val = parsed;
        }
    }
}

/// Denial statuses that leave nothing open on the claim; matches the claims
/// list's open-denial count.
const CLOSED_DENIAL_STATUSES: &[&str] = &["appealed", "overruled", "resolved", "written_off"];

pub(crate) async fn refresh_claim_status(
    pool: &sqlx::PgPool,
    claim_id: &Uuid,
) -> Result<(), AppError> {
    let _ = sqlx::query(
        "UPDATE claims c SET status = CASE \
         WHEN s.open_denials = 0 THEN 'resolved' \
         WHEN c.total_paid > 0 THEN 'partially_paid' \
         ELSE 'denied' END, \
         updated_at = NOW() \
         FROM (SELECT COUNT(*) AS total, \
              COUNT(*) FILTER (WHERE d.status <> ALL($2::text[])) AS open_denials \
              FROM denials d WHERE d.claim_id = $1) s \
         WHERE c.id = $1 AND s.total > 0 RETURNING c.status",
    )
    .bind(claim_id)
    .bind(CLOSED_DENIAL_STATUSES)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)?;
    Ok(())
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    if let sqlx::Error::Database(db_err) = e {
        db_err.code().is_some_and(|c| c == "23505")
    } else {
        false
    }
}

fn update_appeal_sql(mut sets: Vec<String>) -> String {
    // `updated_at` is an expression, not a bound parameter. Compute the ID
    // placeholder before appending it so the bind count and SQL stay aligned.
    let id_idx = sets.len() + 1;
    let org_idx = sets.len() + 2;
    sets.push("updated_at = NOW()".to_string());
    format!(
        "UPDATE appeals_queue AS aq SET {sets} \
         WHERE aq.id = ${id_idx} \
           AND EXISTS (SELECT 1 FROM denials d JOIN claims c ON c.id = d.claim_id \
                       WHERE d.id = aq.denial_id AND c.organization_id = ${org_idx}) \
         RETURNING aq.*",
        sets = sets.join(", ")
    )
}

pub(crate) async fn record_audit(
    pool: &sqlx::PgPool,
    principal: &Principal,
    action: &str,
    resource_type: &str,
    resource_id: Option<&str>,
    details: &serde_json::Value,
) {
    // users.rs runs this exact statement with a Uuid resource id. Postgres
    // prepared statements are cached per connection by SQL text, so binding a
    // string here failed ("incorrect binary data format") whenever the other
    // caller had prepared it first, and the audit entry was lost.
    let resource_id: Option<Uuid> = resource_id.and_then(|id| Uuid::parse_str(id).ok());
    let result = sqlx::query(
        "INSERT INTO audit_log (organization_id, user_id, action, resource_type, resource_id, details, ip_address) \
         VALUES ($1::uuid, $2::uuid, $3, $4, $5::uuid, $6::jsonb, $7::inet)",
    )
    .bind(principal.organization_id.as_deref())
    .bind(principal.user_id.as_deref())
    .bind(action)
    .bind(resource_type)
    .bind(resource_id)
    .bind(details)
    .bind(principal.ip.as_deref())
    .execute(pool)
    .await;
    if let Err(e) = result {
        tracing::warn!("audit write failed for {action}: {e}");
    }
}

// ── Handlers ────────────────────────────────────────────────────────────

pub async fn list_appeals(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<ListAppealsQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let organization_id = organization_id(&principal)?;
    if let Some(ref cat) = params.category {
        if cat != "appeal" && cat != "worklist" {
            return Err(AppError::BadRequest(
                "category must be 'appeal' or 'worklist'".into(),
            ));
        }
    }

    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new(
        "SELECT aq.*, d.cpt_code, d.carc_code, d.charge_amount, \
         c.claim_number, c.patient_name, c.payer_name, \
         aa.needs_appeal, \
         assignee.username AS assigned_username \
         FROM appeals_queue aq \
         JOIN denials d ON d.id = aq.denial_id \
         JOIN claims c ON c.id = d.claim_id \
         LEFT JOIN ai_analyses aa ON aa.id = aq.ai_analysis_id \
         LEFT JOIN users assignee ON assignee.id = aq.assigned_user_id",
    );

    let mut need_where = false;
    qb.push(" WHERE c.organization_id = ");
    qb.push_bind(organization_id);
    let push_prefix = |qb: &mut sqlx::QueryBuilder<sqlx::Postgres>, need_where: &mut bool| {
        if *need_where {
            qb.push(" WHERE ");
            *need_where = false;
        } else {
            qb.push(" AND ");
        }
    };

    if let Some(ref s) = params.outcome_status {
        push_prefix(&mut qb, &mut need_where);
        qb.push("aq.outcome_status = ");
        qb.push_bind(s.clone());
    }
    if let Some(ref rt) = params.resolution_type {
        push_prefix(&mut qb, &mut need_where);
        qb.push("aq.resolution_type = ");
        qb.push_bind(rt.clone());
    }
    match params.category.as_deref() {
        Some("appeal") => {
            push_prefix(&mut qb, &mut need_where);
            qb.push("aq.resolution_type = ANY(");
            qb.push_bind(
                APPEAL_RESOLUTION_TYPES
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>(),
            );
            qb.push("::text[])");
        }
        Some("worklist") => {
            push_prefix(&mut qb, &mut need_where);
            qb.push("(aq.resolution_type IS NULL OR NOT (aq.resolution_type = ANY(");
            qb.push_bind(
                APPEAL_RESOLUTION_TYPES
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>(),
            );
            qb.push("::text[])))");
        }
        _ => {}
    }
    if let Some(ref uid) = params.assigned_user_id {
        push_prefix(&mut qb, &mut need_where);
        qb.push("aq.assigned_user_id = ");
        qb.push_bind(uid.clone());
    }

    if let Some(owner) = queue_owner(&principal) {
        push_prefix(&mut qb, &mut need_where);
        qb.push("(aq.assigned_user_id = ");
        qb.push_bind(owner);
        qb.push(" OR aq.assigned_user_id IS NULL)");
    }

    if params.outcome_status.is_none() {
        if need_where {
            qb.push(" WHERE ");
        } else {
            qb.push(" AND (");
        }
        qb.push(
            "aq.outcome_status IS NULL OR aq.outcome_status NOT IN \
             ('approved', 'overruled', 'resolved', 'denied_again', 'cancelled')",
        );
        if !need_where {
            qb.push(")");
        }
    }

    let limit = params.limit.clamp(1, 500);
    qb.push(" ORDER BY aq.created_at ASC LIMIT ");
    qb.push_bind(limit);
    qb.push(" OFFSET ");
    qb.push_bind(params.offset.max(0));

    let rows = qb
        .build()
        .fetch_all(&state.pool)
        .await
        .map_err(AppError::Db)?;
    let values: Vec<serde_json::Value> = rows.iter().map(row_to_json).collect();
    Ok(Json(values))
}

pub async fn create_appeal(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(body): Json<AppealCreate>,
) -> Result<(axum::http::StatusCode, Json<serde_json::Value>), AppError> {
    let organization_id = organization_id(&principal)?;
    if !ALL_RESOLUTION_TYPES.contains(&body.resolution_type.as_str()) {
        return Err(AppError::BadRequest("Invalid resolution_type".into()));
    }

    let pool = &state.pool;
    let denial_id: Uuid = body
        .denial_id
        .parse()
        .map_err(|_| AppError::BadRequest("Invalid denial_id".into()))?;

    let denial_row = sqlx::query(
        "SELECT d.claim_id FROM denials d JOIN claims c ON c.id = d.claim_id \
         WHERE d.id = $1 AND c.organization_id = $2",
    )
    .bind(denial_id)
    .bind(organization_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;
    let claim_id: Uuid = denial_row.get("claim_id");

    // One open item per denial. This check is the fast path for a clear 409;
    // the partial unique index is the guarantee (handles the race).
    // NOTE: does NOT include 'denied_again' — allows re-appealing a denied_again item.
    let existing = sqlx::query(
        "SELECT id FROM appeals_queue \
         WHERE denial_id = $1 \
           AND (outcome_status IS NULL \
                OR outcome_status NOT IN ('approved', 'overruled', 'resolved', 'cancelled'))",
    )
    .bind(denial_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)?;
    if let Some(row) = existing {
        let id: Uuid = row.get("id");
        return Err(AppError::Conflict(format!(
            "An open appeal already exists for this denial ({id})"
        )));
    }

    let analysis_id: Option<Uuid> = if let Some(ref aid) = body.ai_analysis_id {
        Some(
            aid.parse()
                .map_err(|_| AppError::BadRequest("Invalid ai_analysis_id".into()))?,
        )
    } else {
        let row = sqlx::query(
            "SELECT aa.id FROM ai_analyses aa JOIN claims c ON c.id = aa.claim_id \
             WHERE aa.denial_id = $1 AND c.organization_id = $2 ORDER BY aa.created_at DESC LIMIT 1",
        )
        .bind(denial_id)
        .bind(organization_id)
        .fetch_optional(pool)
        .await
        .map_err(AppError::Db)?;
        row.and_then(|r| r.try_get::<Uuid, _>("id").ok())
    };

    let assigned_to = resolve_assignee(
        pool,
        organization_id,
        body.assigned_user_id.or_else(|| queue_owner(&principal)),
    )
    .await?;

    let row = sqlx::query(
        "INSERT INTO appeals_queue \
         (denial_id, claim_id, ai_analysis_id, resolution_type, assigned_user_id, notes, outcome_status) \
         VALUES ($1, $2, $3, $4, $5::uuid, $6, 'queued') RETURNING *",
    )
    .bind(denial_id)
    .bind(claim_id)
    .bind(analysis_id)
    .bind(&body.resolution_type)
    .bind(assigned_to)
    .bind(body.notes.as_deref())
    .fetch_one(pool)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            AppError::Conflict("An open appeal already exists for this denial".into())
        } else {
            AppError::Db(e)
        }
    })?;

    let denial_status = if APPEAL_RESOLUTION_TYPES.contains(&body.resolution_type.as_str()) {
        "in_appeal"
    } else {
        "in_progress"
    };
    sqlx::query("UPDATE denials SET status = $2, updated_at = NOW() WHERE id = $1")
        .bind(denial_id)
        .bind(denial_status)
        .execute(pool)
        .await
        .map_err(AppError::Db)?;

    refresh_claim_status(pool, &claim_id).await?;

    Ok((axum::http::StatusCode::CREATED, Json(row_to_json(&row))))
}

pub async fn update_appeal(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(appeal_id): Path<Uuid>,
    Json(body): Json<AppealUpdate>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    let pool = &state.pool;
    let organization_id = organization_id(&principal)?;

    let old_row = sqlx::query(
        "SELECT aq.outcome_status, aq.assigned_user_id, aq.resolution_type, aq.denial_id \
         FROM appeals_queue aq \
         JOIN denials d ON d.id = aq.denial_id JOIN claims c ON c.id = d.claim_id \
         WHERE aq.id = $1 AND c.organization_id = $2",
    )
    .bind(appeal_id)
    .bind(organization_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)?;

    if old_row.is_none() {
        return Err(AppError::NotFound);
    }

    let old_outcome: Option<String> = old_row
        .as_ref()
        .and_then(|r| r.try_get("outcome_status").ok().flatten());

    let old_assigned: Option<Uuid> = old_row
        .as_ref()
        .and_then(|r| r.try_get("assigned_user_id").ok().flatten());

    assert_may_touch(&principal, old_assigned)?;

    // Closing a write-off item successfully writes the denial off, which can
    // need a second person's approval first; nothing is changed until then.
    let old_resolution_type: Option<String> = old_row
        .as_ref()
        .and_then(|r| r.try_get("resolution_type").ok().flatten());
    let completes_write_off = old_resolution_type.as_deref() == Some("write_off")
        && body.outcome_status != old_outcome
        && body
            .outcome_status
            .as_deref()
            .is_some_and(|o| SUCCESS_OUTCOMES.contains(&o));
    if completes_write_off {
        let denial_id: Uuid = old_row
            .as_ref()
            .and_then(|r| r.try_get("denial_id").ok())
            .ok_or(AppError::NotFound)?;
        if let write_offs::Gate::Pending {
            request_id,
            amount,
            threshold,
        } = write_offs::gate(
            pool,
            &principal,
            denial_id,
            Some(appeal_id),
            body.notes.as_deref(),
        )
        .await?
        {
            return Ok((
                StatusCode::ACCEPTED,
                Json(write_offs::Gate::pending_response(
                    request_id, amount, threshold,
                )),
            ));
        }
    }

    let mut sets: Vec<String> = Vec::new();

    if body.outcome_status.is_some() {
        sets.push(format!("outcome_status = ${}", sets.len() + 1));
    }
    if body.notes.is_some() {
        sets.push(format!("notes = ${}", sets.len() + 1));
    }
    if body.submitted_at.is_some() {
        sets.push(format!("submitted_at = ${}", sets.len() + 1));
    }
    if body.payer_response.is_some() {
        sets.push(format!("payer_response = ${}", sets.len() + 1));
    }
    if body.payer_response_text.is_some() {
        sets.push(format!("payer_response_text = ${}", sets.len() + 1));
    }
    if body.final_outcome.is_some() {
        sets.push(format!("final_outcome = ${}", sets.len() + 1));
    }

    if sets.is_empty() {
        return Err(AppError::BadRequest("No fields to update".into()));
    }

    let sql = update_appeal_sql(sets);

    let mut q = sqlx::query(&sql);
    if let Some(ref v) = body.outcome_status {
        q = q.bind(v);
    }
    if let Some(ref v) = body.notes {
        q = q.bind(v);
    }
    if let Some(v) = body.submitted_at {
        q = q.bind(v);
    }
    if let Some(v) = body.payer_response {
        q = q.bind(v);
    }
    if let Some(ref v) = body.payer_response_text {
        q = q.bind(v);
    }
    if let Some(ref v) = body.final_outcome {
        q = q.bind(v);
    }
    q = q.bind(appeal_id);
    q = q.bind(organization_id);

    let row = q
        .fetch_optional(pool)
        .await
        .map_err(AppError::Db)?
        .ok_or(AppError::NotFound)?;

    let new_outcome: Option<String> = row.try_get("outcome_status").unwrap_or(None);
    let resolution_type: Option<String> = row.try_get("resolution_type").unwrap_or(None);
    let denial_id: Uuid = row
        .try_get("denial_id")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let claim_id: Uuid = row
        .try_get("claim_id")
        .map_err(|e| AppError::Internal(e.to_string()))?;

    // Audit: record what changed.
    let claim_number: Option<String> = sqlx::query("SELECT claim_number FROM claims WHERE id = $1")
        .bind(claim_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .and_then(|r| r.try_get("claim_number").ok());

    let details = serde_json::json!({
        "username": principal.username,
        "old_outcome": old_outcome,
        "outcome_status": new_outcome,
        "resolution_type": resolution_type,
        "claim_number": claim_number,
    });
    record_audit(
        pool,
        &principal,
        "appeal_updated",
        "appeal",
        Some(&appeal_id.to_string()),
        &details,
    )
    .await;

    // Close the denial out based on the work that was actually done.
    if old_outcome != new_outcome {
        if let Some(ref outcome) = new_outcome {
            if TERMINAL_WORK_OUTCOMES.contains(&outcome.as_str()) {
                let denial_status = if !SUCCESS_OUTCOMES.contains(&outcome.as_str()) {
                    "analyzed"
                } else if resolution_type.as_deref() == Some("write_off") {
                    "written_off"
                } else if resolution_type
                    .as_deref()
                    .map(|rt| APPEAL_RESOLUTION_TYPES.contains(&rt))
                    .unwrap_or(false)
                {
                    "appealed"
                } else {
                    "resolved"
                };

                sqlx::query("UPDATE denials SET status = $2, updated_at = NOW() WHERE id = $1")
                    .bind(denial_id)
                    .bind(denial_status)
                    .execute(pool)
                    .await
                    .map_err(AppError::Db)?;

                refresh_claim_status(pool, &claim_id).await?;
            }
        }
    }

    Ok((StatusCode::OK, Json(row_to_json(&row))))
}

pub async fn get_appeal(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(appeal_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let pool = &state.pool;
    let organization_id = organization_id(&principal)?;

    let row = sqlx::query(
        "SELECT aq.*, \
         assignee.username AS assigned_username, \
         d.cpt_code, d.carc_code, d.rarc_code, d.cagc, \
         d.charge_amount, d.adjustment_reason, d.appeal_deadline, \
         d.status AS denial_status, \
         c.claim_number, c.patient_name, c.patient_id, c.date_of_birth, \
         c.payer_name, c.payer_id_number, c.icd_10_codes, \
         c.service_from, c.service_to, \
         cc.description AS carc_description, \
         aa.explanation, aa.denial_category, aa.required_action, \
         aa.root_cause_summary, aa.action_plan, aa.steps, \
         aa.needs_appeal, aa.draft_appeal_letter, aa.confidence_score \
         FROM appeals_queue aq \
         JOIN denials d ON d.id = aq.denial_id \
         JOIN claims c ON c.id = d.claim_id \
         LEFT JOIN carc_codes cc ON cc.code = d.carc_code \
         LEFT JOIN ai_analyses aa ON aa.id = aq.ai_analysis_id \
         LEFT JOIN users assignee ON assignee.id = aq.assigned_user_id \
         WHERE aq.id = $1 AND c.organization_id = $2",
    )
    .bind(appeal_id)
    .bind(organization_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;

    let assigned: Option<Uuid> = row.try_get("assigned_user_id").unwrap_or(None);
    assert_may_touch(&principal, assigned)?;

    let mut item = row_to_json(&row);
    if let Some(obj) = item.as_object_mut() {
        if let Some(steps) = obj.get_mut("steps") {
            parse_json_field(steps);
        }
        if let Some(plan) = obj.get_mut("action_plan") {
            parse_json_field(plan);
        }
        let is_appeal = obj
            .get("resolution_type")
            .and_then(|v| v.as_str())
            .map(|rt| APPEAL_RESOLUTION_TYPES.contains(&rt))
            .unwrap_or(false);
        obj.insert("is_appeal".into(), serde_json::Value::Bool(is_appeal));
    }

    Ok(Json(item))
}

pub async fn get_appeal_letter(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(appeal_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let pool = &state.pool;
    let organization_id = organization_id(&principal)?;

    let row = sqlx::query(
        "SELECT aq.assigned_user_id, \
         d.cpt_code, d.carc_code, d.charge_amount, \
         c.claim_number, c.patient_name, c.patient_id, \
         c.date_of_birth, c.payer_name, c.payer_id_number, \
         c.icd_10_codes, c.service_from, c.service_to, \
         aa.draft_appeal_letter, aa.explanation, aa.needs_appeal \
         FROM appeals_queue aq \
         JOIN denials d ON d.id = aq.denial_id \
         JOIN claims c ON c.id = d.claim_id \
         LEFT JOIN ai_analyses aa ON aa.id = aq.ai_analysis_id \
         WHERE aq.id = $1 AND c.organization_id = $2",
    )
    .bind(appeal_id)
    .bind(organization_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;

    let assigned: Option<Uuid> = row.try_get("assigned_user_id").unwrap_or(None);
    assert_may_touch(&principal, assigned)?;

    Ok(Json(row_to_json(&row)))
}

pub async fn bulk_queue(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(body): Json<BulkQueue>,
) -> Result<(axum::http::StatusCode, Json<serde_json::Value>), AppError> {
    let organization_id = organization_id(&principal)?;
    if body.denial_ids.is_empty() || body.denial_ids.len() > 500 {
        return Err(AppError::BadRequest(
            "denial_ids must contain between 1 and 500 items".into(),
        ));
    }
    if !ALL_RESOLUTION_TYPES.contains(&body.resolution_type.as_str()) {
        return Err(AppError::BadRequest("Invalid resolution_type".into()));
    }

    let pool = &state.pool;
    let assigned_to = resolve_assignee(
        pool,
        organization_id,
        body.assigned_user_id.or_else(|| queue_owner(&principal)),
    )
    .await?;

    let denial_status = if APPEAL_RESOLUTION_TYPES.contains(&body.resolution_type.as_str()) {
        "in_appeal"
    } else {
        "in_progress"
    };

    // Parse IDs, skipping invalid ones.
    let mut valid_ids: Vec<Uuid> = Vec::new();
    let mut skipped: Vec<serde_json::Value> = Vec::new();
    for id in &body.denial_ids {
        match id.parse::<Uuid>() {
            Ok(u) => valid_ids.push(u),
            Err(_) => skipped.push(serde_json::json!({
                "denial_id": id,
                "reason": "not found",
            })),
        }
    }

    // Batch lookup: resolve the whole cluster up front.
    let rows = sqlx::query(
        "SELECT d.id, d.claim_id, c.claim_number, \
         EXISTS (SELECT 1 FROM appeals_queue aq \
                 WHERE aq.denial_id = d.id \
                   AND (aq.outcome_status IS NULL \
                        OR aq.outcome_status <> ALL($2::text[]))) AS has_open, \
         (SELECT a.id FROM ai_analyses a \
           WHERE a.denial_id = d.id \
           ORDER BY a.created_at DESC LIMIT 1) AS analysis_id \
         FROM denials d JOIN claims c ON c.id = d.claim_id \
         WHERE d.id = ANY($1::uuid[]) AND c.organization_id = $3",
    )
    .bind(&valid_ids)
    .bind(
        TERMINAL_WORK_OUTCOMES
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
    )
    .bind(organization_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)?;

    #[derive(Debug)]
    struct DenialInfo {
        id: Uuid,
        claim_id: Uuid,
        claim_number: String,
        has_open: bool,
        analysis_id: Option<Uuid>,
    }

    let mut found: HashMap<String, DenialInfo> = HashMap::new();
    for row in &rows {
        let id: Uuid = row.get("id");
        let claim_id: Uuid = row.get("claim_id");
        let claim_number: String = row.get("claim_number");
        let has_open: bool = row.get("has_open");
        let analysis_id: Option<Uuid> = row.get("analysis_id");
        found.insert(
            id.to_string(),
            DenialInfo {
                id,
                claim_id,
                claim_number,
                has_open,
                analysis_id,
            },
        );
    }

    let mut queued: Vec<serde_json::Value> = Vec::new();

    for id_str in &body.denial_ids {
        let info = match found.get(id_str) {
            Some(i) => i,
            None => {
                skipped.push(serde_json::json!({
                    "denial_id": id_str,
                    "reason": "not found",
                }));
                continue;
            }
        };

        if info.has_open {
            skipped.push(serde_json::json!({
                "denial_id": id_str,
                "claim_number": info.claim_number,
                "reason": "already has open work",
            }));
            continue;
        }

        let row = sqlx::query(
            "INSERT INTO appeals_queue \
             (denial_id, claim_id, ai_analysis_id, resolution_type, assigned_user_id, notes, outcome_status) \
             VALUES ($1::uuid, $2, $3, $4, $5::uuid, $6, 'queued') RETURNING id",
        )
        .bind(info.id)
        .bind(info.claim_id)
        .bind(info.analysis_id)
        .bind(&body.resolution_type)
        .bind(assigned_to)
        .bind(body.notes.as_deref())
        .fetch_one(pool)
        .await;

        let row = match row {
            Ok(r) => r,
            Err(e) if is_unique_violation(&e) => {
                skipped.push(serde_json::json!({
                    "denial_id": id_str,
                    "claim_number": info.claim_number,
                    "reason": "already has open work",
                }));
                continue;
            }
            Err(e) => return Err(AppError::Db(e)),
        };

        let queue_id: Uuid = row.get("id");

        sqlx::query("UPDATE denials SET status = $2, updated_at = NOW() WHERE id = $1")
            .bind(info.id)
            .bind(denial_status)
            .execute(pool)
            .await
            .map_err(AppError::Db)?;

        refresh_claim_status(pool, &info.claim_id).await?;

        queued.push(serde_json::json!({
            "denial_id": id_str,
            "claim_number": info.claim_number,
            "queue_id": queue_id.to_string(),
        }));
    }

    let details = serde_json::json!({
        "username": principal.username,
        "resolution_type": body.resolution_type,
        "queued": queued.len(),
        "skipped": skipped.len(),
        "claim_numbers": queued.iter().filter_map(|q| q.get("claim_number").and_then(|v| v.as_str())).collect::<Vec<_>>(),
    });
    record_audit(pool, &principal, "bulk_queue", "appeal", None, &details).await;

    Ok((
        axum::http::StatusCode::CREATED,
        Json(serde_json::json!({
            "status": "queued",
            "resolution_type": body.resolution_type,
            "queued": queued.len(),
            "skipped": skipped.len(),
            "items": queued,
            "not_queued": skipped,
        })),
    ))
}

pub async fn assign_appeal(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(appeal_id): Path<Uuid>,
    Json(body): Json<AppealAssign>,
) -> Result<Json<serde_json::Value>, AppError> {
    let pool = &state.pool;
    let organization_id = organization_id(&principal)?;

    let current = sqlx::query(
        "SELECT aq.id, aq.assigned_user_id, aq.resolution_type, aq.denial_id, \
         u.username AS current_username, \
         c.claim_number \
         FROM appeals_queue aq \
         LEFT JOIN users u ON u.id = aq.assigned_user_id \
         JOIN denials d ON d.id = aq.denial_id \
         JOIN claims c ON c.id = d.claim_id \
         WHERE aq.id = $1 AND c.organization_id = $2",
    )
    .bind(appeal_id)
    .bind(organization_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;

    let resolution_type: Option<String> = current.try_get("resolution_type").unwrap_or(None);
    let claim_number: Option<String> = current.try_get("claim_number").unwrap_or(None);
    let current_username: Option<String> = current.try_get("current_username").unwrap_or(None);

    let mut assignee_username: Option<String> = None;
    let mut assignee_id: Option<Uuid> = None;

    if let Some(ref uid) = body.assigned_user_id {
        if !uid.is_empty() {
            let assignee = sqlx::query(
                "SELECT u.id, u.username, u.role, u.is_active FROM users u \
                     JOIN organization_memberships om ON om.user_id = u.id \
                     WHERE u.id = $1::uuid AND om.organization_id = $2",
            )
            .bind(uid)
            .bind(organization_id)
            .fetch_optional(pool)
            .await
            .map_err(AppError::Db)?
            .ok_or(AppError::NotFound)?;

            let is_active: bool = assignee.get("is_active");
            let username: String = assignee.get("username");

            if !is_active {
                return Err(AppError::BadRequest(format!(
                    "{username} is deactivated and cannot be assigned work"
                )));
            }

            assignee_id = Some(
                Uuid::parse_str(uid)
                    .map_err(|_| AppError::BadRequest("Invalid assigned_user_id".into()))?,
            );
            assignee_username = Some(username);
        }
    }

    let row = sqlx::query(
        "UPDATE appeals_queue \
         SET assigned_user_id = $2::uuid, updated_at = NOW() \
         WHERE id = $1 RETURNING *",
    )
    .bind(appeal_id)
    .bind(assignee_id)
    .fetch_one(pool)
    .await
    .map_err(AppError::Db)?;

    let details = serde_json::json!({
        "username": principal.username,
        "from": current_username,
        "to": assignee_username,
        "resolution_type": resolution_type,
        "claim_number": claim_number,
    });
    record_audit(
        pool,
        &principal,
        "assign_appeal",
        "appeal",
        Some(&appeal_id.to_string()),
        &details,
    )
    .await;

    let mut result = row_to_json(&row);
    if let Some(obj) = result.as_object_mut() {
        obj.insert(
            "assigned_username".into(),
            serde_json::to_value(assignee_username).unwrap(),
        );
    }

    Ok(Json(result))
}

// ── Router ──────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_appeals).post(create_appeal))
        .route("/bulk", post(bulk_queue))
        .route("/{appeal_id}", get(get_appeal).patch(update_appeal))
        .route("/{appeal_id}/letter", get(get_appeal_letter))
        .route("/{appeal_id}/assign", post(assign_appeal))
}

#[cfg(test)]
mod tests {
    use super::update_appeal_sql;

    #[test]
    fn update_sql_uses_the_next_bound_parameter_for_the_id() {
        let sql = update_appeal_sql(vec!["outcome_status = $1".into()]);

        assert!(sql.contains("WHERE aq.id = $2"));
    }

    #[test]
    fn update_sql_counts_all_bound_fields_before_the_id() {
        let sql = update_appeal_sql(vec!["outcome_status = $1".into(), "notes = $2".into()]);

        assert!(sql.contains("WHERE aq.id = $3"));
    }
}
