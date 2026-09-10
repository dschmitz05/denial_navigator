//! PHI-safe logging helpers.
//!
//! Request bodies, EDI payloads, document text, access tokens, and provider
//! responses are never suitable log fields. Callers log an operation name,
//! bounded metadata, and a scrubbed error category instead.

/// Produce a bounded, single-line diagnostic without retaining likely secrets.
/// This is intentionally conservative: it is for transport/configuration
/// diagnostics, never for user-supplied payloads or database rows.
pub fn safe_error(error: impl std::fmt::Display) -> String {
    let mut text = error.to_string().replace(['\r', '\n'], " ");
    if text.len() > 240 {
        text.truncate(240);
        text.push_str("…");
    }
    let lower = text.to_ascii_lowercase();
    if [
        "authorization",
        "bearer ",
        "api_key",
        "api-key",
        "token=",
        "password=",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        "redacted sensitive diagnostic".to_string()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::safe_error;

    #[test]
    fn sensitive_diagnostics_are_redacted() {
        assert_eq!(
            safe_error("Authorization: Bearer super-secret"),
            "redacted sensitive diagnostic"
        );
        assert_eq!(
            safe_error("api_key=super-secret"),
            "redacted sensitive diagnostic"
        );
    }

    #[test]
    fn diagnostics_are_single_line_and_bounded() {
        let result = safe_error(format!("network failed\n{}", "x".repeat(300)));
        assert!(!result.contains('\n'));
        assert!(result.len() <= 243);
    }
}
