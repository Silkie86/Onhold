//! Connected accounts: a member's mailbox (and later calendar and the
//! workspace's accounting system), with its encrypted grant, health and
//! sync position.

use chrono::{DateTime, Utc};
use core_domain::{UserId, WorkspaceId};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Integration {
    pub id: Uuid,
    pub provider: String,
    pub purposes: Vec<String>,
    pub owner_id: UserId,
    pub account: String,
    pub shared_mailbox: Option<String>,
    pub shared: bool,
    pub display_name: Option<String>,
    pub settings: Value,
    pub secret: Vec<u8>,
    pub scopes: Vec<String>,
    pub status: String,
    pub failures: i32,
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub mail_cursor: Option<String>,
    pub mail_since: DateTime<Utc>,
    pub next_poll_at: Option<DateTime<Utc>>,
    pub connected_at: DateTime<Utc>,
    pub disconnected_at: Option<DateTime<Utc>>,
    pub version: i64,
}

impl Integration {
    /// The address mail is read from and sent as.
    pub fn mailbox(&self) -> &str {
        self.shared_mailbox.as_deref().unwrap_or(&self.account)
    }
}

pub struct NewIntegration<'a> {
    pub id: Uuid,
    pub provider: &'a str,
    pub purposes: &'a [String],
    pub owner_id: UserId,
    pub account: &'a str,
    pub shared_mailbox: Option<&'a str>,
    pub shared: bool,
    pub display_name: Option<&'a str>,
    pub settings: &'a Value,
    pub secret: &'a [u8],
    pub scopes: &'a [String],
    pub mail_since: DateTime<Utc>,
    pub next_poll_at: Option<DateTime<Utc>>,
}

pub async fn insert(conn: &mut PgConnection, n: NewIntegration<'_>) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO integrations (id, workspace_id, provider, purposes, owner_id, account, shared_mailbox,
                                     shared, display_name, settings, secret, scopes, mail_since, next_poll_at)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)"#,
        n.id,
        n.provider,
        n.purposes,
        n.owner_id.0,
        n.account,
        n.shared_mailbox,
        n.shared,
        n.display_name,
        n.settings,
        n.secret,
        n.scopes,
        n.mail_since,
        n.next_poll_at
    )
    .execute(conn)
    .await?;
    Ok(())
}

macro_rules! select_integration {
    ($tail:literal $(, $arg:expr)*) => {
        sqlx::query_as!(
            Integration,
            r#"SELECT id, provider, purposes, owner_id AS "owner_id: UserId", account, shared_mailbox, shared,
                      display_name, settings, secret, scopes, status, failures, last_error, last_error_at,
                      last_synced_at, mail_cursor, mail_since, next_poll_at, connected_at, disconnected_at, version
               FROM integrations WHERE workspace_id = app_workspace_id() AND "# + $tail
            $(, $arg)*
        )
    };
}

/// One connection, live or disconnected.
pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Integration>> {
    Ok(select_integration!("id = $1", id)
        .fetch_optional(conn)
        .await?)
}

/// One connection, locked for an update in this transaction.
pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<Integration>> {
    Ok(select_integration!("id = $1 FOR UPDATE", id)
        .fetch_optional(conn)
        .await?)
}

/// The live connection to a mailbox, if there is one.
pub async fn live_for(
    conn: &mut PgConnection,
    provider: &str,
    mailbox: &str,
) -> Result<Option<Integration>> {
    Ok(select_integration!(
        "provider = $1 AND coalesce(shared_mailbox, account) = $2 AND disconnected_at IS NULL FOR UPDATE",
        provider,
        mailbox
    )
    .fetch_optional(conn)
    .await?)
}

/// Live connections, oldest first: every one when `viewer` is `None`,
/// else the viewer's own and the shared ones.
pub async fn list(conn: &mut PgConnection, viewer: Option<UserId>) -> Result<Vec<Integration>> {
    Ok(select_integration!(
        "disconnected_at IS NULL AND ($1::uuid IS NULL OR owner_id = $1 OR shared) ORDER BY connected_at, id",
        viewer.map(|v| v.0)
    )
    .fetch_all(conn)
    .await?)
}

/// A new grant (connecting again, or a refreshed token), which also clears
/// a `reconnect` state.
pub async fn set_secret(
    conn: &mut PgConnection,
    id: Uuid,
    secret: &[u8],
    scopes: &[String],
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE integrations SET secret = $2, scopes = $3
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        secret,
        scopes
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Connected again by its owner: working, and read from where it stopped.
pub async fn revive(conn: &mut PgConnection, id: Uuid, next_poll_at: DateTime<Utc>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE integrations
           SET status = 'active', failures = 0, last_error = NULL, last_error_at = NULL, next_poll_at = $2
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        next_poll_at
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// New servers for an IMAP mailbox connected again.
pub async fn set_settings(conn: &mut PgConnection, id: Uuid, settings: &Value) -> Result<()> {
    sqlx::query!(
        r#"UPDATE integrations SET settings = $2 WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        settings
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn set_preferences(
    conn: &mut PgConnection,
    id: Uuid,
    shared: bool,
    display_name: Option<&str>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE integrations SET shared = $2, display_name = $3
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        shared,
        display_name
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Starts a read: `false` when another is under way. The lease runs out on
/// its own if the worker stops part way.
pub async fn claim_poll(conn: &mut PgConnection, id: Uuid, lease_secs: f64) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE integrations SET poll_lease_until = now() + make_interval(secs => $2)
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL
             AND status <> 'reconnect' AND (poll_lease_until IS NULL OR poll_lease_until < now())"#,
        id,
        lease_secs
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Asks for a read as soon as the worker can.
pub async fn poll_now(conn: &mut PgConnection, id: Uuid) -> Result<Option<DateTime<Utc>>> {
    Ok(sqlx::query_scalar!(
        r#"UPDATE integrations SET next_poll_at = now()
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL
             AND status <> 'reconnect'
           RETURNING next_poll_at AS "at!""#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// A successful read: healthy again, with the new position.
pub async fn synced(
    conn: &mut PgConnection,
    id: Uuid,
    cursor: Option<&str>,
    next_poll_at: Option<DateTime<Utc>>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE integrations
           SET mail_cursor = $2, next_poll_at = $3, last_synced_at = now(), status = 'active',
               failures = 0, last_error = NULL, last_error_at = NULL, poll_lease_until = NULL
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL"#,
        id,
        cursor,
        next_poll_at
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Forgets the position, so the next read starts from `since`.
pub async fn restart(conn: &mut PgConnection, id: Uuid, since: DateTime<Utc>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE integrations SET mail_cursor = NULL, mail_since = $2, next_poll_at = now()
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        since
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// A failed call. `status` is `failing` (tried again at `next_poll_at`) or
/// `reconnect` (not tried until its owner connects it again).
pub async fn failed(
    conn: &mut PgConnection,
    id: Uuid,
    status: &str,
    error: &str,
    next_poll_at: Option<DateTime<Utc>>,
) -> Result<i32> {
    Ok(sqlx::query_scalar!(
        r#"UPDATE integrations
           SET status = $2, last_error = left($3, 1000), last_error_at = now(), failures = failures + 1,
               next_poll_at = $4, poll_lease_until = NULL
           WHERE workspace_id = app_workspace_id() AND id = $1
           RETURNING failures"#,
        id,
        status,
        error,
        next_poll_at
    )
    .fetch_one(conn)
    .await?)
}

/// Stops using it; its threads and messages stay.
pub async fn disconnect(conn: &mut PgConnection, id: Uuid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE integrations SET disconnected_at = now(), next_poll_at = NULL, secret = '\x'::bytea
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Every live connection a member owns, so leaving a workspace can stop them.
pub async fn owned_by(conn: &mut PgConnection, owner: UserId) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT id FROM integrations
           WHERE workspace_id = app_workspace_id() AND owner_id = $1 AND disconnected_at IS NULL"#,
        owner.0
    )
    .fetch_all(conn)
    .await?)
}

/// A mailbox due to be read, from any workspace (the worker's sweep).
#[derive(Debug, Clone)]
pub struct Due {
    pub id: Uuid,
    pub workspace_id: WorkspaceId,
    pub at: DateTime<Utc>,
}

pub async fn due(conn: &mut PgConnection, now: DateTime<Utc>, limit: i64) -> Result<Vec<Due>> {
    Ok(sqlx::query_as!(
        Due,
        r#"SELECT id, workspace_id AS "workspace_id: WorkspaceId", next_poll_at AS "at!"
           FROM integrations
           WHERE disconnected_at IS NULL AND next_poll_at <= $1
             AND (poll_lease_until IS NULL OR poll_lease_until < $1)
           ORDER BY next_poll_at LIMIT $2"#,
        now,
        limit
    )
    .fetch_all(conn)
    .await?)
}
