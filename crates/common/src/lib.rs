//! Shared foundation for the Denial Navigator services.
//!
//! Config, database, authentication, TOTP/2FA, RBAC, audit, rate limiting and
//! downstream HTTP clients. Every service links this crate so the security
//! core lives in exactly one place.

pub mod config;
pub mod db;
pub mod auth;
pub mod totp;
pub mod rbac;
pub mod audit;
pub mod ratelimit;
pub mod clients;
pub mod error;

pub use error::AppError;
