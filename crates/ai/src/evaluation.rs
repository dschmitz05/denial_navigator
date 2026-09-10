//! Synthetic, non-PHI evaluation harness for recommendation outputs.
//!
//! The harness intentionally evaluates the stable contract (category, action,
//! schema validity, and citations) rather than provider-specific wording.

use serde_json::Value;

use crate::output::validate_recommendation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationCase<'a> {
    pub name: &'a str,
    pub expected_category: &'a str,
    pub expected_action: &'a str,
    pub require_evidence: bool,
    pub result: &'a Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationFailure {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationReport {
    pub total: usize,
    pub passed: usize,
    pub failures: Vec<EvaluationFailure>,
}

impl EvaluationReport {
    pub fn pass_rate_percent(&self) -> u8 {
        if self.total == 0 {
            return 0;
        }
        ((self.passed * 100) / self.total) as u8
    }

    pub fn meets_minimum(&self, minimum_percent: u8) -> bool {
        self.pass_rate_percent() >= minimum_percent
    }
}

/// Score synthetic expected outcomes against provider response JSON.
pub fn evaluate(cases: &[EvaluationCase<'_>]) -> EvaluationReport {
    let mut failures = Vec::new();
    for case in cases {
        let result = case.result;
        let reason = validate_recommendation(result)
            .err()
            .or_else(|| {
                (result.get("denial_category").and_then(Value::as_str)
                    != Some(case.expected_category))
                .then(|| format!("expected category {}", case.expected_category))
            })
            .or_else(|| {
                (result.get("required_action").and_then(Value::as_str)
                    != Some(case.expected_action))
                .then(|| format!("expected action {}", case.expected_action))
            })
            .or_else(|| {
                (case.require_evidence
                    && result
                        .get("evidence_ids")
                        .and_then(Value::as_array)
                        .is_none_or(Vec::is_empty))
                .then(|| "expected at least one evidence ID".to_string())
            });
        if let Some(reason) = reason {
            failures.push(EvaluationFailure {
                name: case.name.to_string(),
                reason,
            });
        }
    }
    EvaluationReport {
        total: cases.len(),
        passed: cases.len() - failures.len(),
        failures,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn recommendation(category: &str, action: &str, evidence_ids: Value) -> Value {
        json!({
            "explanation": "Synthetic result only.",
            "denial_category": category,
            "required_action": action,
            "root_cause_summary": "Synthetic reason.",
            "action_plan": {"type": action},
            "steps": [],
            "needs_appeal": false,
            "draft_appeal_letter": "not applicable",
            "confidence_score": 0.8,
            "evidence_ids": evidence_ids,
        })
    }

    #[test]
    fn scores_synthetic_cases_and_reports_failures() {
        let documented = recommendation("missing_info", "clinical_documentation", json!(["p-1"]));
        let wrong_action = recommendation("coding_error", "appeal", json!([]));
        let report = evaluate(&[
            EvaluationCase {
                name: "missing-documentation",
                expected_category: "missing_info",
                expected_action: "clinical_documentation",
                require_evidence: true,
                result: &documented,
            },
            EvaluationCase {
                name: "coding-correction",
                expected_category: "coding_error",
                expected_action: "coding_correction",
                require_evidence: false,
                result: &wrong_action,
            },
        ]);

        assert_eq!(report.total, 2);
        assert_eq!(report.passed, 1);
        assert_eq!(report.pass_rate_percent(), 50);
        assert_eq!(report.failures[0].name, "coding-correction");
    }
}
