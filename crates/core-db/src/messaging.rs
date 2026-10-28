//! A workspace's SMS and WhatsApp numbers, who asked for no more messages
//! on them, and the inbox threads and messages on them.

use chrono::{DateTime, Utc};
use core_domain::{UserId, WorkspaceId};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Channel {
    pub id: Uuid,
    pub kind: String,
    pub provider: String,
    pub sender: String,
    pub account_id: String,
    pub display_name: Option<String>,
    pub secret: Vec<u8>,
    pub hook_token: String,
    pub status: String,
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub connected_by: Option<UserId>,
    pub connected_at: DateTime<Utc>,
    pub disconnected_at: Option<DateTime<Utc>>,
}

macro_rules! select_channel {
    ($tail:literal $(, $arg:expr)*) => {
        sqlx::query_as!(
            Channel,
            r#"SELECT id, kind, provider, sender, account_id, display_name, secret, hook_token, status,
                      last_error, last_error_at, connected_by AS "connected_by: UserId", connected_at,
                      disconnected_at
               FROM messaging_channels
               WHERE workspace_id = app_workspace_id() "# + $tail
            $(, $arg)*
        )
    };
}

/// The workspace's live numbers.
pub async fn live(conn: &mut PgConnection) -> Result<Vec<Channel>> {
    Ok(select_channel!("AND disconnected_at IS NULL ORDER BY kind")
        .fetch_all(conn)
        .await?)
}

/// The workspace's live number on `kind` (`sms`, `whatsapp`).
pub async fn live_kind(conn: &mut PgConnection, kind: &str) -> Result<Option<Channel>> {
    Ok(
        select_channel!("AND disconnected_at IS NULL AND kind = $1", kind)
            .fetch_optional(conn)
            .await?,
    )
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Channel>> {
    Ok(select_channel!("AND id = $1", id)
        .fetch_optional(conn)
        .await?)
}

pub struct NewChannel<'a> {
    pub id: Uuid,
    pub kind: &'a str,
    pub provider: &'a str,
    pub sender: &'a str,
    pub account_id: &'a str,
    pub display_name: Option<&'a str>,
    pub secret: &'a [u8],
    pub hook_token: &'a str,
    pub connected_by: Option<UserId>,
}

pub async fn insert(conn: &mut PgConnection, c: NewChannel<'_>) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO messaging_channels (id, workspace_id, kind, provider, sender, account_id, display_name,
                                           secret, hook_token, connected_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, left($6, 200), $7, $8, $9)"#,
        c.id,
        c.kind,
        c.provider,
        c.sender,
        c.account_id,
        c.display_name,
        c.secret,
        c.hook_token,
        c.connected_by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Stops using a number and forgets its credentials. Its threads stay.
pub async fn disconnect(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE messaging_channels SET disconnected_at = now(), secret = '\x'::bytea
           WHERE workspace_id = app_workspace_id() AND id = $1 AND disconnected_at IS NULL"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// How sending went: `None` is healthy again.
pub async fn set_health(conn: &mut PgConnection, id: Uuid, error: Option<&str>) -> Result<()> {
    sqlx::query!(
        r#"UPDATE messaging_channels
           SET status = CASE WHEN $2::text IS NULL THEN 'active' ELSE 'failing' END,
               last_error = CASE WHEN $2::text IS NULL THEN last_error ELSE left($2, 1000) END,
               last_error_at = CASE WHEN $2::text IS NULL THEN last_error_at ELSE now() END
           WHERE workspace_id = app_workspace_id() AND id = $1
             AND ($2::text IS NOT NULL OR status <> 'active')"#,
        id,
        error
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The live number a provider's request is for, by the address it came
/// to. Needs no workspace.
pub async fn for_hook(conn: &mut PgConnection, token: &str) -> Result<Option<(WorkspaceId, Uuid)>> {
    let row = sqlx::query!(
        r#"SELECT workspace_id AS "workspace_id!: WorkspaceId", id AS "id!"
           FROM messaging_channel_for_hook($1)"#,
        token
    )
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|r| (r.workspace_id, r.id)))
}

// Opting out ------------------------------------------------------------------------------

/// Records STOP (`true`) or START (`false`) from a number.
pub async fn set_opt_out(
    conn: &mut PgConnection,
    kind: &str,
    phone: &str,
    out: bool,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO messaging_opt_outs (workspace_id, kind, phone, opted_out)
           VALUES (app_workspace_id(), $1, $2, $3)
           ON CONFLICT (workspace_id, kind, phone)
           DO UPDATE SET opted_out = EXCLUDED.opted_out, changed_at = now()"#,
        kind,
        phone,
        out
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn opted_out(conn: &mut PgConnection, kind: &str, phone: &str) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT opted_out FROM messaging_opt_outs
           WHERE workspace_id = app_workspace_id() AND kind = $1 AND phone = $2"#,
        kind,
        phone
    )
    .fetch_optional(conn)
    .await?
    .unwrap_or(false))
}

// Threads and messages --------------------------------------------------------------------

/// Holds the conversation with `phone` on this number until the
/// transaction ends, so two writers never both start it.
pub async fn lock_number(conn: &mut PgConnection, channel: Uuid, phone: &str) -> Result<()> {
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        format!("messaging:{channel}:{phone}")
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The workspace's country, for numbers written without a country code.
pub async fn country(conn: &mut PgConnection) -> Result<String> {
    Ok(sqlx::query_scalar!(
        r#"SELECT trim(country) AS "country!" FROM workspaces WHERE id = app_workspace_id()"#
    )
    .fetch_one(conn)
    .await?)
}

/// The thread with `phone` on this number, if there is one.
pub async fn thread_for(
    conn: &mut PgConnection,
    channel: Uuid,
    phone: &str,
) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT id FROM threads
           WHERE workspace_id = app_workspace_id() AND messaging_channel_id = $1 AND external_id = $2"#,
        channel,
        phone
    )
    .fetch_optional(conn)
    .await?)
}

pub struct NewThread<'a> {
    pub id: Uuid,
    pub kind: &'a str,
    pub channel: Uuid,
    pub phone: &'a str,
    pub contact_id: Option<Uuid>,
    pub organisation_id: Option<Uuid>,
    pub enquiry_id: Option<Uuid>,
    pub booking_id: Option<Uuid>,
    pub assigned_to: Option<Uuid>,
    pub unread: bool,
    pub at: DateTime<Utc>,
}

/// Starts the thread with `phone` on this number. `false` when one was
/// started meanwhile.
pub async fn insert_thread(conn: &mut PgConnection, t: NewThread<'_>) -> Result<bool> {
    let done = sqlx::query!(
        r#"INSERT INTO threads (id, workspace_id, channel, messaging_channel_id, external_id, contact_id,
                                organisation_id, enquiry_id, booking_id, assigned_to, unread, last_message_at,
                                created_at)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, now())
           ON CONFLICT DO NOTHING"#,
        t.id,
        t.kind,
        t.channel,
        t.phone,
        t.contact_id,
        t.organisation_id,
        t.enquiry_id,
        t.booking_id,
        t.assigned_to,
        t.unread,
        t.at
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub struct NewText<'a> {
    pub id: Uuid,
    pub thread_id: Uuid,
    pub channel: Uuid,
    pub direction: &'a str,
    pub external_id: Option<&'a str>,
    /// Theirs, for one that came in.
    pub from: Option<&'a str>,
    pub from_name: Option<&'a str>,
    /// Theirs, for one sent from here.
    pub to: Option<&'a str>,
    /// A WhatsApp template's name, for one sent from a template.
    pub subject: Option<&'a str>,
    pub body: &'a str,
    pub sent_at: DateTime<Utc>,
    pub status: &'a str,
    /// Sent by a rule.
    pub automatic: bool,
    pub author_id: Option<UserId>,
    pub template_key: Option<&'a str>,
}

/// Adds a text message. `false` when the provider's message was in already.
pub async fn insert_text(conn: &mut PgConnection, m: NewText<'_>) -> Result<bool> {
    let to: Vec<String> = m.to.map(str::to_owned).into_iter().collect();
    let done = sqlx::query!(
        r#"INSERT INTO thread_messages (id, workspace_id, thread_id, direction, messaging_channel_id, external_id,
                                        from_address, from_name, to_addresses, subject, body_text, sent_at, status,
                                        automatic, author_id, template_key)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, left($7, 200), $8, $9, left($10, 200000), $11,
                   $12, $13, $14, $15)
           ON CONFLICT DO NOTHING"#,
        m.id,
        m.thread_id,
        m.direction,
        m.channel,
        m.external_id,
        m.from,
        m.from_name,
        &to,
        m.subject,
        m.body,
        m.sent_at,
        m.status,
        m.automatic,
        m.author_id.map(|a| a.0),
        m.template_key
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// A message sent from this number, by the provider's id for it.
pub async fn sent_message(
    conn: &mut PgConnection,
    channel: Uuid,
    external_id: &str,
) -> Result<Option<(Uuid, Uuid)>> {
    let row = sqlx::query!(
        r#"SELECT id, thread_id FROM thread_messages
           WHERE workspace_id = app_workspace_id() AND messaging_channel_id = $1 AND external_id = $2
             AND direction = 'outbound'"#,
        channel,
        external_id
    )
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|r| (r.id, r.thread_id)))
}

/// What became of a message sent from here, as the provider reported it
/// later: only a failure changes it.
pub async fn failed_later(conn: &mut PgConnection, id: Uuid, error: &str) -> Result<()> {
    sqlx::query!(
        r#"UPDATE thread_messages SET status = 'failed', error = left($2, 1000)
           WHERE workspace_id = app_workspace_id() AND id = $1 AND direction = 'outbound'"#,
        id,
        error
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The messaging number a message was written on, for sending it.
pub async fn message_channel(conn: &mut PgConnection, message: Uuid) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT messaging_channel_id FROM thread_messages
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        message
    )
    .fetch_optional(conn)
    .await?
    .flatten())
}

/// The contact with this number, matched on its last nine digits: the
/// oldest when several have it. With their organisation.
pub async fn contact_with_phone(
    conn: &mut PgConnection,
    key: &str,
) -> Result<Option<(Uuid, Option<Uuid>)>> {
    let row = sqlx::query!(
        r#"SELECT id, organisation_id FROM contacts
           WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL AND phone_keys @> ARRAY[$1::text]
           ORDER BY created_at, id LIMIT 1"#,
        key
    )
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|r| (r.id, r.organisation_id)))
}
