//! Database connection pool (sqlx / PostgreSQL).

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::time::Duration;

use crate::config::DbConfig;

/// Build a pooled PostgreSQL connection.
pub async fn connect(config: &DbConfig) -> Result<PgPool, sqlx::Error> {
    let pool = PgPoolOptions::new()
        .max_connections(config.max_connections)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&config.url)
        .await?;

    sqlx::query("SELECT 1")
        .execute(&pool)
        .await
        .expect("database connectivity check");

    Ok(pool)
}

/// Build a pooled PostgreSQL connection from the environment.
pub async fn connect_from_env() -> Result<PgPool, sqlx::Error> {
    connect(&DbConfig::from_env()).await
}
