//! SQLx migration orchestration for the PostgreSQL system of record.
//!
//! The project pre-dates SQLx migrations and shipped a complete `init.sql`
//! snapshot plus hand-applied numbered change scripts.  This module preserves
//! safe upgrades from that layout: a verified current snapshot is baselined
//! once, then SQLx owns all future migration execution and checksums.

use sqlx::migrate::Migrator;
use sqlx::PgPool;

static MIGRATOR: Migrator = sqlx::migrate!("../../database/migrations");
const INITIAL_SCHEMA: &str = include_str!("../../../database/init.sql");

/// Apply schema changes before serving traffic.
///
/// An empty database is initialized from the checked-in schema snapshot. A
/// database created by the legacy Compose bootstrap is either baselined as a
/// current snapshot, or recognized as the final pre-SQLx (migration 13)
/// schema and advanced through the remaining SQLx migrations.
pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    let has_claims: bool = sqlx::query_scalar("SELECT to_regclass('public.claims') IS NOT NULL")
        .fetch_one(pool)
        .await?;

    if !has_claims {
        tracing::info!("initializing empty database schema");
        sqlx::raw_sql(INITIAL_SCHEMA).execute(pool).await?;
        apply_snapshot_tail(pool).await?;
    }

    ensure_migration_table(pool).await?;
    baseline_current_snapshot(pool).await?;
    MIGRATOR.run(pool).await?;
    Ok(())
}

async fn apply_snapshot_tail(pool: &PgPool) -> Result<(), sqlx::Error> {
    // `init.sql` is a deliberately readable current snapshot, but a few
    // newer additive objects remain in the numbered history. Apply that
    // history once only on the empty-database path, before its checksums are
    // entered in the SQLx ledger below. These scripts are idempotent by
    // policy, which also keeps the snapshot and migration tail compatible.
    for migration in MIGRATOR.iter() {
        sqlx::raw_sql(&migration.sql).execute(pool).await?;
    }
    Ok(())
}

async fn ensure_migration_table(pool: &PgPool) -> Result<(), sqlx::Error> {
    // Matches SQLx's own metadata table so `Migrator::run` can validate the
    // checksums inserted while baselining a legacy schema.
    sqlx::raw_sql(
        "CREATE TABLE IF NOT EXISTS _sqlx_migrations (\
            version BIGINT PRIMARY KEY, \
            description TEXT NOT NULL, \
            installed_on TIMESTAMPTZ NOT NULL DEFAULT now(), \
            success BOOLEAN NOT NULL, \
            checksum BYTEA NOT NULL, \
            execution_time BIGINT NOT NULL\
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn baseline_current_snapshot(pool: &PgPool) -> Result<(), sqlx::Error> {
    let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(pool)
        .await?;
    if applied > 0 {
        return Ok(());
    }

    // A database from the current pre-SQLx Compose release has these core
    // tables but does not yet have institutional_playbooks (introduced in
    // migration 017). It is safe to stamp the known manually-applied history
    // through the last migration represented by the snapshot and let SQLx run
    // the remaining files. Anything else is
    // rejected instead of being guessed at or silently marked up-to-date.
    let (current_snapshot, legacy_v13): (bool, bool) = sqlx::query_as(
        "SELECT to_regclass('public.claims') IS NOT NULL \
            AND to_regclass('public.denials') IS NOT NULL \
            AND to_regclass('public.institutional_playbooks') IS NOT NULL \
            AND to_regclass('public.knowledge_chunks') IS NOT NULL, \
         to_regclass('public.claims') IS NOT NULL \
            AND to_regclass('public.denials') IS NOT NULL \
            AND to_regclass('public.knowledge_chunks') IS NOT NULL \
            AND to_regclass('public.ingestion_log') IS NOT NULL \
            AND to_regclass('public.users') IS NOT NULL \
            AND to_regclass('public.institutional_playbooks') IS NULL",
    )
    .fetch_one(pool)
    .await?;
    if !current_snapshot && !legacy_v13 {
        return Err(sqlx::Error::Protocol(
            "database is not a recognized OpenClaim schema; restore a supported backup or upgrade with the prior release before starting this version".into(),
        ));
    }

    // Current snapshots may predate the newest migrations. Leave migrations
    // after 031 un-stamped so role normalization and later upgrades execute.
    const CURRENT_SNAPSHOT_LAST_VERSION: i64 = 29;
    tracing::info!(
        count = if legacy_v13 {
            13
        } else {
            MIGRATOR
                .iter()
                .filter(|migration| migration.version <= CURRENT_SNAPSHOT_LAST_VERSION)
                .count()
        },
        legacy_v13,
        "baselining existing database schema for SQLx"
    );
    let mut tx = pool.begin().await?;
    for migration in MIGRATOR.iter().filter(|migration| {
        (legacy_v13 && migration.version <= 13)
            || (!legacy_v13 && migration.version <= CURRENT_SNAPSHOT_LAST_VERSION)
    }) {
        sqlx::query(
            "INSERT INTO _sqlx_migrations \
             (version, description, success, checksum, execution_time) \
             VALUES ($1, $2, TRUE, $3, 0)",
        )
        .bind(migration.version)
        .bind(&*migration.description)
        .bind(&*migration.checksum)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::MIGRATOR;

    #[test]
    fn historical_migrations_are_ordered_and_unique() {
        let mut previous = 0;
        for migration in MIGRATOR.iter() {
            assert!(migration.version > previous);
            previous = migration.version;
        }
    }
}
