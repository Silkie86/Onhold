//! Database access: connection pool, migrations and (from M1) the RLS helpers.

use std::time::Duration;

use sqlx::migrate::Migrator;
use sqlx::postgres::{PgPool, PgPoolOptions};

pub use sqlx;

/// Every schema migration, embedded in the binary so a deploy carries its own schema.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

/// Pool settings, taken from configuration by the binary.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub url: String,
    pub max_connections: u32,
    pub acquire_timeout: Duration,
}

/// Builds a pool without connecting, so the process starts (and answers
/// `/healthz`) even while the database is still coming up. `/readyz` reports
/// whether it is reachable.
pub fn connect_lazy(config: &PoolConfig) -> Result<PgPool, DbError> {
    let pool = PgPoolOptions::new()
        .max_connections(config.max_connections)
        .acquire_timeout(config.acquire_timeout)
        .connect_lazy(&config.url)?;
    Ok(pool)
}

/// Applies every pending migration.
pub async fn migrate(pool: &PgPool) -> Result<(), DbError> {
    MIGRATOR.run(pool).await?;
    tracing::info!("migrations applied");
    Ok(())
}

/// Round-trips a trivial query to prove the database is reachable.
pub async fn ping(pool: &PgPool) -> Result<(), DbError> {
    let one = sqlx::query_scalar!(r#"SELECT 1 AS "one!""#)
        .fetch_one(pool)
        .await?;
    debug_assert_eq!(one, 1);
    Ok(())
}
