//! Deterministic denial-resolution guidance.
//!
//! The engine intentionally contains pure rules only. Callers supply the
//! normalized adjustment group and analysis fields, then decide how to expose
//! or persist the recommendation.

/// A suggested resolution and optional explanation for a denial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionRecommendation {
    pub resolution: Option<String>,
    pub note: Option<String>,
}

/// Suggest the next resolution based on the adjustment group and AI analysis.
///
/// `next_payer` is a payer that pays after this claim's payer (secondary
/// coverage or a crossover); a patient balance goes there before the patient.
pub fn recommended_resolution(
    cagc: Option<&str>,
    required_action: Option<&str>,
    denial_category: Option<&str>,
    next_payer: Option<&str>,
) -> ResolutionRecommendation {
    let action = required_action.unwrap_or("");

    let patient_billing = action.is_empty()
        || matches!(
            action,
            "no_action_required" | "bill_patient" | "bill_secondary"
        )
        || denial_category == Some("patient_responsibility");
    if let (Some("PR"), Some(payer), true) = (cagc, next_payer, patient_billing) {
        return ResolutionRecommendation {
            resolution: Some("bill_secondary".into()),
            note: Some(format!(
                "This patient-responsibility balance goes to {payer} before the patient: \
                 the claim has other coverage that pays after this payer."
            )),
        };
    }

    if cagc == Some("PR") && (action.is_empty() || action == "no_action_required") {
        return ResolutionRecommendation {
            resolution: Some("bill_patient".into()),
            note: Some(
                "This is a PR (patient responsibility) adjustment, so the balance is \
                 collectible from the patient rather than written off."
                    .into(),
            ),
        };
    }

    let recoverable_target = match denial_category {
        Some("coding_error" | "missing_info" | "bundled_service") => Some("corrected_claim"),
        Some("lack_of_preauth" | "medical_necessity") => Some("clinical_docs"),
        Some("patient_responsibility") => Some("bill_patient"),
        _ => None,
    };

    if action == "no_action_required" {
        if let (Some(category), Some(target)) = (denial_category, recoverable_target) {
            return ResolutionRecommendation {
                resolution: Some(target.into()),
                note: Some(format!(
                    "The analysis calls this \"{}\", which is something you can act on, so writing it off would contradict its own explanation.",
                    category.replace('_', " ")
                )),
            };
        }
    }

    let resolution = match action {
        "appeal" => Some("appeal_letter"),
        "coding_correction" => Some("corrected_claim"),
        "clinical_documentation" => Some("clinical_docs"),
        "bill_patient" => Some("bill_patient"),
        "bill_secondary" => Some("bill_secondary"),
        "no_action_required" => Some("write_off"),
        _ => None,
    };

    ResolutionRecommendation {
        resolution: resolution.map(str::to_owned),
        note: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patient_responsibility_overrides_a_write_off() {
        let recommendation =
            recommended_resolution(Some("PR"), Some("no_action_required"), None, None);

        assert_eq!(recommendation.resolution.as_deref(), Some("bill_patient"));
        assert!(recommendation.note.is_some());
    }

    #[test]
    fn recoverable_category_overrides_no_action() {
        let recommendation = recommended_resolution(
            None,
            Some("no_action_required"),
            Some("medical_necessity"),
            None,
        );

        assert_eq!(recommendation.resolution.as_deref(), Some("clinical_docs"));
        assert!(recommendation
            .note
            .as_deref()
            .unwrap()
            .contains("medical necessity"));
    }

    #[test]
    fn maps_known_actions_without_a_note() {
        let recommendation = recommended_resolution(None, Some("appeal"), None, None);

        assert_eq!(recommendation.resolution.as_deref(), Some("appeal_letter"));
        assert_eq!(recommendation.note, None);
    }

    #[test]
    fn a_patient_balance_with_other_coverage_goes_to_that_payer() {
        let recommendation = recommended_resolution(
            Some("PR"),
            Some("bill_patient"),
            Some("patient_responsibility"),
            Some("SYNTHETIC SECONDARY PLAN"),
        );
        assert_eq!(recommendation.resolution.as_deref(), Some("bill_secondary"));
        assert!(recommendation
            .note
            .unwrap()
            .contains("SYNTHETIC SECONDARY PLAN"));
    }

    #[test]
    fn other_coverage_does_not_override_an_appeal() {
        let recommendation = recommended_resolution(
            Some("PR"),
            Some("appeal"),
            None,
            Some("SYNTHETIC SECONDARY PLAN"),
        );
        assert_eq!(recommendation.resolution.as_deref(), Some("appeal_letter"));
    }
}
