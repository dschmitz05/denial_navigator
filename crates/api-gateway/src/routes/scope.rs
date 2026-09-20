//! Request-scoping helpers shared across route handlers.

use denial_auth::rbac::Principal;
use denial_common::error::AppError;
use uuid::Uuid;

/// The organization a request is scoped to, taken from the caller's
/// principal. Every handler that reads or writes organization-owned data
/// calls this first; a principal with no parseable organization id has no
/// data to see, so this is a 403, not a 400.
pub fn organization_id(principal: &Principal) -> Result<Uuid, AppError> {
    principal
        .organization_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(AppError::Forbidden)
}
