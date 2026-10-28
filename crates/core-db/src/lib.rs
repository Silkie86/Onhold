//! Database access: pools, migrations, the tenant transaction that row-level
//! security keys on, and the SQL for every table.
//!
//! Three pools exist. The *owner* pool runs migrations and nothing else in
//! the API. The *app* pool switches every connection to the `onhold_app`
//! role, which row-level security always applies to; all request work uses
//! it, through [`begin`], which sets the workspace and user for one
//! transaction. The *worker* pool runs as `onhold_worker`, which sees the
//! queue tables of every workspace and nothing else; a job that touches a
//! workspace's data does so through the app pool, like a request.

pub mod accounting;
pub mod api_keys;
pub mod app_sign_ins;
pub mod audit;
pub mod auth;
pub mod availability;
pub mod bookings;
pub mod calendar_feeds;
pub mod calendar_push;
pub mod contacts;
pub mod counters;
pub mod crm;
pub mod documents;
pub mod enquiries;
pub mod fields;
pub mod files;
pub mod finance;
pub mod idempotency;
pub mod imports;
pub mod inbox;
pub mod integrations;
pub mod invitations;
pub mod invoices;
pub mod jobs;
pub mod members;
pub mod messages;
pub mod organisations;
pub mod outbox;
pub mod payments;
pub mod pdf_imports;
pub mod pipelines;
pub mod quotes;
pub mod reports;
pub mod rules;
pub mod search;
pub mod sender_domains;
pub mod sessions;
pub mod settlements;
pub mod signatures;
pub mod stripe;
pub mod talent;
pub mod talent_calendars;
pub mod tasks;
pub mod templates;
pub mod things;
pub mod users;
pub mod views;
pub mod webhooks;
pub mod workspaces;

use std::time::Duration;

use core_domain::{UserId, WorkspaceId};
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::{Executor, Postgres, Transaction};

pub use sqlx;

/// Every schema migration, embedded in the binary so a deploy carries its own schema.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// The role all request work runs as. Not a table owner, no BYPASSRLS.
/// (Spelled out literally in [`app_pool_options`], which needs a `'static` query.)
pub const APP_ROLE: &str = "onhold_app";

/// A transaction with the request's workspace and user set.
pub type Tx = Transaction<'static, Postgres>;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

impl DbError {
    /// True when the error is an exclusion-constraint violation (two rows
    /// overlapping where they may not), optionally on a named constraint.
    pub fn is_exclusion_violation(&self, constraint: Option<&str>) -> bool {
        match self {
            DbError::Sqlx(sqlx::Error::Database(db)) => {
                db.code().as_deref() == Some("23P01")
                    && constraint.is_none_or(|c| db.constraint() == Some(c))
            }
            _ => false,
        }
    }

    /// True when the error is a check-constraint violation on a named constraint.
    pub fn is_check_violation(&self, constraint: &str) -> bool {
        match self {
            DbError::Sqlx(sqlx::Error::Database(db)) => {
                db.code().as_deref() == Some("23514") && db.constraint() == Some(constraint)
            }
            _ => false,
        }
    }

    /// True when the error is a unique-constraint violation, optionally on a named constraint.
    pub fn is_unique_violation(&self, constraint: Option<&str>) -> bool {
        match self {
            DbError::Sqlx(sqlx::Error::Database(db)) => {
                db.is_unique_violation() && constraint.is_none_or(|c| db.constraint() == Some(c))
            }
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, DbError>;

/// Pool settings, taken from configuration by the binary.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub url: String,
    pub max_connections: u32,
    pub acquire_timeout: Duration,
}

fn options(config: &PoolConfig) -> PgPoolOptions {
    PgPoolOptions::new()
        .max_connections(config.max_connections)
        .acquire_timeout(config.acquire_timeout)
}

/// The pool for request work. Every connection runs as [`APP_ROLE`]. Lazy,
/// so the process starts (and answers `/healthz`) while the database is
/// still coming up; `/readyz` reports whether it is reachable.
pub fn connect_app(config: &PoolConfig) -> Result<PgPool> {
    Ok(app_pool_options(options(config)).connect_lazy(&config.url)?)
}

/// Adds the role switch to any pool options; used by [`connect_app`] and by tests.
pub fn app_pool_options(options: PgPoolOptions) -> PgPoolOptions {
    options.after_connect(|conn, _meta| {
        Box::pin(async move {
            conn.execute("SET ROLE onhold_app").await?;
            Ok(())
        })
    })
}

/// The pool for the worker's queue work. Every connection runs as `onhold_worker`.
pub fn connect_worker(config: &PoolConfig) -> Result<PgPool> {
    Ok(worker_pool_options(options(config)).connect_lazy(&config.url)?)
}

/// Adds the worker role switch to any pool options; used by [`connect_worker`] and by tests.
pub fn worker_pool_options(options: PgPoolOptions) -> PgPoolOptions {
    options.after_connect(|conn, _meta| {
        Box::pin(async move {
            conn.execute("SET ROLE onhold_worker").await?;
            Ok(())
        })
    })
}

/// The pool for migrations and operator tooling: the login role as-is.
pub fn connect_owner(config: &PoolConfig) -> Result<PgPool> {
    Ok(options(config).connect_lazy(&config.url)?)
}

/// Starts a transaction scoped to `workspace` and `user`. Row-level security
/// reads both for the rest of the transaction; either may be absent (sign-in
/// has no workspace; an API key has no user).
pub async fn begin(
    pool: &PgPool,
    workspace: Option<WorkspaceId>,
    user: Option<UserId>,
) -> Result<Tx> {
    let mut tx = pool.begin().await?;
    set_context(&mut tx, workspace, user).await?;
    Ok(tx)
}

/// Re-points an open transaction at a different workspace or user, for the
/// few flows that cross from one to the other (accepting an invitation).
pub async fn set_context(
    tx: &mut Tx,
    workspace: Option<WorkspaceId>,
    user: Option<UserId>,
) -> Result<()> {
    let ws = workspace.map(|w| w.0.to_string()).unwrap_or_default();
    let uid = user.map(|u| u.0.to_string()).unwrap_or_default();
    sqlx::query!(
        r#"SELECT set_config('app.workspace_id', $1, true) AS "ws", set_config('app.user_id', $2, true) AS "uid""#,
        ws,
        uid
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(())
}

/// Applies every pending migration.
pub async fn migrate(pool: &PgPool) -> Result<()> {
    MIGRATOR.run(pool).await?;
    tracing::info!("migrations applied");
    Ok(())
}

/// Round-trips a trivial query to prove the database is reachable.
pub async fn ping(pool: &PgPool) -> Result<()> {
    let one = sqlx::query_scalar!(r#"SELECT 1 AS "one!""#)
        .fetch_one(pool)
        .await?;
    debug_assert_eq!(one, 1);
    Ok(())
}
