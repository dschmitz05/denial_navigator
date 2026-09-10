//! Validation for structured denial recommendations returned by AI providers.

use serde_json::Value;

const CATEGORIES: &[&str] = &[
    "coding_error",
    "missing_info",
    "lack_of_preauth",
    "medical_necessity",
    "bundled_service",
    "duplicate_claim",
    "timely_filing",
    "non_covered_service",
    "patient_responsibility",
    "other",
];
const ACTIONS: &[&str] = &[
    "coding_correction",
    "clinical_documentation",
    "appeal",
    "bill_patient",
    "no_action_required",
];

pub fn validate_recommendation(value: &Value) -> Result<(), String> {
    let object = value.as_object().ok_or("response must be a JSON object")?;
    for field in [
        "explanation",
        "denial_category",
        "required_action",
        "root_cause_summary",
        "action_plan",
        "steps",
        "needs_appeal",
        "draft_appeal_letter",
        "confidence_score",
        "evidence_ids",
    ] {
        if !object.contains_key(field) {
            return Err(format!("missing required field: {field}"));
        }
    }
    let string = |field: &str| {
        object
            .get(field)
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| format!("{field} must be a non-empty string"))
    };
    string("explanation")?;
    let category = string("denial_category")?;
    if !CATEGORIES.contains(&category) {
        return Err("denial_category is not allowed".into());
    }
    let action = string("required_action")?;
    if !ACTIONS.contains(&action) {
        return Err("required_action is not allowed".into());
    }
    if !object.get("action_plan").is_some_and(Value::is_object) {
        return Err("action_plan must be an object".into());
    }
    if !object.get("steps").is_some_and(Value::is_array) {
        return Err("steps must be an array".into());
    }
    if !object.get("evidence_ids").is_some_and(|value| {
        value
            .as_array()
            .is_some_and(|ids| ids.iter().all(Value::is_string))
    }) {
        return Err("evidence_ids must be an array of strings".into());
    }
    if !object.get("needs_appeal").is_some_and(Value::is_boolean) {
        return Err("needs_appeal must be a boolean".into());
    }
    let confidence = object
        .get("confidence_score")
        .and_then(Value::as_f64)
        .ok_or("confidence_score must be a number")?;
    if !(0.0..=1.0).contains(&confidence) {
        return Err("confidence_score must be between 0 and 1".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_missing_schema_fields() {
        assert!(validate_recommendation(&serde_json::json!({"explanation":"x"})).is_err());
    }
}
