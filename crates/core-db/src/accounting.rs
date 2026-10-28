//! A workspace's connection to its books in Xero or QuickBooks, and what in
//! the books stands for what in OnHold.

use chrono::{DateTime, NaiveDate, Utc};
use core_domain::{UserId, WorkspaceId};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Connection {
    pub id: Uuid,
    pub provider: String,
    pub tenant_id: String,
    pub tenant_name: String,
    pub automatic_tax: bool,
    pub secret: Vec<u8>,
    pub sales_account: Option<String>,
    pub bank_account: Option<String>,
    pub start_date: NaiveDate,
    pub status: String,
    pub failures: i32,
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub paid_checked_at: Option<DateTime<Utc>>,
    /// Where a read-back cut short goes on from.
    pub paid_cursor: Option<Uuid>,
    /// Refusals from before this are tried again.
    pub retry_before: DateTime<Utc>,
    pub import_status: String,
    pub import_page: i32,
    pub imported: i32,
    pub import_error: Option<String>,
    pub connected_by: Option<UserId>,
    pub connected_at: DateTime<Utc>,
    pub disconnected_at: Option<DateTime<Utc>>,
    pub version: i64,
}

macro_rules! select_connection {
    ($tail:literal $(, $arg:expr)*) => {
        sqlx::query_as!(
            Connection,
            r#"SELECT id, provider, tenant_id, tenant_name, automatic_tax, secret, sales_account,
                      bank_account, start_date, status, failures, last_error, last_error_at,
                      last_synced_at, paid_checked_at, paid_cursor, retry_before, import_status, import_page, imported,
                      import_error, connected_by AS "connected_by: UserId", connected_at,
                      disconnected_at, version
               FROM accounting_connections
               WHERE workspace_id = app_workspace_id() "# + $tail
            $(, $arg)*
        )
    };
}

/// The workspace's live connection, if any.
pub async fn live(conn: &mut PgConnection) -> Result<Option<Connection>> {
    Ok(select_connection!("AND disconnected_at IS NULL")
        .fetch_optional(conn)
        .await?)
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Connection>> {
    Ok(select_connection!("AND id = $1", id)
        .fetch_optional(conn)
        .await?)
}

pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<Connection>> {
    Ok(select_connection!("AND id = $1 FOR UPDATE", id)
        .fetch_optional(conn)
        .await?)
}

/// The latest connection to these books, live or not, locked.
pub async fn for_books(
    conn: &mut PgConnection,
    provider: &str,
    tenant_id: &str,
) -> Result<Option<Connection>> {
    Ok(select_connection!(
        "AND provider = $1 AND tenant_id = $2 ORDER BY connected_at DESC LIMIT 1 FOR UPDATE",
        provider,
        tenant_id
    )
    .fetch_optional(conn)
    .await?)
}

pub struct NewConnection<'a> {
    pub id: Uuid,
    pub provider: &'a str,
    pub tenant_id: &'a str,
    pub tenant_name: &'a str,
    pub automatic_tax: bool,
    pub secret: &'a [u8],
    pub start_date: NaiveDate,
    pub connected_by: Option<UserId>,
}

/// Adds a connection. It syncs once its accounts are chosen.
pub async fn insert(conn: &mut PgConnection, c: NewConnection<'_>) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO accounting_connections
               (id, workspace_id, provider, tenant_id, tenant_name, automatic_tax, secret, start_date, connected_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8)"#,
        c.id,
        c.provider,
        c.tenant_id,
        c.tenant_name,
        c.automatic_tax,
        c.secret,
        c.start_date,
        c.connected_by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Connects the same books again: live, healthy, with the new grant, and
/// due a sync. What was sent before stays known.
pub async fn revive(
    conn: &mut PgConnection,
    id: Uuid,
    tenant_name: &str,
    automatic_tax: bool,
    secret: &[u8],
    connected_by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE accounting_connections
           SET tenant_name = $2, automatic_tax = $3, secret = $4, connected_by = $5,
               connected_at = now(), disconnected_at = NULL, revoke_at = NULL, status = 'active',
               failures = 0, last_error = NULL, last_error_at = NULL, next_sync_at = now(),
               sync_lease_until = NULL, sync_token = NULL, sync_again = false
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        tenant_name,
        automatic_tax,
        secret,
        connected_by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// A refreshed grant.
pub async fn set_secret(conn: &mut PgConnection, id: Uuid, secret: &[u8]) -> Result<()> {
    sqlx::query!(
        r#"UPDATE accounting_connections SET secret = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL"#,
        id,
        secret
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The accounts chosen in settings, and from when issued invoices are
/// sent. Due a sync straight away.
pub async fn set_settings(
    conn: &mut PgConnection,
    id: Uuid,
    sales_account: Option<&str>,
    bank_account: Option<&str>,
    start_date: NaiveDate,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE accounting_connections
           SET sales_account = $2, bank_account = $3, start_date = $4,
               next_sync_at = CASE WHEN status = 'reconnect' OR coalesce(sync_lease_until > now(), false)
                                   THEN next_sync_at ELSE now() END,
               sync_again = sync_again OR coalesce(sync_lease_until > now(), false)
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL"#,
        id,
        sales_account,
        bank_account,
        start_date
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Takes the connection for one sync, returning the token its steps carry:
/// `None` when it is disconnected, needs connecting again, or another sync
/// holds it. Until the sync ends, the next one is due when its lease runs
/// out, so a sync cut off part way is followed by another (queued under a
/// time no job has used).
pub async fn claim(conn: &mut PgConnection, id: Uuid, lease_secs: f64) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"UPDATE accounting_connections
           SET sync_lease_until = now() + make_interval(secs => $3), sync_token = $2,
               next_sync_at = now() + make_interval(secs => $3)
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL
             AND status <> 'reconnect' AND (sync_lease_until IS NULL OR sync_lease_until < now())
           RETURNING sync_token AS "token!""#,
        id,
        core_domain::ids::new_id(),
        lease_secs
    )
    .fetch_optional(conn)
    .await?)
}

/// Keeps the sync holding `token` going: `false` when it no longer holds
/// the connection.
pub async fn renew(
    conn: &mut PgConnection,
    id: Uuid,
    token: Uuid,
    lease_secs: f64,
) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE accounting_connections
           SET sync_lease_until = now() + make_interval(secs => $3),
               next_sync_at = now() + make_interval(secs => $3)
           WHERE workspace_id = app_workspace_id() AND id = $1 AND sync_token = $2
             AND disconnected_at IS NULL"#,
        id,
        token,
        lease_secs
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// A good sync: healthy again, next due at `next`, or straight away when
/// something changed while it ran. With `paid_checked` (a full read-back),
/// what was refused so far is tried again by the next sync.
pub async fn synced(
    conn: &mut PgConnection,
    id: Uuid,
    token: Uuid,
    next: DateTime<Utc>,
    paid_checked: bool,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE accounting_connections
           SET last_synced_at = now(), status = 'active', failures = 0, last_error = NULL,
               last_error_at = NULL, sync_lease_until = NULL, sync_token = NULL,
               paid_checked_at = CASE WHEN $4 AND NOT sync_again THEN now() ELSE paid_checked_at END,
               retry_before = CASE WHEN $4 AND NOT sync_again THEN now() ELSE retry_before END,
               paid_cursor = CASE WHEN $4 THEN NULL ELSE paid_cursor END,
               next_sync_at = CASE WHEN sync_again THEN now() ELSE $3 END,
               sync_again = false
           WHERE workspace_id = app_workspace_id() AND id = $1 AND sync_token = $2
             AND disconnected_at IS NULL"#,
        id,
        token,
        next,
        paid_checked
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// A failed sync, and when to try again (`None`: not until connected again).
pub async fn failed(
    conn: &mut PgConnection,
    id: Uuid,
    token: Uuid,
    status: &str,
    error: &str,
    next: Option<DateTime<Utc>>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE accounting_connections
           SET status = $3, last_error = left($4, 1000), last_error_at = now(), failures = failures + 1,
               next_sync_at = $5, sync_lease_until = NULL, sync_token = NULL, sync_again = false
           WHERE workspace_id = app_workspace_id() AND id = $1 AND sync_token = $2
             AND disconnected_at IS NULL"#,
        id,
        token,
        status,
        error,
        next
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The grant no longer works, found outside a sync (in settings).
pub async fn needs_reconnect(conn: &mut PgConnection, id: Uuid, error: &str) -> Result<()> {
    sqlx::query!(
        r#"UPDATE accounting_connections
           SET status = 'reconnect', last_error = left($2, 1000), last_error_at = now(),
               next_sync_at = NULL
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL"#,
        id,
        error
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// When the next sync is due, if one is to be queued now: not while one
/// runs (it is followed by another when something changed meanwhile).
pub async fn next_sync_at(conn: &mut PgConnection, id: Uuid) -> Result<Option<DateTime<Utc>>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT CASE WHEN coalesce(sync_lease_until > now(), false) THEN NULL ELSE next_sync_at END
           FROM accounting_connections WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id
    )
    .fetch_optional(conn)
    .await?
    .flatten())
}

/// Asks for a sync now (a member asked); with `check_paid`, one that also
/// reads back what was paid and tries every refused item again.
pub async fn sync_now(
    conn: &mut PgConnection,
    id: Uuid,
    check_paid: bool,
) -> Result<Option<DateTime<Utc>>> {
    Ok(sqlx::query_scalar!(
        r#"UPDATE accounting_connections
           SET next_sync_at = CASE WHEN coalesce(sync_lease_until > now(), false) THEN next_sync_at
                                   ELSE now() END,
               sync_again = sync_again OR coalesce(sync_lease_until > now(), false),
               paid_checked_at = CASE WHEN $2 THEN NULL ELSE paid_checked_at END,
               retry_before = CASE WHEN $2 THEN now() ELSE retry_before END
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL
             AND status <> 'reconnect'
           RETURNING next_sync_at AS "at!""#,
        id,
        check_paid
    )
    .fetch_optional(conn)
    .await?)
}

/// Asks for a sync because something changed: one already waiting to run
/// covers it, and one running is followed by another. The time to queue a
/// sync for, if one is to be queued.
pub async fn request_sync(conn: &mut PgConnection, id: Uuid) -> Result<Option<DateTime<Utc>>> {
    let row = sqlx::query!(
        r#"UPDATE accounting_connections
           SET sync_again = sync_again OR coalesce(sync_lease_until > now(), false),
               next_sync_at = CASE WHEN coalesce(sync_lease_until > now(), false) THEN next_sync_at
                                   ELSE least(coalesce(next_sync_at, now()), now()) END
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL
             AND status <> 'reconnect'
           RETURNING next_sync_at AS "at?", coalesce(sync_lease_until > now(), false) AS "running!""#,
        id
    )
    .fetch_optional(conn)
    .await?;
    Ok(row.filter(|r| !r.running).and_then(|r| r.at))
}

/// Lets go of the connection without syncing (nothing chosen to sync to
/// yet): it waits until settings are saved, unless they were saved while
/// it ran.
pub async fn idle(conn: &mut PgConnection, id: Uuid, token: Uuid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE accounting_connections
           SET sync_lease_until = NULL, sync_token = NULL, sync_again = false,
               next_sync_at = CASE WHEN sync_again THEN now() ELSE NULL END
           WHERE workspace_id = app_workspace_id() AND id = $1 AND sync_token = $2"#,
        id,
        token
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Revoking failed for now: try again at `at`.
pub async fn revoke_later(conn: &mut PgConnection, id: Uuid, at: DateTime<Utc>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE accounting_connections SET revoke_at = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND revoke_at IS NOT NULL"#,
        id,
        at
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Starts importing past invoices, from the first page.
pub async fn start_import(conn: &mut PgConnection, id: Uuid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE accounting_connections
           SET import_status = 'running', import_page = 1, imported = 0, import_error = NULL,
               next_sync_at = CASE WHEN status = 'reconnect' OR coalesce(sync_lease_until > now(), false)
                                   THEN next_sync_at ELSE now() END,
               sync_again = sync_again OR coalesce(sync_lease_until > now(), false)
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// A page of past invoices is in: `added` more, and the next page to read
/// (`None`: that was the last).
pub async fn import_progress(
    conn: &mut PgConnection,
    id: Uuid,
    token: Uuid,
    next_page: Option<i32>,
    added: i32,
) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE accounting_connections
           SET imported = imported + $4,
               import_page = coalesce($3, import_page),
               import_status = CASE WHEN $3::int IS NULL THEN 'done' ELSE import_status END
           WHERE workspace_id = app_workspace_id() AND id = $1 AND sync_token = $2
             AND import_status = 'running'"#,
        id,
        token,
        next_page,
        added
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn import_failed(conn: &mut PgConnection, id: Uuid, error: &str) -> Result<()> {
    sqlx::query!(
        r#"UPDATE accounting_connections
           SET import_status = 'failed', import_error = left($2, 1000)
           WHERE workspace_id = app_workspace_id() AND id = $1 AND import_status = 'running'"#,
        id,
        error
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Where a read-back cut short goes on from next time.
pub async fn set_paid_cursor(
    conn: &mut PgConnection,
    id: Uuid,
    token: Uuid,
    cursor: Option<Uuid>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE accounting_connections SET paid_cursor = $3
           WHERE workspace_id = app_workspace_id() AND id = $1 AND sync_token = $2"#,
        id,
        token,
        cursor
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Whether another live connection, in any workspace, reaches these books.
pub async fn books_in_use(
    conn: &mut PgConnection,
    provider: &str,
    tenant_id: &str,
    except: Uuid,
) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT accounting_books_in_use($1, $2, $3) AS "used!""#,
        provider,
        tenant_id,
        except
    )
    .fetch_one(conn)
    .await?)
}

/// Stops syncing. The grant is kept for the worker, which revokes it, then
/// forgets it.
pub async fn disconnect(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE accounting_connections
           SET disconnected_at = now(), next_sync_at = NULL, sync_lease_until = NULL,
               sync_token = NULL, revoke_at = now(),
               import_status = CASE WHEN import_status = 'running' THEN 'failed' ELSE import_status END,
               import_error = CASE WHEN import_status = 'running' THEN 'disconnected part way'
                                   ELSE import_error END
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// A disconnected connection whose grant is still to be revoked.
pub async fn to_revoke(conn: &mut PgConnection, id: Uuid) -> Result<Option<Connection>> {
    Ok(
        select_connection!("AND id = $1 AND revoke_at IS NOT NULL FOR UPDATE", id)
            .fetch_optional(conn)
            .await?,
    )
}

/// The grant has been revoked (or need not be): forget it.
pub async fn forget_grant(conn: &mut PgConnection, id: Uuid) -> Result<()> {
    sqlx::query!(
        r#"UPDATE accounting_connections SET secret = '\x'::bytea, revoke_at = NULL
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NOT NULL"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Due {
    pub id: Uuid,
    pub workspace_id: WorkspaceId,
    pub at: DateTime<Utc>,
}

/// Connections due a sync, in workspaces still working (the worker's sweep).
pub async fn due(conn: &mut PgConnection, now: DateTime<Utc>, limit: i64) -> Result<Vec<Due>> {
    Ok(sqlx::query_as!(
        Due,
        r#"SELECT c.id, c.workspace_id AS "workspace_id: WorkspaceId", c.next_sync_at AS "at!"
           FROM accounting_connections c JOIN workspaces w ON w.id = c.workspace_id
           WHERE c.next_sync_at <= $1
             AND (c.sync_lease_until IS NULL OR c.sync_lease_until < $1)
             AND w.status IN ('trial', 'active')
           ORDER BY c.next_sync_at LIMIT $2"#,
        now,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// Disconnected connections whose grants are still to be revoked, closed
/// workspaces included (the worker's sweep).
pub async fn revokes_due(conn: &mut PgConnection, limit: i64) -> Result<Vec<Due>> {
    Ok(sqlx::query_as!(
        Due,
        r#"SELECT id, workspace_id AS "workspace_id: WorkspaceId", revoke_at AS "at!"
           FROM accounting_connections WHERE revoke_at <= now()
           ORDER BY revoke_at LIMIT $1"#,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// The live connection's id, if it syncs (for queueing on a change).
pub async fn syncing(conn: &mut PgConnection) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT id FROM accounting_connections
           WHERE workspace_id = app_workspace_id() AND disconnected_at IS NULL
             AND status <> 'reconnect' AND sales_account IS NOT NULL"#
    )
    .fetch_optional(conn)
    .await?)
}

// Links -----------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub kind: String,
    pub local_id: Uuid,
    pub remote_id: Option<String>,
    pub state: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub attempts: i32,
    /// What a `sending` payment or credit was for.
    pub amount_minor: Option<i64>,
}

pub async fn link(
    conn: &mut PgConnection,
    connection: Uuid,
    kind: &str,
    local: Uuid,
) -> Result<Option<Link>> {
    Ok(sqlx::query_as!(
        Link,
        r#"SELECT kind, local_id, remote_id, state, status, error, attempts, amount_minor
           FROM accounting_links
           WHERE workspace_id = app_workspace_id() AND connection_id = $1 AND kind = $2 AND local_id = $3"#,
        connection,
        kind,
        local
    )
    .fetch_optional(conn)
    .await?)
}

/// What in OnHold stands for this in the books.
pub async fn by_remote(
    conn: &mut PgConnection,
    connection: Uuid,
    kind: &str,
    remote: &str,
) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT local_id FROM accounting_links
           WHERE workspace_id = app_workspace_id() AND connection_id = $1 AND kind = $2 AND remote_id = $3
           LIMIT 1"#,
        connection,
        kind,
        remote
    )
    .fetch_optional(conn)
    .await?)
}

/// Records what is in the books for `local`, as sent (`synced`) or come
/// from there (`imported`).
pub async fn save_link(
    conn: &mut PgConnection,
    connection: Uuid,
    kind: &str,
    local: Uuid,
    remote: Option<&str>,
    state: Option<&str>,
    status: &str,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO accounting_links (id, workspace_id, connection_id, kind, local_id, remote_id, state, status)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7)
           ON CONFLICT (connection_id, kind, local_id)
           DO UPDATE SET remote_id = EXCLUDED.remote_id, state = EXCLUDED.state,
                         status = EXCLUDED.status, error = NULL, amount_minor = NULL"#,
        core_domain::ids::new_id(),
        connection,
        kind,
        local,
        remote,
        state,
        status
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// A payment or credit of `amount` is about to be asked for: until its
/// answer is in, it is asked for again just so (same key, same amount).
pub async fn sending(
    conn: &mut PgConnection,
    connection: Uuid,
    kind: &str,
    local: Uuid,
    amount: i64,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO accounting_links (id, workspace_id, connection_id, kind, local_id, state, status, amount_minor)
           VALUES ($1, app_workspace_id(), $2, $3, $4, 'sending', 'synced', $5)
           ON CONFLICT (connection_id, kind, local_id)
           DO UPDATE SET remote_id = NULL, state = 'sending', status = 'synced', error = NULL,
                         amount_minor = EXCLUDED.amount_minor"#,
        core_domain::ids::new_id(),
        connection,
        kind,
        local,
        amount
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The books refused `local`: why, and one more refusal counted. A refused
/// payment or credit was never made, so it is worked out afresh next time.
pub async fn fail_link(
    conn: &mut PgConnection,
    connection: Uuid,
    kind: &str,
    local: Uuid,
    error: &str,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO accounting_links (id, workspace_id, connection_id, kind, local_id, status, error, attempts)
           VALUES ($1, app_workspace_id(), $2, $3, $4, 'failed', left($5, 1000), 1)
           ON CONFLICT (connection_id, kind, local_id)
           DO UPDATE SET status = 'failed', error = EXCLUDED.error,
                         attempts = accounting_links.attempts + 1,
                         state = CASE WHEN accounting_links.state = 'sending' THEN NULL
                                      ELSE accounting_links.state END,
                         amount_minor = NULL"#,
        core_domain::ids::new_id(),
        connection,
        kind,
        local,
        error
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Which tax rate in the books each tax code is.
pub async fn tax_map(conn: &mut PgConnection, connection: Uuid) -> Result<Vec<(Uuid, String)>> {
    Ok(sqlx::query!(
        r#"SELECT local_id, remote_id AS "remote_id!" FROM accounting_links
           WHERE workspace_id = app_workspace_id() AND connection_id = $1 AND kind = 'tax_code'
             AND remote_id IS NOT NULL"#,
        connection
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|r| (r.local_id, r.remote_id))
    .collect())
}

/// Sets (or clears) which tax rate in the books a tax code is.
pub async fn set_tax(
    conn: &mut PgConnection,
    connection: Uuid,
    tax_code: Uuid,
    remote: Option<&str>,
) -> Result<()> {
    save_link(
        conn, connection, "tax_code", tax_code, remote, None, "synced",
    )
    .await
}

#[derive(Debug, Clone)]
pub struct Failure {
    pub kind: String,
    pub local_id: Uuid,
    pub error: Option<String>,
    pub attempts: i32,
    pub updated_at: DateTime<Utc>,
    /// The invoice's or credit note's number, where there is one.
    pub reference: Option<String>,
}

/// What the books refused, newest first.
pub async fn failures(
    conn: &mut PgConnection,
    connection: Uuid,
    limit: i64,
) -> Result<Vec<Failure>> {
    Ok(sqlx::query_as!(
        Failure,
        r#"SELECT l.kind, l.local_id, l.error, l.attempts, l.updated_at,
                  coalesce(i.reference, ai.reference) AS reference
           FROM accounting_links l
           LEFT JOIN invoices i ON l.kind IN ('invoice', 'credit') AND i.id = l.local_id
           LEFT JOIN payment_allocations a ON l.kind = 'allocation' AND a.id = l.local_id
           LEFT JOIN invoices ai ON ai.id = a.invoice_id
           WHERE l.workspace_id = app_workspace_id() AND l.connection_id = $1 AND l.status = 'failed'
           ORDER BY l.updated_at DESC LIMIT $2"#,
        connection,
        limit
    )
    .fetch_all(conn)
    .await?)
}

pub async fn failure_count(conn: &mut PgConnection, connection: Uuid) -> Result<i64> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM accounting_links
           WHERE workspace_id = app_workspace_id() AND connection_id = $1 AND status = 'failed'"#,
        connection
    )
    .fetch_one(conn)
    .await?)
}

// Work ------------------------------------------------------------------------------------
//
// A refused item is tried again once after each full sync (`retry_before`)
// and whenever it changes; new work always comes first.

/// Invoices and credit notes the workspace issued from `start` on that the
/// books do not have as they are now: never sent, voided since, or refused
/// before. Invoices come before the credit notes against them.
pub async fn pending_documents(
    conn: &mut PgConnection,
    connection: Uuid,
    start: NaiveDate,
    retry_before: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT i.id FROM invoices i
           LEFT JOIN accounting_links l
             ON l.connection_id = $1 AND l.kind = 'invoice' AND l.local_id = i.id
           WHERE i.workspace_id = app_workspace_id() AND i.issuer_kind = 'workspace'
             AND NOT i.historical AND i.status <> 'draft' AND i.deleted_at IS NULL
             AND i.issue_date >= $2
             AND (l.id IS NULL
                  OR (l.status = 'failed' AND (l.updated_at < $3 OR i.updated_at > l.updated_at))
                  OR (l.status = 'synced'
                      AND l.state IS DISTINCT FROM CASE WHEN i.status = 'void' THEN 'void' ELSE 'issued' END))
           ORDER BY (l.status IS NOT DISTINCT FROM 'failed'), (i.kind = 'credit_note'), i.issue_date,
                    i.created_at, i.id
           LIMIT $4"#,
        connection,
        start,
        retry_before,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// Issued credit notes in the books not yet set against the invoice they
/// credit (which is in the books too), and ones whose setting against it
/// is still waiting for an answer (voided since, too).
pub async fn pending_credits(
    conn: &mut PgConnection,
    connection: Uuid,
    retry_before: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT c.id FROM invoices c
           JOIN accounting_links lc
             ON lc.connection_id = $1 AND lc.kind = 'invoice' AND lc.local_id = c.id
            AND lc.status = 'synced' AND lc.state = 'issued' AND lc.remote_id IS NOT NULL
           JOIN accounting_links li
             ON li.connection_id = $1 AND li.kind = 'invoice' AND li.local_id = c.credits_invoice_id
            AND li.status = 'synced' AND li.remote_id IS NOT NULL
           LEFT JOIN accounting_links la
             ON la.connection_id = $1 AND la.kind = 'credit' AND la.local_id = c.id
           WHERE c.workspace_id = app_workspace_id() AND c.kind = 'credit_note' AND c.deleted_at IS NULL
             AND (la.state IS NOT DISTINCT FROM 'sending'
                  OR (c.status <> 'void'
                      AND (la.id IS NULL
                           OR (la.status = 'failed'
                               AND (la.updated_at < $2 OR c.updated_at > la.updated_at)))))
           ORDER BY (la.status IS NOT DISTINCT FROM 'failed'), c.issue_date, c.id
           LIMIT $3"#,
        connection,
        retry_before,
        limit
    )
    .fetch_all(conn)
    .await?)
}

#[derive(Debug, Clone)]
pub struct AllocationWork {
    pub id: Uuid,
    pub invoice_id: Uuid,
    /// The invoice in the books.
    pub remote_invoice: String,
    pub amount_minor: i64,
    pub currency: String,
    pub paid_on: NaiveDate,
    pub reference: Option<String>,
    pub removed: bool,
    /// What the books have for it, when sent.
    pub remote_id: Option<String>,
    /// What OnHold last did with it in the books (`live`, `removed`,
    /// `sending`).
    pub state: Option<String>,
    /// What a `sending` payment was for.
    pub sent_minor: Option<i64>,
    pub attempts: i32,
}

/// Payments' allocations to invoices in the books that the books do not
/// have as they are now: new ones (`send_new`), ones still waiting for an
/// answer, ones taken back (a payment voided), or ones refused before. Not
/// those that came from the books.
pub async fn pending_allocations(
    conn: &mut PgConnection,
    connection: Uuid,
    send_new: bool,
    retry_before: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<AllocationWork>> {
    // Two halves, each on an index: what stands (from the invoices in the
    // books), and what was taken back (from what was sent).
    Ok(sqlx::query_as!(
        AllocationWork,
        r#"SELECT w.id AS "id!", w.invoice_id AS "invoice_id!", w.remote_invoice AS "remote_invoice!",
                  w.amount_minor AS "amount_minor!", w.currency AS "currency!", w.paid_on AS "paid_on!",
                  w.reference, w.removed AS "removed!", w.remote_id, w.state, w.sent_minor,
                  w.attempts AS "attempts!"
           FROM (
             SELECT a.id, a.invoice_id, li.remote_id AS remote_invoice, a.amount_minor, p.currency,
                    p.paid_on, p.reference, false AS removed, la.remote_id, la.state,
                    la.amount_minor AS sent_minor, coalesce(la.attempts, 0) AS attempts,
                    (la.status IS NOT DISTINCT FROM 'failed') AS refused, a.created_at
             FROM accounting_links li
             JOIN payment_allocations a ON a.invoice_id = li.local_id AND a.removed_at IS NULL
             JOIN payments p ON p.id = a.payment_id
             LEFT JOIN accounting_links la
               ON la.connection_id = $1 AND la.kind = 'allocation' AND la.local_id = a.id
             WHERE li.workspace_id = app_workspace_id() AND li.connection_id = $1
               AND li.kind = 'invoice' AND li.status = 'synced' AND li.remote_id IS NOT NULL
               AND a.workspace_id = app_workspace_id() AND p.kind = 'payment'
               AND (la.state IS NOT DISTINCT FROM 'sending'
                    OR ($2 AND (la.id IS NULL
                                OR (la.status = 'failed' AND la.updated_at < $3))))
             UNION ALL
             SELECT a.id, a.invoice_id, li.remote_id, a.amount_minor, p.currency, p.paid_on,
                    p.reference, true, la.remote_id, la.state, la.amount_minor, la.attempts,
                    (la.status = 'failed'), a.created_at
             FROM accounting_links la
             JOIN payment_allocations a ON a.id = la.local_id AND a.removed_at IS NOT NULL
             JOIN payments p ON p.id = a.payment_id
             JOIN accounting_links li
               ON li.connection_id = $1 AND li.kind = 'invoice' AND li.local_id = a.invoice_id
              AND li.remote_id IS NOT NULL
             WHERE la.workspace_id = app_workspace_id() AND la.connection_id = $1
               AND la.kind = 'allocation' AND la.status <> 'imported'
               AND (la.state IN ('live', 'sending') OR la.status = 'failed')
               AND (la.status = 'synced' OR la.updated_at < $3 OR a.removed_at > la.updated_at)
           ) w
           ORDER BY w.refused, w.created_at, w.id
           LIMIT $4"#,
        connection,
        send_new,
        retry_before,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// Whether a payment against this invoice is still on its way to the books
/// or being taken back there, so what the books show is not yet final.
pub async fn allocation_work_pending(
    conn: &mut PgConnection,
    connection: Uuid,
    invoice: Uuid,
) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1 FROM payment_allocations a
             JOIN accounting_links la
               ON la.connection_id = $1 AND la.kind = 'allocation' AND la.local_id = a.id
             WHERE a.workspace_id = app_workspace_id() AND a.invoice_id = $2
               AND la.status <> 'imported'
               AND (la.state = 'sending' OR (a.removed_at IS NOT NULL AND la.state = 'live'))
           ) AS "pending!""#,
        connection,
        invoice
    )
    .fetch_one(conn)
    .await?)
}

#[derive(Debug, Clone)]
pub struct OpenInvoice {
    pub id: Uuid,
    pub remote_id: String,
    pub currency: String,
    pub total_minor: i64,
    pub paid_minor: i64,
    pub credited_minor: i64,
}

/// Invoices in the books (sent from here or imported from there) that
/// OnHold still shows as owing, a page at a time after `after`.
pub async fn open_invoices(
    conn: &mut PgConnection,
    connection: Uuid,
    after: Uuid,
    limit: i64,
) -> Result<Vec<OpenInvoice>> {
    Ok(sqlx::query_as!(
        OpenInvoice,
        r#"SELECT i.id, l.remote_id AS "remote_id!", i.currency AS "currency!", i.total_minor,
                  i.paid_minor, i.credited_minor
           FROM invoices i
           JOIN accounting_links l
             ON l.connection_id = $1 AND l.kind = 'invoice' AND l.local_id = i.id
            AND l.status IN ('synced', 'imported') AND l.remote_id IS NOT NULL
           WHERE i.workspace_id = app_workspace_id() AND i.kind = 'invoice'
             AND i.status IN ('issued', 'part_paid', 'overdue') AND i.deleted_at IS NULL
             AND i.id > $2
           ORDER BY i.id LIMIT $3"#,
        connection,
        after,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// An invoice of this number from the workspace, for an import that meets
/// one already in OnHold.
pub async fn invoice_by_reference(
    conn: &mut PgConnection,
    reference: &str,
) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT id FROM invoices
           WHERE workspace_id = app_workspace_id() AND kind = 'invoice' AND issuer_kind = 'workspace'
             AND deleted_at IS NULL AND lower(reference) = lower($1)
           ORDER BY (status = 'void'), created_at LIMIT 1"#,
        reference
    )
    .fetch_optional(conn)
    .await?)
}

/// The organisation of this name, if there is exactly one.
pub async fn organisation_named(conn: &mut PgConnection, name: &str) -> Result<Option<Uuid>> {
    let ids = sqlx::query_scalar!(
        r#"SELECT id FROM organisations
           WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
             AND lower(btrim(name)) = lower(btrim($1))
           LIMIT 2"#,
        name
    )
    .fetch_all(conn)
    .await?;
    Ok((ids.len() == 1).then(|| ids[0]))
}
