//! Feedback loop routes.
//!
//! Ported from `api-gateway/routes/feedback.py`. Records whether a
//! recommendation was accepted and whether the resubmission was paid, and
//! aggregates both against honest denominators - a rate is only computed over
//! the rows where the outcome is actually known.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Extension, Json, Router};
use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use denial_db::pgjson::{row_to_json, value_at};
use serde::Deserialize;
use sqlx::{QueryBuilder, Row};
use uuid::Uuid;

use crate::state::AppState;

#[derive(Deserialize)]
pub struct FeedbackCreate {
    pub ai_analysis_id: String,
    pub user_id: Option<String>,
    pub rating: Option<i16>,
    pub accepted: Option<bool>,
    pub user_edits: Option<serde_json::Value>,
    pub action_taken: Option<String>,
    pub was_paid_on_resubmit: Option<bool>,
    pub resubmit_result: Option<String>,
    pub feedback_text: Option<String>,
}

#[derive(Deserialize)]
pub struct ListFeedbackQuery {
    pub ai_analysis_id: Option<String>,
    pub accepted: Option<bool>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

#[derive(Deserialize)]
pub struct SimilarResolvedCasesQuery {
    pub denial_id: Uuid,
    #[serde(default = "default_similar_limit")]
    pub limit: i64,
}

fn default_similar_limit() -> i64 {
    5
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

fn f64_col(row: &sqlx::postgres::PgRow, col: &str) -> Option<f64> {
    row.try_get::<Option<f64>, _>(col).ok().flatten()
}

fn i64_col(row: &sqlx::postgres::PgRow, col: &str) -> i64 {
    row.try_get::<Option<i64>, _>(col)
        .ok()
        .flatten()
        .unwrap_or(0)
}

/// `round(num / denom, 4)`, or `None` when the denominator is zero - the same
/// honest-denominator rule the SQL is written around.
fn rate(num: i64, denom: i64) -> Option<f64> {
    if denom == 0 {
        None
    } else {
        Some(((num as f64 / denom as f64) * 10_000.0).round() / 10_000.0)
    }
}

pub async fn list_feedback(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<ListFeedbackQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let organization_id = organization_id(&principal)?;
    let mut qb = QueryBuilder::<sqlx::Postgres>::new(
        "SELECT fl.*, aa.explanation, aa.denial_category, \
         c.claim_number, c.patient_name \
         FROM feedback_loop fl \
         JOIN ai_analyses aa ON aa.id = fl.ai_analysis_id \
         JOIN denials d ON d.id = aa.denial_id \
         JOIN claims c ON c.id = aa.claim_id",
    );

    qb.push(" WHERE c.organization_id = ")
        .push_bind(organization_id);
    if let Some(ref v) = params.ai_analysis_id {
        let id = Uuid::parse_str(v.trim())
            .map_err(|_| AppError::BadRequest("ai_analysis_id must be a UUID".into()))?;
        qb.push(" AND fl.ai_analysis_id = ").push_bind(id);
    }
    if let Some(v) = params.accepted {
        qb.push(" AND fl.accepted = ").push_bind(v);
    }

    qb.push(" ORDER BY fl.created_at DESC LIMIT ")
        .push_bind(params.limit.clamp(1, 500));

    let rows = qb
        .build()
        .fetch_all(&state.pool)
        .await
        .map_err(AppError::Db)?;
    Ok(Json(rows.iter().map(row_to_json).collect()))
}

pub async fn create_feedback(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(body): Json<FeedbackCreate>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    let organization_id = organization_id(&principal)?;
    let ai_analysis_id = Uuid::parse_str(body.ai_analysis_id.trim())
        .map_err(|_| AppError::BadRequest("ai_analysis_id must be a UUID".into()))?;
    let belongs: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM ai_analyses aa JOIN claims c ON c.id = aa.claim_id WHERE aa.id = $1 AND c.organization_id = $2)",
    )
    .bind(ai_analysis_id)
    .bind(organization_id)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;
    if !belongs {
        return Err(AppError::NotFound);
    }
    let user_id = match body
        .user_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(s) => Some(
            Uuid::parse_str(s)
                .map_err(|_| AppError::BadRequest("user_id must be a UUID".into()))?,
        ),
        None => None,
    };
    let user_edits = match &body.user_edits {
        Some(v) => Some(serde_json::to_string(v).map_err(|e| AppError::Internal(e.to_string()))?),
        None => None,
    };

    let row = sqlx::query(
        "INSERT INTO feedback_loop \
            (ai_analysis_id, user_id, rating, accepted, user_edits, action_taken, \
             was_paid_on_resubmit, resubmit_result, feedback_text) \
         VALUES ($1, $2, $3, $4, $5::jsonb, $6, $7, $8, $9) \
         RETURNING *",
    )
    .bind(ai_analysis_id)
    .bind(user_id)
    .bind(body.rating)
    .bind(body.accepted)
    .bind(user_edits)
    .bind(&body.action_taken)
    .bind(body.was_paid_on_resubmit)
    .bind(&body.resubmit_result)
    .bind(&body.feedback_text)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    Ok((StatusCode::CREATED, Json(row_to_json(&row))))
}

pub async fn feedback_analytics(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<serde_json::Value>, AppError> {
    let organization_id = organization_id(&principal)?;
    let head = sqlx::query(
        "SELECT \
            COUNT(*)                                                 AS total_feedback, \
            COUNT(*) FILTER (WHERE accepted)                         AS accepted_count, \
            COUNT(*) FILTER (WHERE accepted IS NOT NULL)             AS rated_accept_count, \
            COUNT(*) FILTER (WHERE was_paid_on_resubmit)             AS success_count, \
            COUNT(*) FILTER (WHERE was_paid_on_resubmit IS NOT NULL) AS outcome_known_count, \
            AVG(rating)::float8                                      AS avg_rating, \
            COUNT(*) FILTER (WHERE action_taken = 'corrected_claim') AS corrected_count \
         FROM feedback_loop fl \
         JOIN ai_analyses aa ON aa.id = fl.ai_analysis_id \
         JOIN claims c ON c.id = aa.claim_id \
         WHERE c.organization_id = $1",
    )
    .bind(organization_id)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let by_action = sqlx::query(
        "SELECT COALESCE(aa.required_action, 'unclassified')        AS required_action, \
                COUNT(*)                                            AS feedback_count, \
                AVG(fl.rating)::float8                              AS avg_rating, \
                COUNT(*) FILTER (WHERE fl.was_paid_on_resubmit)     AS paid_count, \
                COUNT(*) FILTER (WHERE fl.was_paid_on_resubmit IS NOT NULL) AS outcome_known \
         FROM feedback_loop fl JOIN ai_analyses aa ON aa.id = fl.ai_analysis_id \
         JOIN claims c ON c.id = aa.claim_id \
         WHERE c.organization_id = $1 \
         GROUP BY COALESCE(aa.required_action, 'unclassified') \
         ORDER BY COUNT(*) DESC",
    )
    .bind(organization_id)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let by_playbook = sqlx::query(
        "SELECT COALESCE(pb.name, 'No playbook applied') AS playbook_name, \
                COUNT(*) AS feedback_count, AVG(fl.rating)::float8 AS avg_rating, \
                COUNT(*) FILTER (WHERE fl.was_paid_on_resubmit) AS paid_count, \
                COUNT(*) FILTER (WHERE fl.was_paid_on_resubmit IS NOT NULL) AS outcome_known \
         FROM feedback_loop fl JOIN ai_analyses aa ON aa.id = fl.ai_analysis_id \
         JOIN claims c ON c.id = aa.claim_id \
         LEFT JOIN institutional_playbooks pb ON pb.id = aa.playbook_id \
         WHERE c.organization_id = $1 \
         GROUP BY COALESCE(pb.name, 'No playbook applied') \
         ORDER BY COUNT(*) DESC",
    )
    .bind(organization_id)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let by_carc = sqlx::query(
        "SELECT d.carc_code, \
                COALESCE(cc.description, 'No description on file')  AS carc_description, \
                COUNT(*)                                            AS feedback_count, \
                AVG(fl.rating)::float8                              AS avg_rating, \
                COUNT(*) FILTER (WHERE fl.was_paid_on_resubmit)     AS paid_count, \
                COUNT(*) FILTER (WHERE fl.was_paid_on_resubmit IS NOT NULL) AS outcome_known \
         FROM feedback_loop fl \
         JOIN ai_analyses aa ON aa.id = fl.ai_analysis_id \
         JOIN denials d ON d.id = aa.denial_id \
         JOIN claims c ON c.id = d.claim_id \
         LEFT JOIN carc_codes cc ON cc.code = d.carc_code \
         WHERE c.organization_id = $1 \
         GROUP BY d.carc_code, cc.description \
         ORDER BY COUNT(*) DESC LIMIT 10",
    )
    .bind(organization_id)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let by_payer = sqlx::query(
        "SELECT COALESCE(c.payer_name, 'Unknown')                   AS payer_name, \
                COUNT(*)                                            AS feedback_count, \
                AVG(fl.rating)::float8                              AS avg_rating, \
                COUNT(*) FILTER (WHERE fl.was_paid_on_resubmit)     AS paid_count, \
                COUNT(*) FILTER (WHERE fl.was_paid_on_resubmit IS NOT NULL) AS outcome_known, \
                COALESCE((SUM(d.charge_amount) FILTER (WHERE fl.was_paid_on_resubmit))::float8, 0) \
                    AS recovered_amount \
         FROM feedback_loop fl \
         JOIN ai_analyses aa ON aa.id = fl.ai_analysis_id \
         JOIN denials d ON d.id = aa.denial_id \
         JOIN claims c ON c.id = d.claim_id \
         WHERE c.organization_id = $1 \
         GROUP BY COALESCE(c.payer_name, 'Unknown') \
         ORDER BY COUNT(*) DESC LIMIT 10",
    )
    .bind(organization_id)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let trend = sqlx::query(
        "SELECT date_trunc('month', fl.created_at)::date            AS month, \
                COUNT(*)                                            AS feedback_count, \
                AVG(fl.rating)::float8                              AS avg_rating, \
                COUNT(*) FILTER (WHERE fl.was_paid_on_resubmit)     AS paid_count, \
                COUNT(*) FILTER (WHERE fl.was_paid_on_resubmit IS NOT NULL) AS outcome_known \
         FROM feedback_loop fl \
         JOIN ai_analyses aa ON aa.id = fl.ai_analysis_id \
         JOIN claims c ON c.id = aa.claim_id \
         WHERE c.organization_id = $1 \
           AND fl.created_at > NOW() - INTERVAL '12 months' \
         GROUP BY 1 ORDER BY 1",
    )
    .bind(organization_id)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let money = sqlx::query(
        "SELECT \
            COALESCE((SUM(d.charge_amount) FILTER (WHERE fl.was_paid_on_resubmit))::float8, 0) \
                AS recovered, \
            COALESCE((SUM(d.charge_amount) FILTER (WHERE fl.was_paid_on_resubmit IS FALSE))::float8, 0) \
                AS not_recovered, \
            COALESCE((SUM(d.charge_amount) FILTER (WHERE fl.was_paid_on_resubmit IS NULL))::float8, 0) \
                AS still_open \
         FROM feedback_loop fl \
         JOIN ai_analyses aa ON aa.id = fl.ai_analysis_id \
         JOIN denials d ON d.id = aa.denial_id \
         JOIN claims c ON c.id = d.claim_id \
         WHERE c.organization_id = $1",
    )
    .bind(organization_id)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;

    let total_analyses: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM ai_analyses aa JOIN claims c ON c.id = aa.claim_id WHERE c.organization_id = $1",
    )
        .bind(organization_id)
        .fetch_one(&state.pool)
        .await
        .map_err(AppError::Db)?;
    let total_analyses = total_analyses.0;

    let grouped = |rows: &[sqlx::postgres::PgRow], key: &'static str| -> Vec<serde_json::Value> {
        rows.iter()
            .map(|r| {
                let paid = i64_col(r, "paid_count");
                let known = i64_col(r, "outcome_known");
                let mut obj = serde_json::json!({
                    key: value_at(r, key),
                    "feedback_count": i64_col(r, "feedback_count"),
                    "avg_rating": f64_col(r, "avg_rating"),
                    "paid_count": paid,
                    "outcome_known": known,
                    "success_rate": rate(paid, known),
                });
                if key == "payer_name" {
                    obj["recovered_amount"] =
                        serde_json::json!(f64_col(r, "recovered_amount").unwrap_or(0.0));
                }
                if key == "carc_code" {
                    obj["carc_description"] = value_at(r, "carc_description");
                }
                obj
            })
            .collect()
    };

    let total_feedback = i64_col(&head, "total_feedback");
    let accepted_count = i64_col(&head, "accepted_count");
    let rated_accept_count = i64_col(&head, "rated_accept_count");
    let success_count = i64_col(&head, "success_count");
    let outcome_known_count = i64_col(&head, "outcome_known_count");

    let trend_out: Vec<serde_json::Value> = trend
        .iter()
        .map(|r| {
            let paid = i64_col(r, "paid_count");
            let known = i64_col(r, "outcome_known");
            serde_json::json!({
                "month": value_at(r, "month"),
                "feedback_count": i64_col(r, "feedback_count"),
                "avg_rating": f64_col(r, "avg_rating"),
                "paid_count": paid,
                "outcome_known": known,
                "success_rate": rate(paid, known),
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "total_analyses": total_analyses,
        "total_feedback": total_feedback,
        "coverage_rate": rate(total_feedback, total_analyses),
        "accepted_count": accepted_count,
        "acceptance_rate": rate(accepted_count, rated_accept_count),
        "success_count": success_count,
        "outcome_known_count": outcome_known_count,
        "success_rate": rate(success_count, outcome_known_count),
        "avg_rating": f64_col(&head, "avg_rating"),
        "corrected_count": i64_col(&head, "corrected_count"),
        "by_required_action": grouped(&by_action, "required_action"),
        "by_playbook": grouped(&by_playbook, "playbook_name"),
        "by_carc": grouped(&by_carc, "carc_code"),
        "by_payer": grouped(&by_payer, "payer_name"),
        "trend": trend_out,
        "money": {
            "recovered": f64_col(&money, "recovered").unwrap_or(0.0),
            "not_recovered": f64_col(&money, "not_recovered").unwrap_or(0.0),
            "still_open": f64_col(&money, "still_open").unwrap_or(0.0),
        },
    })))
}

/// Return successful historical cases using coded, non-PHI similarities only.
/// Patient identifiers, claim numbers, narrative notes, and free-text feedback
/// are deliberately absent from both the ranking and response.
pub async fn similar_resolved_cases(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(params): Query<SimilarResolvedCasesQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let organization_id = organization_id(&principal)?;
    let rows = sqlx::query(
        "WITH target AS ( \
             SELECT d.id, d.cagc, d.carc_code, d.cpt_code, c.payer_name \
             FROM denials d JOIN claims c ON c.id = d.claim_id WHERE d.id = $1 AND c.organization_id = $2 \
         ) \
         SELECT aa.id AS analysis_id, d.id AS denial_id, d.cagc, d.carc_code, d.cpt_code, \
                c.payer_name, aa.denial_category, aa.required_action, fl.action_taken, \
                fl.was_paid_on_resubmit, fl.created_at, \
                (CASE WHEN c.payer_name IS NOT DISTINCT FROM target.payer_name THEN 4 ELSE 0 END + \
                 CASE WHEN d.carc_code IS NOT DISTINCT FROM target.carc_code THEN 3 ELSE 0 END + \
                 CASE WHEN d.cpt_code IS NOT DISTINCT FROM target.cpt_code THEN 2 ELSE 0 END + \
                 CASE WHEN d.cagc = target.cagc THEN 1 ELSE 0 END) AS similarity_score \
         FROM target \
         JOIN denials d ON d.id <> target.id \
         JOIN claims c ON c.id = d.claim_id \
         JOIN ai_analyses aa ON aa.denial_id = d.id \
         JOIN feedback_loop fl ON fl.ai_analysis_id = aa.id \
         WHERE fl.was_paid_on_resubmit IS TRUE AND c.organization_id = $2 \
         ORDER BY similarity_score DESC, fl.created_at DESC LIMIT $3",
    )
    .bind(params.denial_id)
    .bind(organization_id)
    .bind(params.limit.clamp(1, 25))
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    Ok(Json(rows.iter().map(row_to_json).collect()))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_feedback).post(create_feedback))
        .route("/analytics", get(feedback_analytics))
        .route("/similar-resolved", get(similar_resolved_cases))
}
