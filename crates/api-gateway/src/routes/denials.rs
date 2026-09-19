use axum::extract::{Query, State};
use axum::routing::get;
use axum::Router;
use axum::{Extension, Json};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, NaiveDate, Utc};
use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use denial_db::pgjson::row_to_json;
use denial_domain::ACTIVE_DENIAL_STATUSES;
use denial_engine::recommended_resolution;
use serde::Deserialize;
use sqlx::{QueryBuilder, Row};
use uuid::Uuid;

use crate::state::AppState;

#[derive(Deserialize)]
pub struct DenialUpdate {
    pub status: Option<String>,
    pub appeal_deadline: Option<NaiveDate>,
}

#[derive(Deserialize)]
pub struct ListDenialsQuery {
    pub status: Option<String>,
    pub carc_code: Option<String>,
    pub payer_name: Option<String>,
    pub min_amount: Option<f64>,
    pub max_amount: Option<f64>,
    pub min_age_days: Option<i32>,
    pub max_age_days: Option<i32>,
    pub owner: Option<String>,
    pub facility_type_code: Option<String>,
    pub cagc: Option<String>,
    pub claim_id: Option<String>,
    pub q: Option<String>,
    #[serde(default)]
    pub priority: bool,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    pub cursor: Option<String>,
    pub sort: Option<String>,
    #[serde(default)]
    pub descending: bool,
}

fn default_limit() -> i64 {
    50
}

fn organization_id(principal: &Principal) -> Result<Uuid, AppError> {
    principal
        .organization_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(AppError::Forbidden)
}

fn decode_cursor(cursor: &str) -> Result<i64, AppError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| AppError::BadRequest("invalid cursor".into()))?;
    let value =
        std::str::from_utf8(&bytes).map_err(|_| AppError::BadRequest("invalid cursor".into()))?;
    value
        .parse::<i64>()
        .map_err(|_| AppError::BadRequest("invalid cursor".into()))
}

#[derive(Deserialize)]
pub struct PayerWindow {
    pub payer_name: String,
    pub appeal_window_days: u32,
    pub notes: Option<String>,
}

pub async fn list_denials(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<ListDenialsQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    if let (Some(minimum), Some(maximum)) = (params.min_amount, params.max_amount) {
        if minimum > maximum {
            return Err(AppError::BadRequest(
                "min_amount cannot exceed max_amount".into(),
            ));
        }
    }
    let mut qb = QueryBuilder::<sqlx::Postgres>::new(
        "SELECT * FROM (SELECT DISTINCT ON (d.id) d.*, c.claim_number, c.patient_name, c.payer_name, c.facility_type_code, \
         c.total_charge, \
         (d.appeal_deadline - CURRENT_DATE) AS days_until_deadline, \
         (d.appeal_deadline IS NOT NULL AND d.appeal_deadline < CURRENT_DATE) AS deadline_passed, \
         cc.description as carc_description, \
         rc.description as rarc_description, \
         aa.explanation, aa.denial_category, aa.needs_appeal, \
         aq.outcome_status as appeal_status \
         FROM denials d \
         JOIN claims c ON c.id = d.claim_id \
         LEFT JOIN carc_codes cc ON cc.code = d.carc_code \
         LEFT JOIN rarc_codes rc ON rc.code = d.rarc_code \
         LEFT JOIN LATERAL (SELECT * FROM ai_analyses a WHERE a.denial_id = d.id ORDER BY a.created_at DESC LIMIT 1) aa ON TRUE \
         LEFT JOIN appeals_queue aq ON aq.denial_id = d.id \
             AND (aq.outcome_status IS NULL OR aq.outcome_status NOT IN ('approved','overruled','resolved','denied_again','cancelled'))",
    );

    let mut need_where = false;
    qb.push(" WHERE c.organization_id = ")
        .push_bind(organization_id);
    let mut push_prefix = |qb: &mut QueryBuilder<sqlx::Postgres>, need_where: &mut bool| {
        if *need_where {
            qb.push(" WHERE ");
            *need_where = false;
        } else {
            qb.push(" AND ");
        }
    };

    match &params.status {
        Some(s) => {
            push_prefix(&mut qb, &mut need_where);
            qb.push("d.status = ");
            qb.push_bind(s);
        }
        None => {
            push_prefix(&mut qb, &mut need_where);
            qb.push("d.status = ANY(");
            qb.push_bind(ACTIVE_DENIAL_STATUSES.to_vec());
            qb.push("::text[])");
        }
    }

    if let Some(ref carc) = params.carc_code {
        push_prefix(&mut qb, &mut need_where);
        qb.push("d.carc_code = ");
        qb.push_bind(carc);
    }
    if let Some(ref payer_name) = params.payer_name {
        push_prefix(&mut qb, &mut need_where);
        qb.push("c.payer_name ILIKE ");
        qb.push_bind(format!("%{}%", payer_name.trim()));
    }
    if let Some(min_amount) = params.min_amount {
        push_prefix(&mut qb, &mut need_where);
        qb.push("d.charge_amount >= ");
        qb.push_bind(min_amount);
    }
    if let Some(max_amount) = params.max_amount {
        push_prefix(&mut qb, &mut need_where);
        qb.push("d.charge_amount <= ");
        qb.push_bind(max_amount);
    }
    if let Some(minimum_age) = params.min_age_days {
        push_prefix(&mut qb, &mut need_where);
        qb.push("CURRENT_DATE - d.denial_date >= ");
        qb.push_bind(minimum_age.max(0));
    }
    if let Some(maximum_age) = params.max_age_days {
        push_prefix(&mut qb, &mut need_where);
        qb.push("CURRENT_DATE - d.denial_date <= ");
        qb.push_bind(maximum_age.max(0));
    }
    if let Some(owner) = params.owner.as_deref() {
        push_prefix(&mut qb, &mut need_where);
        if owner == "unassigned" {
            qb.push("aq.assigned_user_id IS NULL");
        } else {
            let owner = Uuid::parse_str(owner).map_err(|_| {
                AppError::BadRequest("owner must be a user UUID or unassigned".into())
            })?;
            qb.push("aq.assigned_user_id = ");
            qb.push_bind(owner);
        }
    }
    if let Some(facility_type_code) = params.facility_type_code.as_deref() {
        push_prefix(&mut qb, &mut need_where);
        qb.push("c.facility_type_code = ");
        qb.push_bind(facility_type_code);
    }
    if let Some(ref cagc) = params.cagc {
        push_prefix(&mut qb, &mut need_where);
        qb.push("d.cagc = ");
        qb.push_bind(cagc);
    }
    if let Some(ref claim_id) = params.claim_id {
        push_prefix(&mut qb, &mut need_where);
        qb.push("d.claim_id = (SELECT id FROM claims WHERE claim_number = ");
        qb.push_bind(claim_id);
        qb.push(")");
    }
    if let Some(ref search) = params.q {
        let search = search.trim();
        if !search.is_empty() {
            push_prefix(&mut qb, &mut need_where);
            qb.push("(c.claim_number ILIKE ");
            let pattern = format!("%{}%", search);
            qb.push_bind(pattern.clone());
            qb.push(" OR c.patient_name ILIKE ");
            qb.push_bind(pattern.clone());
            qb.push(" OR d.cpt_code ILIKE ");
            qb.push_bind(pattern.clone());
            qb.push(" OR d.carc_code ILIKE ");
            qb.push_bind(pattern);
            qb.push(")");
        }
    }
    if params.priority {
        push_prefix(&mut qb, &mut need_where);
        qb.push(
            "d.appeal_deadline IS NOT NULL AND d.appeal_deadline <= CURRENT_DATE + INTERVAL '14 days'",
        );
    }

    qb.push(" ORDER BY d.id) sub ");
    let sort_column = match params.sort.as_deref() {
        Some("deadline") => "sub.appeal_deadline",
        Some("created") => "sub.created_at",
        Some("amount") | None => "sub.charge_amount",
        Some(_) => {
            return Err(AppError::BadRequest(
                "sort must be amount, deadline, or created".into(),
            ))
        }
    };
    if params.priority {
        qb.push("ORDER BY sub.appeal_deadline ASC NULLS LAST, sub.charge_amount DESC");
    } else {
        qb.push("ORDER BY ");
        qb.push(sort_column);
        qb.push(if params.descending || params.sort.is_none() {
            " DESC"
        } else {
            " ASC"
        });
        qb.push(" NULLS LAST, sub.id ASC");
    }

    let limit = params.limit.clamp(1, 500);
    let offset = match params.cursor.as_deref() {
        Some(cursor) => decode_cursor(cursor)?.max(0),
        None => params.offset.max(0),
    };
    qb.push(" LIMIT ");
    qb.push_bind(limit + 1);
    qb.push(" OFFSET ");
    qb.push_bind(offset);

    let rows = qb
        .build()
        .fetch_all(&state.pool)
        .await
        .map_err(AppError::Db)?;
    let has_next = rows.len() > limit as usize;
    let values: Vec<serde_json::Value> = rows
        .into_iter()
        .take(limit as usize)
        .map(|row| row_to_json(&row))
        .collect();
    let next_cursor = has_next.then(|| URL_SAFE_NO_PAD.encode((offset + limit).to_string()));
    Ok(Json(
        serde_json::json!({"items": values, "next_cursor": next_cursor}),
    ))
}

pub async fn denials_by_carc(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let organization_id = organization_id(&principal)?;
    let rows = sqlx::query(
        "SELECT d.carc_code, COALESCE(cc.description, 'Unknown') as carc_description, \
         d.cagc, COUNT(*) as denial_count, SUM(d.charge_amount) as total_denied_amount, \
         AVG(d.charge_amount) as avg_denial_amount \
         FROM denials d JOIN claims c ON c.id = d.claim_id \
         LEFT JOIN carc_codes cc ON cc.code = d.carc_code \
         WHERE c.organization_id = $1 AND d.status IN ('open', 'analyzed') \
         GROUP BY d.carc_code, cc.description, d.cagc \
         ORDER BY total_denied_amount DESC",
    )
    .bind(organization_id)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;

    Ok(Json(rows.iter().map(row_to_json).collect()))
}

/// Open denial exposure by payer for the active organization.
pub async fn denials_by_payer(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let organization_id = organization_id(&principal)?;
    let rows = sqlx::query(
        "SELECT COALESCE(NULLIF(btrim(c.payer_name), ''), 'Unknown') AS payer_name, \
                COUNT(*) AS denial_count, \
                COALESCE(SUM(d.charge_amount), 0)::float8 AS total_denied_amount, \
                COALESCE(AVG(d.charge_amount), 0)::float8 AS avg_denial_amount, \
                COUNT(*) FILTER (WHERE d.appeal_deadline < CURRENT_DATE) AS overdue_count, \
                MIN(d.appeal_deadline) AS nearest_appeal_deadline \
         FROM denials d JOIN claims c ON c.id = d.claim_id \
         WHERE c.organization_id = $1 \
           AND d.status = ANY(ARRAY['open','analyzed','in_progress','in_appeal']::text[]) \
         GROUP BY COALESCE(NULLIF(btrim(c.payer_name), ''), 'Unknown') \
         ORDER BY total_denied_amount DESC, denial_count DESC",
    )
    .bind(organization_id)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    Ok(Json(rows.iter().map(row_to_json).collect()))
}

/// Open denial exposure by the latest AI-derived root cause.
pub async fn denials_by_root_cause(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let organization_id = organization_id(&principal)?;
    let rows = sqlx::query(
        "SELECT COALESCE(NULLIF(btrim(aa.root_cause_summary), ''), \
                         NULLIF(btrim(aa.denial_category), ''), \
                         'Unclassified') AS root_cause, \
                COUNT(*) AS denial_count, \
                COALESCE(SUM(d.charge_amount), 0)::float8 AS total_denied_amount, \
                COALESCE(AVG(d.charge_amount), 0)::float8 AS avg_denial_amount \
         FROM denials d JOIN claims c ON c.id = d.claim_id \
         LEFT JOIN LATERAL ( \
             SELECT root_cause_summary, denial_category \
             FROM ai_analyses \
             WHERE denial_id = d.id \
             ORDER BY created_at DESC LIMIT 1 \
         ) aa ON TRUE \
         WHERE c.organization_id = $1 \
           AND d.status = ANY(ARRAY['open','analyzed','in_progress','in_appeal']::text[]) \
         GROUP BY COALESCE(NULLIF(btrim(aa.root_cause_summary), ''), \
                           NULLIF(btrim(aa.denial_category), ''), 'Unclassified') \
         ORDER BY total_denied_amount DESC, denial_count DESC",
    )
    .bind(organization_id)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    Ok(Json(rows.iter().map(row_to_json).collect()))
}

/// Active denial exposure grouped by the age of the payer decision.
pub async fn denial_aging_buckets(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let organization_id = organization_id(&principal)?;
    let rows = sqlx::query(
        "WITH active AS ( \
             SELECT d.*, CASE \
                 WHEN d.denial_date IS NULL THEN 'Unknown' \
                 WHEN CURRENT_DATE - d.denial_date <= 30 THEN '0–30 days' \
                 WHEN CURRENT_DATE - d.denial_date <= 60 THEN '31–60 days' \
                 WHEN CURRENT_DATE - d.denial_date <= 90 THEN '61–90 days' \
                 ELSE '91+ days' END AS bucket, \
                 CASE \
                 WHEN d.denial_date IS NULL THEN 5 \
                 WHEN CURRENT_DATE - d.denial_date <= 30 THEN 1 \
                 WHEN CURRENT_DATE - d.denial_date <= 60 THEN 2 \
                 WHEN CURRENT_DATE - d.denial_date <= 90 THEN 3 \
                 ELSE 4 END AS bucket_order \
             FROM denials d JOIN claims c ON c.id = d.claim_id \
             WHERE c.organization_id = $1 \
               AND d.status = ANY(ARRAY['open','analyzed','in_progress','in_appeal']::text[]) \
         ) \
         SELECT bucket, bucket_order, COUNT(*) AS denial_count, \
                COALESCE(SUM(charge_amount), 0)::float8 AS total_denied_amount, \
                COALESCE(AVG(charge_amount), 0)::float8 AS avg_denial_amount \
         FROM active GROUP BY bucket, bucket_order ORDER BY bucket_order",
    )
    .bind(organization_id)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    Ok(Json(rows.iter().map(row_to_json).collect()))
}

/// Financial recovery summary using the latest recorded resubmission outcome
/// for each denial. A missing outcome remains explicitly unresolved.
pub async fn denial_financial_summary(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<serde_json::Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let row = sqlx::query(
        "WITH tenant_denials AS ( \
             SELECT d.id, d.charge_amount \
             FROM denials d JOIN claims c ON c.id = d.claim_id \
             WHERE c.organization_id = $1 \
         ), latest_outcome AS ( \
             SELECT DISTINCT ON (aa.denial_id) aa.denial_id, fl.was_paid_on_resubmit \
             FROM feedback_loop fl \
             JOIN ai_analyses aa ON aa.id = fl.ai_analysis_id \
             JOIN tenant_denials td ON td.id = aa.denial_id \
             WHERE fl.was_paid_on_resubmit IS NOT NULL \
             ORDER BY aa.denial_id, fl.created_at DESC \
         ) \
         SELECT COUNT(*) AS total_denials, \
                COALESCE(SUM(td.charge_amount), 0)::float8 AS denied_dollars, \
                COALESCE(SUM(td.charge_amount) FILTER (WHERE lo.was_paid_on_resubmit), 0)::float8 AS recovered_dollars, \
                COALESCE(SUM(td.charge_amount) FILTER (WHERE lo.was_paid_on_resubmit IS FALSE), 0)::float8 AS not_recovered_dollars, \
                COALESCE(SUM(td.charge_amount) FILTER (WHERE lo.was_paid_on_resubmit IS NULL), 0)::float8 AS unresolved_dollars, \
                COUNT(*) FILTER (WHERE lo.was_paid_on_resubmit IS NOT NULL) AS outcome_known_count, \
                COUNT(*) FILTER (WHERE lo.was_paid_on_resubmit) AS recovered_count \
         FROM tenant_denials td LEFT JOIN latest_outcome lo ON lo.denial_id = td.id",
    )
    .bind(organization_id)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;
    let known: i64 = row.try_get("outcome_known_count").unwrap_or(0);
    let recovered: i64 = row.try_get("recovered_count").unwrap_or(0);
    Ok(Json(serde_json::json!({
        "total_denials": row.try_get::<i64, _>("total_denials").unwrap_or(0),
        "denied_dollars": row.try_get::<f64, _>("denied_dollars").unwrap_or(0.0),
        "recovered_dollars": row.try_get::<f64, _>("recovered_dollars").unwrap_or(0.0),
        "not_recovered_dollars": row.try_get::<f64, _>("not_recovered_dollars").unwrap_or(0.0),
        "unresolved_dollars": row.try_get::<f64, _>("unresolved_dollars").unwrap_or(0.0),
        "outcome_known_count": known,
        "recovered_count": recovered,
        "recovery_rate": if known == 0 { serde_json::Value::Null } else { serde_json::json!(recovered as f64 / known as f64) },
    })))
}

/// Resolution timing for terminal appeal/worklist outcomes.
pub async fn denial_resolution_timing(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<serde_json::Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let row = sqlx::query(
        "WITH resolved AS ( \
             SELECT DISTINCT ON (d.id) d.id, d.denial_date, aq.updated_at AS resolved_at \
             FROM denials d \
             JOIN claims c ON c.id = d.claim_id \
             JOIN appeals_queue aq ON aq.denial_id = d.id \
             WHERE c.organization_id = $1 \
               AND d.denial_date IS NOT NULL \
               AND aq.outcome_status = ANY(ARRAY['approved','overruled','resolved','denied_again','cancelled']::text[]) \
             ORDER BY d.id, aq.updated_at DESC \
         ), intervals AS ( \
             SELECT GREATEST(0, EXTRACT(EPOCH FROM (resolved_at - denial_date)) / 86400.0) AS resolution_days \
             FROM resolved \
         ) \
         SELECT COUNT(*) AS resolved_count, \
                AVG(resolution_days)::float8 AS average_resolution_days, \
                percentile_cont(0.5) WITHIN GROUP (ORDER BY resolution_days)::float8 AS median_resolution_days \
         FROM intervals",
    )
    .bind(organization_id)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;
    Ok(Json(serde_json::json!({
        "resolved_count": row.try_get::<i64, _>("resolved_count").unwrap_or(0),
        "average_resolution_days": row.try_get::<Option<f64>, _>("average_resolution_days").ok().flatten(),
        "median_resolution_days": row.try_get::<Option<f64>, _>("median_resolution_days").ok().flatten(),
    })))
}

#[derive(Deserialize)]
pub struct CarcOptionsQuery {
    pub status: Option<String>,
}

pub async fn carc_options(
    State(state): State<AppState>,
    Query(params): Query<CarcOptionsQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let rows;
    if let Some(ref status) = params.status {
        rows = sqlx::query(
            "SELECT d.carc_code, COALESCE(cc.description, 'No description on file') AS description, \
             COUNT(*) AS denial_count \
             FROM denials d LEFT JOIN carc_codes cc ON cc.code = d.carc_code \
             WHERE d.carc_code IS NOT NULL AND d.carc_code <> '' AND d.status = $1 \
             GROUP BY d.carc_code, cc.description \
             ORDER BY COUNT(*) DESC, d.carc_code",
        )
        .bind(status)
        .fetch_all(&state.pool)
        .await
        .map_err(AppError::Db)?;
    } else {
        rows = sqlx::query(
            "SELECT d.carc_code, COALESCE(cc.description, 'No description on file') AS description, \
             COUNT(*) AS denial_count \
             FROM denials d LEFT JOIN carc_codes cc ON cc.code = d.carc_code \
             WHERE d.carc_code IS NOT NULL AND d.carc_code <> '' \
               AND d.status = ANY(ARRAY['open','analyzed']::text[]) \
             GROUP BY d.carc_code, cc.description \
             ORDER BY COUNT(*) DESC, d.carc_code",
        )
        .fetch_all(&state.pool)
        .await
        .map_err(AppError::Db)?;
    }

    Ok(Json(rows.iter().map(row_to_json).collect()))
}

pub async fn list_appeal_windows(
    State(state): State<AppState>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let rows = sqlx::query(
        "SELECT p.id, p.payer_name, p.appeal_window_days, p.notes, p.updated_at, \
         (SELECT COUNT(DISTINCT c.id) FROM claims c \
           WHERE p.payer_name <> '*' AND lower(c.payer_name) = lower(p.payer_name)) AS claims_covered \
         FROM payer_appeal_policies p \
         ORDER BY (p.payer_name = '*') DESC, p.payer_name",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let unconfigured = sqlx::query(
        "SELECT c.payer_name, COUNT(DISTINCT c.id) AS claims \
         FROM claims c \
         WHERE c.payer_name IS NOT NULL \
           AND NOT EXISTS (SELECT 1 FROM payer_appeal_policies p WHERE lower(p.payer_name) = lower(c.payer_name)) \
         GROUP BY c.payer_name ORDER BY 2 DESC",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let mut out = Vec::new();
    for row in &rows {
        let id: Uuid = row
            .try_get("id")
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let payer_name: String = row
            .try_get("payer_name")
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let appeal_window_days: Option<i32> = row
            .try_get("appeal_window_days")
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let notes: Option<String> = row
            .try_get("notes")
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let updated_at: Option<DateTime<Utc>> = row
            .try_get("updated_at")
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let claims_covered: Option<i64> = row
            .try_get("claims_covered")
            .map_err(|e| AppError::Internal(e.to_string()))?;

        out.push(serde_json::json!({
            "id": id.to_string(),
            "payer_name": payer_name,
            "appeal_window_days": appeal_window_days,
            "notes": notes,
            "updated_at": updated_at.map(|t| t.to_rfc3339()),
            "is_default": payer_name == "*",
            "claims_covered": claims_covered,
        }));
    }
    for row in &unconfigured {
        let payer_name: String = row
            .try_get("payer_name")
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let claims: Option<i64> = row
            .try_get("claims")
            .map_err(|e| AppError::Internal(e.to_string()))?;
        out.push(serde_json::json!({
            "id": null,
            "payer_name": payer_name,
            "appeal_window_days": null,
            "notes": null,
            "updated_at": null,
            "is_default": false,
            "claims_covered": claims,
            "using_default": true,
        }));
    }

    Ok(Json(out))
}

pub async fn set_appeal_window(
    State(state): State<AppState>,
    Json(body): Json<PayerWindow>,
) -> Result<Json<serde_json::Value>, AppError> {
    let name = body.payer_name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::BadRequest("A payer name is required".into()));
    }

    sqlx::query(
        "INSERT INTO payer_appeal_policies (payer_name, appeal_window_days, notes) \
         VALUES ($1, $2, $3) \
         ON CONFLICT (lower(payer_name)) DO UPDATE \
             SET appeal_window_days = EXCLUDED.appeal_window_days, \
                 notes = EXCLUDED.notes, \
                 updated_at = NOW()",
    )
    .bind(&name)
    .bind(body.appeal_window_days as i32)
    .bind(&body.notes)
    .execute(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let updated_count: (i64,) = if name == "*" {
        let r: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM (UPDATE denials d \
             SET appeal_deadline = appeal_deadline_for(c.payer_name, d.denial_date), updated_at = NOW() \
             FROM claims c WHERE c.id = d.claim_id \
               AND d.status IN ('open', 'analyzed') \
               AND NOT EXISTS (SELECT 1 FROM payer_appeal_policies p \
                                 WHERE p.payer_name <> '*' AND lower(p.payer_name) = lower(c.payer_name)) \
             RETURNING d.id) sub",
        )
        .fetch_one(&state.pool)
        .await
        .map_err(AppError::Db)?;
        r
    } else {
        let r: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM (UPDATE denials d \
             SET appeal_deadline = appeal_deadline_for(c.payer_name, d.denial_date), updated_at = NOW() \
             FROM claims c WHERE c.id = d.claim_id \
               AND d.status IN ('open', 'analyzed') \
               AND lower(c.payer_name) = lower($1) \
             RETURNING d.id) sub",
        )
        .bind(&name)
        .fetch_one(&state.pool)
        .await
        .map_err(AppError::Db)?;
        r
    };

    Ok(Json(serde_json::json!({
        "status": "saved",
        "payer_name": name,
        "appeal_window_days": body.appeal_window_days,
        "denials_redated": updated_count.0,
    })))
}

pub async fn get_denial(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    axum::extract::Path(denial_id): axum::extract::Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let row = sqlx::query(
        "SELECT d.*, c.claim_number, c.patient_name, c.patient_id, c.facility_type_code, \
         c.date_of_birth, c.payer_name, c.payer_id_number, \
         c.icd_10_codes, c.total_charge, c.total_paid, \
         cc.description as carc_description, \
         rc.description as rarc_description, \
         aa.id AS ai_analysis_id, \
         aa.explanation, aa.action_plan, aa.steps, aa.draft_appeal_letter, \
         aa.denial_category, aa.required_action, aa.needs_appeal, \
         aa.confidence_score, \
         aq.id AS appeal_id, aq.outcome_status AS appeal_status, \
         aq.resolution_type AS appeal_resolution_type \
         FROM denials d \
         JOIN claims c ON c.id = d.claim_id \
         LEFT JOIN carc_codes cc ON cc.code = d.carc_code \
         LEFT JOIN rarc_codes rc ON rc.code = d.rarc_code \
         LEFT JOIN LATERAL (SELECT * FROM ai_analyses a WHERE a.denial_id = d.id ORDER BY a.created_at DESC LIMIT 1) aa ON TRUE \
         LEFT JOIN appeals_queue aq ON aq.denial_id = d.id \
             AND (aq.outcome_status IS NULL OR aq.outcome_status NOT IN ('approved','overruled','resolved','denied_again','cancelled')) \
         WHERE d.id = $1 AND c.organization_id = $2",
    )
    .bind(denial_id)
    .bind(organization_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;

    let mut denial = row_to_json(&row);

    let cagc: Option<String> = row.try_get("cagc").unwrap_or(None);
    let required_action: Option<String> = row.try_get("required_action").unwrap_or(None);
    let denial_category: Option<String> = row.try_get("denial_category").unwrap_or(None);

    let recommendation = recommended_resolution(
        cagc.as_deref(),
        required_action.as_deref(),
        denial_category.as_deref(),
    );
    denial["recommended_resolution"] = serde_json::to_value(recommendation.resolution).unwrap();
    denial["recommendation_note"] = serde_json::to_value(recommendation.note).unwrap();

    Ok(Json(denial))
}

/// The denial workflow state machine. `denials.status` is a flat set; this
/// table is the set of statuses each state may move to. A no-op (same status)
/// is always allowed. Terminal states can be reopened into active work but not
/// reset back to the start, and `overruled` is only reachable once an appeal
/// has been underway (you cannot be overruled from a bare `open` denial).
fn allowed_transitions(current: &str) -> &'static [&'static str] {
    match current {
        "open" => &[
            "analyzed",
            "in_progress",
            "in_appeal",
            "appealed",
            "resolved",
            "written_off",
        ],
        "analyzed" => &[
            "open",
            "in_progress",
            "in_appeal",
            "appealed",
            "overruled",
            "resolved",
            "written_off",
        ],
        "in_progress" => &[
            "open",
            "analyzed",
            "in_appeal",
            "appealed",
            "overruled",
            "resolved",
            "written_off",
        ],
        "in_appeal" => &[
            "open",
            "analyzed",
            "in_progress",
            "appealed",
            "overruled",
            "resolved",
            "written_off",
        ],
        "appealed" => &[
            "open",
            "analyzed",
            "in_progress",
            "in_appeal",
            "overruled",
            "resolved",
            "written_off",
        ],
        "overruled" => &[
            "open",
            "analyzed",
            "in_progress",
            "in_appeal",
            "appealed",
            "resolved",
            "written_off",
        ],
        "resolved" => &["in_progress", "in_appeal", "appealed", "overruled"],
        "written_off" => &["in_progress", "in_appeal", "appealed", "overruled"],
        _ => &[],
    }
}

pub async fn update_denial(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    axum::extract::Path(denial_id): axum::extract::Path<Uuid>,
    Json(body): Json<DenialUpdate>,
) -> Result<Json<serde_json::Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let mut sets = Vec::new();

    // Enforce the workflow state machine before writing: read the current
    // status (org-scoped) and reject a transition the machine does not allow,
    // so a denial cannot be teleported to an arbitrary state.
    let (status_changed, status_from) = if let Some(ref status) = body.status {
        let current: Option<String> = sqlx::query_scalar(
            "SELECT status FROM denials WHERE id = $1 \
             AND claim_id IN (SELECT id FROM claims WHERE organization_id = $2)",
        )
        .bind(denial_id)
        .bind(organization_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::Db)?;

        let current = current.ok_or(AppError::NotFound)?;
        let changed = current != *status;
        if changed && !allowed_transitions(&current).contains(&status.as_str()) {
            return Err(AppError::Conflict(format!(
                "Invalid denial status transition: {current} -> {status}"
            )));
        }
        sets.push(format!("status = ${}", sets.len() + 1));
        (changed, if changed { Some(current) } else { None })
    } else {
        (false, None)
    };

    if body.appeal_deadline.is_some() {
        sets.push(format!("appeal_deadline = ${}", sets.len() + 1));
    }

    if sets.is_empty() {
        return Err(AppError::BadRequest("No updates provided".into()));
    }

    sets.push("updated_at = NOW()".to_string());
    let id_idx = sets.len() + 1;

    let sql = format!(
        "UPDATE denials SET {} WHERE id = ${} AND claim_id IN (SELECT id FROM claims WHERE organization_id = ${}) RETURNING *",
        sets.join(", "),
        id_idx,
        id_idx + 1
    );

    let mut q = sqlx::query(&sql);
    if let Some(ref s) = body.status {
        q = q.bind(s);
    }
    if let Some(d) = body.appeal_deadline {
        q = q.bind(d);
    }
    q = q.bind(denial_id);
    q = q.bind(organization_id);

    let row = q
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::Db)?
        .ok_or(AppError::NotFound)?;

    // Individually audit each status transition (who, from -> to, when).
    if status_changed {
        let to = body.status.clone().unwrap_or_default();
        let from = status_from.unwrap_or_default();
        denial_audit::record(
            &state.pool,
            "denial_status_changed",
            "denial",
            Some(&denial_id.to_string()),
            principal.user_id.as_deref(),
            &serde_json::json!({ "from": from, "to": to }),
            None,
            None,
        )
        .await;
    }

    Ok(Json(row_to_json(&row)))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_denials))
        .route("/bulk-carc", get(denials_by_carc))
        .route("/by-payer", get(denials_by_payer))
        .route("/by-root-cause", get(denials_by_root_cause))
        .route("/aging-buckets", get(denial_aging_buckets))
        .route("/financial-summary", get(denial_financial_summary))
        .route("/resolution-timing", get(denial_resolution_timing))
        .route("/carc-options", get(carc_options))
        .route(
            "/appeal-windows",
            get(list_appeal_windows).put(set_appeal_window),
        )
        .route("/{denial_id}", get(get_denial).patch(update_denial))
}

#[cfg(test)]
mod tests {
    use super::allowed_transitions;

    #[test]
    fn open_cannot_jump_to_overruled() {
        // You cannot be overruled before an appeal has been underway.
        assert!(!allowed_transitions("open").contains(&"overruled"));
    }

    #[test]
    fn open_reaches_normal_forward_states() {
        for next in [
            "analyzed",
            "in_progress",
            "in_appeal",
            "appealed",
            "resolved",
            "written_off",
        ] {
            assert!(
                allowed_transitions("open").contains(&next),
                "open -> {next}"
            );
        }
    }

    #[test]
    fn terminal_states_cannot_reset_to_start() {
        for terminal in ["resolved", "written_off"] {
            assert!(
                !allowed_transitions(terminal).contains(&"open"),
                "{terminal} -> open"
            );
            assert!(
                !allowed_transitions(terminal).contains(&"analyzed"),
                "{terminal} -> analyzed"
            );
        }
    }

    #[test]
    fn terminal_states_can_reopen_into_active_work() {
        for terminal in ["resolved", "written_off"] {
            for next in ["in_progress", "in_appeal", "appealed", "overruled"] {
                assert!(
                    allowed_transitions(terminal).contains(&next),
                    "{terminal} -> {next}"
                );
            }
        }
    }

    #[test]
    fn appeal_states_reach_overruled_and_resolved() {
        for s in ["in_appeal", "appealed"] {
            assert!(
                allowed_transitions(s).contains(&"overruled"),
                "{s} -> overruled"
            );
            assert!(
                allowed_transitions(s).contains(&"resolved"),
                "{s} -> resolved"
            );
        }
    }

    #[test]
    fn unknown_status_allows_nothing() {
        assert!(allowed_transitions("bogus").is_empty());
    }
}
