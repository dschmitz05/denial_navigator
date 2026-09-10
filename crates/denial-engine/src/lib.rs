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
pub fn recommended_resolution(
    cagc: Option<&str>,
    required_action: Option<&str>,
    denial_category: Option<&str>,
) -> ResolutionRecommendation {
    let action = required_action.unwrap_or("");

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
        let recommendation = recommended_resolution(Some("PR"), Some("no_action_required"), None);

        assert_eq!(recommendation.resolution.as_deref(), Some("bill_patient"));
        assert!(recommendation.note.is_some());
    }

    #[test]
    fn recoverable_category_overrides_no_action() {
        let recommendation =
            recommended_resolution(None, Some("no_action_required"), Some("medical_necessity"));

        assert_eq!(recommendation.resolution.as_deref(), Some("clinical_docs"));
        assert!(recommendation
            .note
            .as_deref()
            .unwrap()
            .contains("medical necessity"));
    }

    #[test]
    fn maps_known_actions_without_a_note() {
        let recommendation = recommended_resolution(None, Some("appeal"), None);

        assert_eq!(recommendation.resolution.as_deref(), Some("appeal_letter"));
        assert_eq!(recommendation.note, None);
    }
}
