use axum::extract::{Query, State};
use axum::routing::get;
use axum::Json;
use axum::Router;
use chrono::{DateTime, NaiveDate, Utc};
use denial_common::error::AppError;
use denial_common::pgjson::row_to_json;
use serde::Deserialize;
use sqlx::{QueryBuilder, Row};
use uuid::Uuid;

use crate::state::AppState;

const ACTIVE_DENIAL_STATUSES: &[&str] = &["open", "analyzed"];

#[derive(Deserialize)]
pub struct DenialUpdate {
    pub status: Option<String>,
    pub appeal_deadline: Option<NaiveDate>,
}

#[derive(Deserialize)]
pub struct ListDenialsQuery {
    pub status: Option<String>,
    pub carc_code: Option<String>,
    pub cagc: Option<String>,
    pub claim_id: Option<String>,
    pub q: Option<String>,
    #[serde(default)]
    pub priority: bool,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    50
}

#[derive(Deserialize)]
pub struct PayerWindow {
    pub payer_name: String,
    pub appeal_window_days: u32,
    pub notes: Option<String>,
}

fn recommended_resolution(
    cagc: Option<&str>,
    required_action: Option<&str>,
    denial_category: Option<&str>,
) -> (Option<String>, Option<String>) {
    let action = required_action.unwrap_or("");

    if cagc == Some("PR") && (action.is_empty() || action == "no_action_required") {
        return (
            Some("bill_patient".into()),
            Some(
                "This is a PR (patient responsibility) adjustment, so the balance is \
                 collectible from the patient rather than written off."
                    .into(),
            ),
        );
    }

    let recoverable: &[&str] = &[
        "coding_error",
        "missing_info",
        "bundled_service",
        "lack_of_preauth",
        "medical_necessity",
        "patient_responsibility",
    ];
    let recoverable_map: &[(&str, &str)] = &[
        ("coding_error", "corrected_claim"),
        ("missing_info", "corrected_claim"),
        ("bundled_service", "corrected_claim"),
        ("lack_of_preauth", "clinical_docs"),
        ("medical_necessity", "clinical_docs"),
        ("patient_responsibility", "bill_patient"),
    ];

    if action == "no_action_required" {
        if let Some(cat) = denial_category {
            if recoverable.contains(&cat) {
                let target = recoverable_map
                    .iter()
                    .find(|(k, _)| *k == cat)
                    .map(|(_, v)| v.to_string());
                return (
                    target,
                    Some(format!(
                        "The analysis calls this \"{}\", which is something you can act on, so writing it off would contradict its own explanation.",
                        cat.replace('_', " ")
                    )),
                );
            }
        }
    }

    let resolution_map: &[(&str, &str)] = &[
        ("appeal", "appeal_letter"),
        ("coding_correction", "corrected_claim"),
        ("clinical_documentation", "clinical_docs"),
        ("bill_patient", "bill_patient"),
        ("no_action_required", "write_off"),
    ];

    let result = resolution_map
        .iter()
        .find(|(k, _)| *k == action)
        .map(|(_, v)| v.to_string());

    (result, None)
}

pub async fn list_denials(
    State(state): State<AppState>,
    Query(params): Query<ListDenialsQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let mut qb = QueryBuilder::<sqlx::Postgres>::new(
        "SELECT * FROM (SELECT DISTINCT ON (d.id) d.*, c.claim_number, c.patient_name, c.payer_name, \
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

    let mut need_where = true;
    let mut push_prefix =
        |qb: &mut QueryBuilder<sqlx::Postgres>, need_where: &mut bool| {
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
    if params.priority {
        qb.push("ORDER BY sub.appeal_deadline ASC NULLS LAST, sub.charge_amount DESC");
    } else {
        qb.push("ORDER BY sub.charge_amount DESC");
    }

    let limit = params.limit.clamp(1, 500);
    let offset = params.offset.max(0);
    qb.push(" LIMIT ");
    qb.push_bind(limit);
    qb.push(" OFFSET ");
    qb.push_bind(offset);

    let rows = qb
        .build()
        .fetch_all(&state.pool)
        .await
        .map_err(AppError::Db)?;
    let values: Vec<serde_json::Value> = rows.iter().map(row_to_json).collect();
    Ok(Json(values))
}

pub async fn denials_by_carc(
    State(state): State<AppState>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let rows = sqlx::query(
        "SELECT d.carc_code, COALESCE(cc.description, 'Unknown') as carc_description, \
         d.cagc, COUNT(*) as denial_count, SUM(d.charge_amount) as total_denied_amount, \
         AVG(d.charge_amount) as avg_denial_amount \
         FROM denials d LEFT JOIN carc_codes cc ON cc.code = d.carc_code \
         WHERE d.status IN ('open', 'analyzed') \
         GROUP BY d.carc_code, cc.description, d.cagc \
         ORDER BY total_denied_amount DESC",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;

    Ok(Json(rows.iter().map(row_to_json).collect()))
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
        let id: Uuid = row.try_get("id").map_err(|e| AppError::Internal(e.to_string()))?;
        let payer_name: String = row.try_get("payer_name").map_err(|e| AppError::Internal(e.to_string()))?;
        let appeal_window_days: Option<i32> = row.try_get("appeal_window_days").map_err(|e| AppError::Internal(e.to_string()))?;
        let notes: Option<String> = row.try_get("notes").map_err(|e| AppError::Internal(e.to_string()))?;
        let updated_at: Option<DateTime<Utc>> = row.try_get("updated_at").map_err(|e| AppError::Internal(e.to_string()))?;
        let claims_covered: Option<i64> = row.try_get("claims_covered").map_err(|e| AppError::Internal(e.to_string()))?;

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
        let payer_name: String = row.try_get("payer_name").map_err(|e| AppError::Internal(e.to_string()))?;
        let claims: Option<i64> = row.try_get("claims").map_err(|e| AppError::Internal(e.to_string()))?;
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
    axum::extract::Path(denial_id): axum::extract::Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let row = sqlx::query(
        "SELECT d.*, c.claim_number, c.patient_name, c.patient_id, \
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
         WHERE d.id = $1",
    )
    .bind(denial_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?
    .ok_or(AppError::NotFound)?;

    let mut denial = row_to_json(&row);

    let cagc: Option<String> = row.try_get("cagc").unwrap_or(None);
    let required_action: Option<String> = row.try_get("required_action").unwrap_or(None);
    let denial_category: Option<String> = row.try_get("denial_category").unwrap_or(None);

    let (resolution, note) = recommended_resolution(
        cagc.as_deref(),
        required_action.as_deref(),
        denial_category.as_deref(),
    );
    denial["recommended_resolution"] = serde_json::to_value(resolution).unwrap();
    denial["recommendation_note"] = serde_json::to_value(note).unwrap();

    Ok(Json(denial))
}

pub async fn update_denial(
    State(state): State<AppState>,
    axum::extract::Path(denial_id): axum::extract::Path<Uuid>,
    Json(body): Json<DenialUpdate>,
) -> Result<Json<serde_json::Value>, AppError> {
    let mut sets = Vec::new();

    if let Some(ref status) = body.status {
        sets.push(format!("status = ${}", sets.len() + 1));
    }
    if body.appeal_deadline.is_some() {
        sets.push(format!("appeal_deadline = ${}", sets.len() + 1));
    }

    if sets.is_empty() {
        return Err(AppError::BadRequest("No updates provided".into()));
    }

    sets.push("updated_at = NOW()".to_string());
    let id_idx = sets.len() + 1;

    let sql = format!(
        "UPDATE denials SET {} WHERE id = ${} RETURNING *",
        sets.join(", "),
        id_idx
    );

    let mut q = sqlx::query(&sql);
    if let Some(ref s) = body.status {
        q = q.bind(s);
    }
    if let Some(d) = body.appeal_deadline {
        q = q.bind(d);
    }
    q = q.bind(denial_id);

    let row = q
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::Db)?
        .ok_or(AppError::NotFound)?;

    Ok(Json(row_to_json(&row)))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_denials))
        .route("/bulk-carc", get(denials_by_carc))
        .route("/carc-options", get(carc_options))
        .route("/appeal-windows", get(list_appeal_windows).put(set_appeal_window))
        .route("/{denial_id}", get(get_denial).patch(update_denial))
}
