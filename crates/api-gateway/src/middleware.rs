//! Access-control middleware.
//!
//! The whole decision - public paths, identity resolution, MFA confinement,
//! account currency (deactivation / role change / session revocation), and
//! role-based authorisation - lives in [`denial_auth::rbac::decide`], a
//! faithful port of `services/access.py`. This layer snapshots the request
//! into a [`RequestCtx`] (so the future stays `Send` across the DB check),
//! runs the decision, and attaches the resolved [`Principal`] for handlers to
//! read via `Extension<Principal>`.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use denial_auth::rbac::{
    client_ip, decide, forbidden, unauthorized, Decision, Principal, RequestCtx,
};
use denial_common::config::GatewayConfig;
use denial_common::AppError;
use ipnet::IpNet;
use sqlx::PgPool;

/// Access-control middleware state.
#[derive(Clone)]
pub struct AccessState {
    pub pool: PgPool,
    pub config: GatewayConfig,
    pub trusted: Vec<IpNet>,
}

/// Access-control middleware. See the module docs.
pub async fn access(
    State(state): State<AccessState>,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let peer: Option<SocketAddr> = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    let ctx = RequestCtx::from_request(&req, peer);

    Ok(
        match decide(&state.pool, &ctx, &state.config, &state.trusted).await {
            Decision::Public => {
                let mut p = Principal::anonymous();
                p.ip = client_ip(peer, &req, &state.trusted);
                req.extensions_mut().insert(p);
                next.run(req).await
            }
            Decision::Unauthorized(detail) => unauthorized(&detail),
            Decision::Forbidden(detail) => forbidden(&detail),
            Decision::Authorized(principal) => {
                req.extensions_mut().insert(principal);
                next.run(req).await
            }
        },
    )
}
