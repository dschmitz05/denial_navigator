//! Pure business rules for denial-management workflows.
//!
//! This crate deliberately has no HTTP, database, or cloud dependencies. Its
//! values can therefore be shared by the API, job worker, and tests without
//! importing infrastructure concerns.

use serde::{Deserialize, Serialize};

/// States in which a denial still needs revenue-cycle work.
pub const ACTIVE_DENIAL_STATUSES: &[&str] = &["open", "analyzed"];

/// Terminal denial case states used in claim rollups.
pub const TERMINAL_DENIAL_STATUSES: &[&str] = &["appealed", "overruled", "resolved", "written_off"];

/// Terminal work-queue outcomes. A queue item with another outcome is active.
pub const TERMINAL_WORK_OUTCOMES: &[&str] = &[
    "approved",
    "overruled",
    "resolved",
    "denied_again",
    "cancelled",
];

/// Resolution types that represent a formal appeal rather than internal work.
pub const APPEAL_RESOLUTION_TYPES: &[&str] = &["appeal_letter"];

/// Work that resolves a denial without formally appealing the payer.
pub const WORKLIST_RESOLUTION_TYPES: &[&str] = &[
    "corrected_claim",
    "clinical_docs",
    "payer_contact",
    "bill_patient",
    "write_off",
];

/// Resolution types accepted by the workflow API.
pub const ALL_RESOLUTION_TYPES: &[&str] = &[
    "appeal_letter",
    "corrected_claim",
    "clinical_docs",
    "payer_contact",
    "bill_patient",
    "write_off",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionType {
    AppealLetter,
    CorrectedClaim,
    ClinicalDocs,
    PayerContact,
    BillPatient,
    WriteOff,
}

impl ResolutionType {
    pub const fn is_appeal(self) -> bool {
        matches!(self, Self::AppealLetter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_appeal_letter_is_an_appeal() {
        assert!(ResolutionType::AppealLetter.is_appeal());
        assert!(!ResolutionType::CorrectedClaim.is_appeal());
    }

    #[test]
    fn terminal_work_outcomes_exclude_active_states() {
        assert!(TERMINAL_WORK_OUTCOMES.contains(&"resolved"));
        assert!(!TERMINAL_WORK_OUTCOMES.contains(&"in_progress"));
    }
}
