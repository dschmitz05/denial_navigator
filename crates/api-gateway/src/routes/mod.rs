pub mod analyses;
pub mod appeals;
pub mod audit;
pub mod auth;
pub mod claims;
pub mod denials;
pub mod feedback;
pub mod ingestion;
pub mod knowledge;
pub mod notifications;
pub mod playbooks;
pub mod reference;
pub mod retention;
pub mod system;
pub mod users;

use crate::state::AppState;
use axum::Router;

pub fn api_router() -> Router<AppState> {
    Router::new()
        .nest("/auth", auth::router())
        .nest("/claims", claims::router())
        .nest("/denials", denials::router())
        .nest("/analyses", analyses::router())
        .nest("/appeals", appeals::router())
        .nest("/ingestion", ingestion::router())
        .nest("/knowledge", knowledge::router())
        .nest("/feedback", feedback::router())
        .nest("/reference", reference::router())
        .nest("/audit", audit::router())
        .nest("/users", users::router())
        .nest("/notifications", notifications::router())
        .nest("/playbooks", playbooks::router())
        .nest("/system", system::router())
        .nest("/retention", retention::router())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Router` panics on overlapping routes at construction, so building the
    /// full tree is enough to catch a conflict between two nested modules.
    #[test]
    fn api_router_builds_without_route_conflicts() {
        let _ = api_router();
    }
}
