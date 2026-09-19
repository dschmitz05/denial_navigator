//! Payer deadline rules beyond the appeal window (FB-10).
//!
//! Payers run several clocks: timely filing from the date of service, corrected
//! claims and reconsiderations from the remittance, and a second-level appeal
//! from the first appeal's decision. What matters to a specialist is the clock
//! for the action being recommended, so a denial shows every deadline that has
//! a rule and marks the one for its recommended resolution.

use axum::extract::{Path, State};
use axum::{Extension, Json};
use chrono::NaiveDate;
use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use serde::Deserialize;
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::routes::appeals::record_audit;
use crate::state::AppState;

/// Rule types, what they are counted from, and a label for people.
pub const RULE_TYPES: &[(&str, &str, &str)] = &[
    ("timely_filing", "date of service", "Timely filing"),
    ("corrected_claim", "remittance date", "Corrected claim"),
    ("reconsideration", "remittance date", "Reconsideration"),
    (
        "appeal_level_2",
        "first-level appeal decision",
        "Second-level appeal",
    ),
];

fn organization_id(principal: &Principal) -> Result<Uuid, AppError> {
    principal
        .organization_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(AppError::Forbidden)
}

/// The deadline that governs a recommended resolution, if any. An appeal after
/// the payer upheld a first appeal is a second-level appeal.
pub fn deadline_for_resolution(resolution: &str, appealed_before: bool) -> Option<&'static str> {
    match resolution {
        "corrected_claim" => Some("corrected_claim"),
        "clinical_docs" => Some("reconsideration"),
        "appeal_letter" if appealed_before => Some("appeal_level_2"),
        "appeal_letter" => Some("appeal_level_1"),
        _ => None,
    }
}

/// The days a payer allows for each rule type (payer rule, else the
/// organization's `*` default).
pub async fn rule_days(
    pool: &sqlx::PgPool,
    organization_id: Uuid,
    payer_name: &str,
) -> Result<Vec<(String, i32)>, AppError> {
    let rows = sqlx::query(
        "SELECT DISTINCT ON (deadline_type) deadline_type, days \
         FROM payer_deadline_rules \
         WHERE organization_id = $1 AND (lower(payer_name) = lower($2) OR payer_name = '*') \
         ORDER BY deadline_type, (payer_name = '*')",
    )
    .bind(organization_id)
    .bind(payer_name)
    .fetch_all(pool)
    .await
    .map_err(AppError::Db)?;
    Ok(rows
        .iter()
        .map(|r| (r.get::<String, _>("deadline_type"), r.get::<i32, _>("days")))
        .collect())
}

/// What a denial's deadlines are counted from.
pub struct Anchors {
    pub date_of_service: Option<NaiveDate>,
    pub remittance_date: Option<NaiveDate>,
    pub first_appeal_decision: Option<NaiveDate>,
    pub appeal_level_1_due: Option<NaiveDate>,
}

/// Every deadline that applies to a denial, and the one for `resolution`.
pub fn deadlines(
    rules: &[(String, i32)],
    anchors: &Anchors,
    resolution: Option<&str>,
    today: NaiveDate,
) -> (Vec<Value>, Option<Value>) {
    let item = |kind: &str, label: &str, due: NaiveDate, basis: String| {
        serde_json::json!({
            "type": kind,
            "label": label,
            "due_date": due.to_string(),
            "days_left": (due - today).num_days(),
            "basis": basis,
        })
    };
    let mut out = Vec::new();
    if let Some(due) = anchors.appeal_level_1_due {
        out.push(item(
            "appeal_level_1",
            "Appeal",
            due,
            "payer appeal window".into(),
        ));
    }
    for (kind, from, label) in RULE_TYPES {
        let Some((_, days)) = rules.iter().find(|(k, _)| k == kind) else {
            continue;
        };
        let anchor = match *kind {
            "timely_filing" => anchors.date_of_service,
            "appeal_level_2" => anchors.first_appeal_decision,
            _ => anchors.remittance_date,
        };
        if let Some(start) = anchor {
            let due = start + chrono::Duration::days(i64::from(*days));
            out.push(item(
                kind,
                label,
                due,
                format!("{days} days from {from} {start}"),
            ));
        }
    }
    let wanted = resolution
        .and_then(|r| deadline_for_resolution(r, anchors.first_appeal_decision.is_some()));
    let action = wanted.and_then(|w| out.iter().find(|d| d["type"] == w).cloned());
    (out, action)
}

pub async fn list_rules(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<Value>>, AppError> {
    let rows = sqlx::query(
        "SELECT id, payer_name, deadline_type, days, notes, updated_at \
         FROM payer_deadline_rules WHERE organization_id = $1 \
         ORDER BY (payer_name = '*') DESC, payer_name, deadline_type",
    )
    .bind(organization_id(&principal)?)
    .fetch_all(&state.pool)
    .await
    .map_err(AppError::Db)?;
    Ok(Json(
        rows.iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.get::<Uuid, _>("id").to_string(),
                    "payer_name": r.get::<String, _>("payer_name"),
                    "deadline_type": r.get::<String, _>("deadline_type"),
                    "days": r.get::<i32, _>("days"),
                    "notes": r.get::<Option<String>, _>("notes"),
                    "updated_at": r.get::<chrono::DateTime<chrono::Utc>, _>("updated_at").to_rfc3339(),
                })
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct RuleInput {
    pub payer_name: String,
    pub deadline_type: String,
    pub days: i32,
    #[serde(default)]
    pub notes: Option<String>,
}

/// Creates or replaces the rule for a payer and deadline type.
pub async fn set_rule(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(input): Json<RuleInput>,
) -> Result<Json<Value>, AppError> {
    let payer = input.payer_name.trim();
    if payer.is_empty() || payer.len() > 255 {
        return Err(AppError::BadRequest(
            "payer_name is required ('*' for the default)".into(),
        ));
    }
    if !RULE_TYPES.iter().any(|(k, _, _)| *k == input.deadline_type) {
        return Err(AppError::Unprocessable(
            "deadline_type must be timely_filing, corrected_claim, reconsideration or appeal_level_2".into(),
        ));
    }
    if !(1..=3650).contains(&input.days) {
        return Err(AppError::Unprocessable(
            "days must be between 1 and 3650".into(),
        ));
    }
    let organization_id = organization_id(&principal)?;
    let user = principal
        .user_id
        .as_deref()
        .and_then(|u| Uuid::parse_str(u).ok());
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO payer_deadline_rules (organization_id, payer_name, deadline_type, days, notes, updated_by) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (organization_id, lower(payer_name), deadline_type) \
         DO UPDATE SET days = EXCLUDED.days, notes = EXCLUDED.notes, \
                       updated_by = EXCLUDED.updated_by, updated_at = NOW() \
         RETURNING id",
    )
    .bind(organization_id)
    .bind(payer)
    .bind(&input.deadline_type)
    .bind(input.days)
    .bind(&input.notes)
    .bind(user)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::Db)?;
    record_audit(
        &state.pool,
        &principal,
        "deadline_rule_set",
        "deadline_rule",
        Some(&id.to_string()),
        &serde_json::json!({
            "username": principal.username,
            "payer_name": payer,
            "deadline_type": input.deadline_type,
            "days": input.days,
        }),
    )
    .await;
    Ok(Json(serde_json::json!({ "id": id.to_string() })))
}

pub async fn delete_rule(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let deleted = sqlx::query(
        "DELETE FROM payer_deadline_rules WHERE id = $1 AND organization_id = $2 RETURNING id",
    )
    .bind(id)
    .bind(organization_id(&principal)?)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::Db)?;
    if deleted.is_none() {
        return Err(AppError::NotFound);
    }
    record_audit(
        &state.pool,
        &principal,
        "deadline_rule_deleted",
        "deadline_rule",
        Some(&id.to_string()),
        &serde_json::json!({ "username": principal.username }),
    )
    .await;
    Ok(Json(serde_json::json!({ "deleted": id.to_string() })))
}

#[cfg(test)]
mod tests {
    use super::{deadline_for_resolution, deadlines, Anchors};
    use chrono::NaiveDate;

    fn date(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn rules() -> Vec<(String, i32)> {
        vec![
            ("timely_filing".into(), 120),
            ("corrected_claim".into(), 90),
            ("appeal_level_2".into(), 30),
        ]
    }

    #[test]
    fn a_corrected_claim_is_due_90_days_from_the_remittance() {
        let anchors = Anchors {
            date_of_service: Some(date("2026-07-01")),
            remittance_date: Some(date("2026-08-01")),
            first_appeal_decision: None,
            appeal_level_1_due: Some(date("2026-10-30")),
        };
        let (all, action) = deadlines(
            &rules(),
            &anchors,
            Some("corrected_claim"),
            date("2026-08-10"),
        );
        assert_eq!(
            all.len(),
            3,
            "appeal, timely filing, corrected claim: {all:?}"
        );
        let action = action.expect("corrected claim deadline");
        assert_eq!(action["due_date"], "2026-10-30");
        assert_eq!(action["days_left"], 81);
    }

    #[test]
    fn an_appeal_after_a_denied_first_appeal_is_second_level() {
        assert_eq!(
            deadline_for_resolution("appeal_letter", true),
            Some("appeal_level_2")
        );
        assert_eq!(
            deadline_for_resolution("appeal_letter", false),
            Some("appeal_level_1")
        );
        assert_eq!(deadline_for_resolution("bill_patient", false), None);
    }

    #[test]
    fn a_rule_without_its_anchor_date_is_skipped() {
        let anchors = Anchors {
            date_of_service: None,
            remittance_date: None,
            first_appeal_decision: None,
            appeal_level_1_due: None,
        };
        let (all, action) = deadlines(
            &rules(),
            &anchors,
            Some("corrected_claim"),
            date("2026-08-10"),
        );
        assert!(all.is_empty() && action.is_none());
    }
}
