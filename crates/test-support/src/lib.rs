//! Shared synthetic fixtures for tests across Denial Navigator crates.
//!
//! Fixtures deliberately contain no production PHI.

/// A valid synthetic X12 835 remittance containing claims and denials.
pub const SYNTHETIC_835: &str = include_str!("../fixtures/sample_835.txt");

#[cfg(test)]
mod tests {
    use super::SYNTHETIC_835;

    #[test]
    fn synthetic_remittance_is_present_and_identifies_its_transaction_type() {
        assert!(!SYNTHETIC_835.is_empty());
        assert!(SYNTHETIC_835.contains("ST*835"));
    }
}
