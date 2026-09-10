//! Shared foundation for the Denial Navigator services.
//!
//! Config, database, authentication, TOTP/2FA, RBAC, audit, rate limiting and
//! downstream HTTP clients. Every service links this crate so the security
//! core lives in exactly one place.

pub mod clients;
pub mod config;
pub mod error;
pub mod internal_auth;
pub mod logging;
pub mod ratelimit;
pub mod totp;

pub use error::AppError;
