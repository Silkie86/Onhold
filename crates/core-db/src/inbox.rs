//! The unified inbox: threads and their messages, and the files that came
//! with them. A thread in a mailbox that is not shared is seen only by the
//! mailbox's owner; every query takes the viewer for that (`None` sees all:
//! the worker, and services acting for the system).

use chrono::{DateTime, Utc};
use core_domain::{UserId, WorkspaceId};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Thread {
    pub id: Uuid,
    pub channel: String,
    pub integration_id: Option<Uuid>,
    pub external_id: Option<String>,
    pub subject: Option<String>,
    pub contact_id: Option<Uuid>,
    pub organisation_id: Option<Uuid>,
    pub enquiry_id: Option<Uuid>,
    pub booking_id: Option<Uuid>,
    pub status: String,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub assigned_to: Option<Uuid>,
    pub unread: bool,
    pub message_count: i32,
    pub participants: Vec<String>,
    pub preview: Option<String>,
    pub last_message_at: DateTime<Utc>,
    pub last_inbound_at: Option<DateTime<Utc>>,
    pub last_outbound_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

/// Which threads to list.
#[derive(Debug, Default)]
pub struct Filter<'a> {
    /// `None` sees every mailbox; else the viewer's own and shared ones.
    pub viewer: Option<UserId>,
    pub status: Option<&'a str>,
    pub integration_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub organisation_id: Option<Uuid>,
    pub enquiry_id: Option<Uuid>,
    pub booking_id: Option<Uuid>,
    pub assigned_to: Option<Uuid>,
    /// Only threads linked to no contact, enquiry or booking.
    pub unlinked: bool,
    pub unread: bool,
    /// Matched against the subject and the people in it.
    pub q: Option<&'a str>,
    pub offset: i64,
    pub limit: i64,
}

/// Newest activity first.
pub async fn list(conn: &mut PgConnection, f: Filter<'_>) -> Result<(Vec<Thread>, i64)> {
    let rows = sqlx::query!(
        r#"SELECT t.id, t.channel, t.integration_id, t.external_id, t.subject, t.contact_id, t.organisation_id,
                  t.enquiry_id, t.booking_id, t.status, t.snoozed_until, t.assigned_to, t.unread,
                  t.message_count, t.participants, t.preview, t.last_message_at, t.last_inbound_at,
                  t.last_outbound_at, t.created_at, t.version, count(*) OVER () AS "total!"
           FROM threads t
           LEFT JOIN integrations i ON i.workspace_id = t.workspace_id AND i.id = t.integration_id
           WHERE t.workspace_id = app_workspace_id()
             AND ($1::uuid IS NULL OR t.integration_id IS NULL OR i.shared OR i.owner_id = $1)
             AND ($2::text IS NULL OR t.status = $2)
             AND ($3::uuid IS NULL OR t.integration_id = $3)
             AND ($4::uuid IS NULL OR t.contact_id = $4)
             AND ($5::uuid IS NULL OR t.organisation_id = $5)
             AND ($6::uuid IS NULL OR t.enquiry_id = $6)
             AND ($7::uuid IS NULL OR t.booking_id = $7)
             AND ($8::uuid IS NULL OR t.assigned_to = $8)
             AND (NOT $9 OR (t.contact_id IS NULL AND t.enquiry_id IS NULL AND t.booking_id IS NULL))
             AND (NOT $10 OR t.unread)
             AND ($11::text IS NULL OR t.search_text LIKE '%' || $11 || '%')
           ORDER BY t.last_message_at DESC, t.id DESC
           OFFSET $12 LIMIT $13"#,
        f.viewer.map(|v| v.0),
        f.status,
        f.integration_id,
        f.contact_id,
        f.organisation_id,
        f.enquiry_id,
        f.booking_id,
        f.assigned_to,
        f.unlinked,
        f.unread,
        f.q.map(|q| q.to_lowercase().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")),
        f.offset,
        f.limit
    )
    .fetch_all(conn)
    .await?;
    let total = rows.first().map_or(0, |r| r.total);
    let items = rows
        .into_iter()
        .map(|r| Thread {
            id: r.id,
            channel: r.channel,
            integration_id: r.integration_id,
            external_id: r.external_id,
            subject: r.subject,
            contact_id: r.contact_id,
            organisation_id: r.organisation_id,
            enquiry_id: r.enquiry_id,
            booking_id: r.booking_id,
            status: r.status,
            snoozed_until: r.snoozed_until,
            assigned_to: r.assigned_to,
            unread: r.unread,
            message_count: r.message_count,
            participants: r.participants,
            preview: r.preview,
            last_message_at: r.last_message_at,
            last_inbound_at: r.last_inbound_at,
            last_outbound_at: r.last_outbound_at,
            created_at: r.created_at,
            version: r.version,
        })
        .collect();
    Ok((items, total))
}

/// One thread the viewer may see.
pub async fn get(
    conn: &mut PgConnection,
    id: Uuid,
    viewer: Option<UserId>,
) -> Result<Option<Thread>> {
    Ok(sqlx::query_as!(
        Thread,
        r#"SELECT t.id, t.channel, t.integration_id, t.external_id, t.subject, t.contact_id, t.organisation_id,
                  t.enquiry_id, t.booking_id, t.status, t.snoozed_until, t.assigned_to, t.unread,
                  t.message_count, t.participants, t.preview, t.last_message_at, t.last_inbound_at,
                  t.last_outbound_at, t.created_at, t.version
           FROM threads t
           LEFT JOIN integrations i ON i.workspace_id = t.workspace_id AND i.id = t.integration_id
           WHERE t.workspace_id = app_workspace_id() AND t.id = $1
             AND ($2::uuid IS NULL OR t.integration_id IS NULL OR i.shared OR i.owner_id = $2)"#,
        id,
        viewer.map(|v| v.0)
    )
    .fetch_optional(conn)
    .await?)
}

/// Locks a thread for an update in this transaction; returns its version.
pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar!(
        "SELECT version FROM threads WHERE workspace_id = app_workspace_id() AND id = $1 FOR UPDATE",
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// The thread a provider conversation was read into.
pub async fn by_external(
    conn: &mut PgConnection,
    integration_id: Uuid,
    external_id: &str,
) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT id FROM threads
           WHERE workspace_id = app_workspace_id() AND integration_id = $1 AND external_id = $2"#,
        integration_id,
        external_id
    )
    .fetch_optional(conn)
    .await?)
}

/// The thread holding any of these messages (by Message-ID): one in this
/// mailbox, or one that belongs to no mailbox (written through OnHold's
/// own sender). Never a thread of another mailbox, which may be private.
pub async fn by_message_ids(
    conn: &mut PgConnection,
    integration_id: Uuid,
    message_ids: &[String],
) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT m.thread_id FROM thread_messages m
           JOIN threads t ON t.workspace_id = m.workspace_id AND t.id = m.thread_id
           WHERE m.workspace_id = app_workspace_id() AND m.message_id = ANY($2)
             AND (m.integration_id = $1 OR m.integration_id IS NULL)
             AND (t.integration_id = $1 OR t.integration_id IS NULL)
           ORDER BY t.integration_id IS NULL, m.sent_at DESC LIMIT 1"#,
        integration_id,
        message_ids
    )
    .fetch_optional(conn)
    .await?)
}

pub struct NewThread<'a> {
    pub id: Uuid,
    pub integration_id: Option<Uuid>,
    pub external_id: Option<&'a str>,
    pub subject: Option<&'a str>,
    pub contact_id: Option<Uuid>,
    pub organisation_id: Option<Uuid>,
    pub enquiry_id: Option<Uuid>,
    pub booking_id: Option<Uuid>,
    pub assigned_to: Option<Uuid>,
    pub unread: bool,
    pub at: DateTime<Utc>,
}

pub async fn insert(conn: &mut PgConnection, t: NewThread<'_>) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO threads (id, workspace_id, integration_id, external_id, subject, contact_id, organisation_id,
                                enquiry_id, booking_id, assigned_to, unread, last_message_at, created_at)
           VALUES ($1, app_workspace_id(), $2, $3, left($4, 1000), $5, $6, $7, $8, $9, $10, $11, now())"#,
        t.id,
        t.integration_id,
        t.external_id,
        t.subject,
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
    Ok(())
}

/// A thread first written through OnHold's own sender moves into the
/// mailbox the conversation continued in.
pub async fn adopt(
    conn: &mut PgConnection,
    id: Uuid,
    integration_id: Uuid,
    external_id: Option<&str>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE threads SET integration_id = $2, external_id = coalesce(external_id, $3)
           WHERE workspace_id = app_workspace_id() AND id = $1 AND integration_id IS NULL"#,
        id,
        integration_id,
        external_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The provider's own thread id for a thread sent from its mailbox (first,
/// or again after the provider's conversation was deleted), so replies
/// read later land in the same thread.
pub async fn adopt_external(
    conn: &mut PgConnection,
    id: Uuid,
    integration_id: Uuid,
    external_id: &str,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE threads SET external_id = $3
           WHERE workspace_id = app_workspace_id() AND id = $1 AND integration_id = $2
             AND external_id IS DISTINCT FROM $3"#,
        id,
        integration_id,
        external_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub struct Links {
    pub contact_id: Option<Uuid>,
    pub organisation_id: Option<Uuid>,
    pub enquiry_id: Option<Uuid>,
    pub booking_id: Option<Uuid>,
}

pub async fn set_links(conn: &mut PgConnection, id: Uuid, l: &Links) -> Result<()> {
    sqlx::query!(
        r#"UPDATE threads SET contact_id = $2, organisation_id = $3, enquiry_id = $4, booking_id = $5
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        l.contact_id,
        l.organisation_id,
        l.enquiry_id,
        l.booking_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub struct State {
    pub status: String,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub assigned_to: Option<Uuid>,
    pub unread: bool,
}

pub async fn set_state(conn: &mut PgConnection, id: Uuid, s: &State) -> Result<()> {
    sqlx::query!(
        r#"UPDATE threads SET status = $2, snoozed_until = $3, assigned_to = $4, unread = $5
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        s.status,
        s.snoozed_until,
        s.assigned_to,
        s.unread
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Brings a thread's summary up to date after a message was added. An
/// inbound message from a person reopens a snoozed or resolved thread and
/// marks it unread.
pub async fn refresh(conn: &mut PgConnection, id: Uuid, reopen: bool) -> Result<()> {
    sqlx::query!(
        r#"UPDATE threads t SET
               message_count = s.n,
               last_message_at = s.last_at,
               last_inbound_at = s.last_in,
               last_outbound_at = s.last_out,
               preview = s.preview,
               participants = s.participants,
               unread = t.unread OR $2,
               status = CASE WHEN $2 THEN 'open' ELSE t.status END,
               snoozed_until = CASE WHEN $2 THEN NULL ELSE t.snoozed_until END
           FROM (
               SELECT count(*)::int AS n,
                      max(sent_at) AS last_at,
                      max(sent_at) FILTER (WHERE direction = 'inbound') AS last_in,
                      max(sent_at) FILTER (WHERE direction = 'outbound' AND status <> 'failed') AS last_out,
                      -- The latest message a person wrote, else the latest of all.
                      (SELECT btrim(left(regexp_replace(m2.body_text, '\s+', ' ', 'g'), 300)) FROM thread_messages m2
                       WHERE m2.thread_id = $1 AND m2.workspace_id = app_workspace_id()
                       ORDER BY m2.automatic, m2.sent_at DESC, m2.id DESC LIMIT 1) AS preview,
                      (SELECT coalesce((array_agg(DISTINCT a ORDER BY a))[1:100], '{}') FROM (
                           SELECT unnest(ARRAY[m3.from_address] || m3.to_addresses || m3.cc_addresses) AS a
                           FROM thread_messages m3
                           WHERE m3.thread_id = $1 AND m3.workspace_id = app_workspace_id()
                       ) x
                       WHERE a IS NOT NULL
                         AND a NOT IN (SELECT coalesce(i.shared_mailbox, i.account) FROM integrations i
                                       WHERE i.workspace_id = app_workspace_id())
                      ) AS participants
               FROM thread_messages WHERE thread_id = $1 AND workspace_id = app_workspace_id()
           ) s
           WHERE t.workspace_id = app_workspace_id() AND t.id = $1"#,
        id,
        reopen
    )
    .execute(conn)
    .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Message {
    pub id: Uuid,
    pub thread_id: Uuid,
    pub direction: String,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub from_address: Option<String>,
    pub from_name: Option<String>,
    pub reply_to: Option<String>,
    pub to_addresses: Vec<String>,
    pub cc_addresses: Vec<String>,
    pub subject: Option<String>,
    pub body_text: String,
    pub body_html: Option<String>,
    pub sent_at: DateTime<Utc>,
    pub status: String,
    pub error: Option<String>,
    pub automatic: bool,
    pub bulk: bool,
    pub author_id: Option<Uuid>,
    pub template_key: Option<String>,
}

pub struct NewMessage<'a> {
    pub id: Uuid,
    pub thread_id: Uuid,
    pub direction: &'a str,
    pub integration_id: Option<Uuid>,
    pub external_id: Option<&'a str>,
    pub message_id: Option<&'a str>,
    pub in_reply_to: Option<&'a str>,
    pub references: &'a [String],
    pub from_address: Option<&'a str>,
    pub from_name: Option<&'a str>,
    pub reply_to: Option<&'a str>,
    pub to_addresses: &'a [String],
    pub cc_addresses: &'a [String],
    pub subject: Option<&'a str>,
    pub body_text: &'a str,
    pub body_html: Option<&'a str>,
    pub sent_at: DateTime<Utc>,
    pub status: &'a str,
    pub automatic: bool,
    pub bulk: bool,
    pub author_id: Option<UserId>,
    pub template_key: Option<&'a str>,
}

/// Adds a message. `false` when the mailbox already had it (read before).
pub async fn insert_message(conn: &mut PgConnection, m: NewMessage<'_>) -> Result<bool> {
    let done = sqlx::query!(
        r#"INSERT INTO thread_messages (id, workspace_id, thread_id, direction, integration_id, external_id,
                                        message_id, in_reply_to, "references", from_address, from_name,
                                        to_addresses, cc_addresses, subject, body_text, body_html, sent_at,
                                        status, automatic, bulk, author_id, template_key, reply_to)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, left($10, 200), $11, $12,
                   left($13, 1000), left($14, 200000), $15, $16, $17, $18, $19, $20, $21, $22)
           ON CONFLICT DO NOTHING"#,
        m.id,
        m.thread_id,
        m.direction,
        m.integration_id,
        m.external_id,
        m.message_id,
        m.in_reply_to,
        m.references,
        m.from_address,
        m.from_name,
        m.to_addresses,
        m.cc_addresses,
        m.subject,
        m.body_text,
        m.body_html,
        m.sent_at,
        m.status,
        m.automatic,
        m.bulk,
        m.author_id.map(|a| a.0),
        m.template_key,
        m.reply_to
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Whether this mailbox already holds a message, by its provider id or
/// (for one sent from here and now read back) by its Message-ID.
pub async fn known(
    conn: &mut PgConnection,
    integration_id: Uuid,
    external_id: &str,
    message_id: Option<&str>,
) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT id FROM thread_messages
           WHERE workspace_id = app_workspace_id() AND integration_id = $1
             AND (external_id = $2 OR ($3::text IS NOT NULL AND message_id = $3))
           LIMIT 1"#,
        integration_id,
        external_id,
        message_id
    )
    .fetch_optional(conn)
    .await?)
}

/// Gives a message the provider's current id when its copy is read (again):
/// a sent one read back, or one the server renumbered. A message written
/// here whose copy is in the mailbox was sent, whatever was recorded.
pub async fn set_external(conn: &mut PgConnection, id: Uuid, external_id: &str) -> Result<()> {
    sqlx::query!(
        r#"UPDATE thread_messages SET external_id = $2,
               status = CASE WHEN direction = 'outbound' THEN 'sent' ELSE status END,
               error = CASE WHEN direction = 'outbound' THEN NULL ELSE error END
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        external_id
    )
    .execute(conn)
    .await?;
    Ok(())
}

macro_rules! select_message {
    ($tail:literal $(, $arg:expr)*) => {
        sqlx::query_as!(
            Message,
            r#"SELECT id, thread_id, direction, message_id, in_reply_to, "references" AS "references!",
                      from_address, from_name, reply_to, to_addresses, cc_addresses, subject, body_text, body_html,
                      sent_at, status, error, automatic, bulk, author_id, template_key
               FROM thread_messages WHERE workspace_id = app_workspace_id() AND "# + $tail
            $(, $arg)*
        )
    };
}

/// A thread's messages, oldest first.
pub async fn messages(conn: &mut PgConnection, thread_id: Uuid) -> Result<Vec<Message>> {
    Ok(
        select_message!("thread_id = $1 ORDER BY sent_at, id", thread_id)
            .fetch_all(conn)
            .await?,
    )
}

pub async fn message(conn: &mut PgConnection, id: Uuid) -> Result<Option<Message>> {
    Ok(select_message!("id = $1", id).fetch_optional(conn).await?)
}

/// Locks an outgoing message while it is sent.
pub async fn lock_message(conn: &mut PgConnection, id: Uuid) -> Result<Option<Message>> {
    Ok(select_message!("id = $1 FOR UPDATE", id)
        .fetch_optional(conn)
        .await?)
}

/// Marks a queued message as being handed to the provider. `false` when it
/// is no longer queued (sent, failed, or a send already under way).
pub async fn start_sending(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE thread_messages SET status = 'sending'
           WHERE workspace_id = app_workspace_id() AND id = $1 AND status = 'queued'"#,
        id
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// What became of an outgoing message.
pub async fn set_outcome(
    conn: &mut PgConnection,
    id: Uuid,
    status: &str,
    external_id: Option<&str>,
    error: Option<&str>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE thread_messages
           SET status = $2, external_id = coalesce(external_id, $3), error = left($4, 1000),
               sent_at = CASE WHEN $2 = 'sent' THEN now() ELSE sent_at END
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        status,
        external_id,
        error
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// A file listed with a message.
#[derive(Debug, Clone)]
pub struct MessageFile {
    pub id: Uuid,
    pub message_id: Uuid,
    /// Sent from OnHold: the stored file.
    pub file_id: Option<Uuid>,
    /// Read from a mailbox: which attachment of the message it is.
    pub part: Option<i32>,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
}

/// A stored file sent with a message.
pub async fn attach_file(conn: &mut PgConnection, message_id: Uuid, file_id: Uuid) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO thread_message_files (id, workspace_id, message_id, file_id, filename, content_type, size_bytes)
           SELECT $3, app_workspace_id(), $1, f.id, f.filename, f.content_type, f.size_bytes
           FROM files f WHERE f.workspace_id = app_workspace_id() AND f.id = $2"#,
        message_id,
        file_id,
        core_domain::ids::new_id()
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// An attachment left in the mailbox: only its name and size are kept.
pub async fn attach_part(
    conn: &mut PgConnection,
    message_id: Uuid,
    part: i32,
    filename: &str,
    content_type: &str,
    size_bytes: i64,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO thread_message_files (id, workspace_id, message_id, part, filename, content_type, size_bytes)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6)"#,
        core_domain::ids::new_id(),
        message_id,
        part,
        filename,
        content_type,
        size_bytes
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// The files of a thread's messages.
pub async fn files(conn: &mut PgConnection, thread_id: Uuid) -> Result<Vec<MessageFile>> {
    Ok(sqlx::query_as!(
        MessageFile,
        r#"SELECT mf.id, mf.message_id, mf.file_id, mf.part, mf.filename, mf.content_type, mf.size_bytes
           FROM thread_message_files mf
           JOIN thread_messages m ON m.workspace_id = mf.workspace_id AND m.id = mf.message_id
           WHERE mf.workspace_id = app_workspace_id() AND m.thread_id = $1
           ORDER BY m.sent_at, mf.part NULLS FIRST, mf.filename"#,
        thread_id
    )
    .fetch_all(conn)
    .await?)
}

/// One file of a thread, with where its message came from.
#[derive(Debug, Clone)]
pub struct ThreadFile {
    pub file: MessageFile,
    pub integration_id: Option<Uuid>,
    pub external_id: Option<String>,
}

pub async fn file(
    conn: &mut PgConnection,
    thread_id: Uuid,
    id: Uuid,
) -> Result<Option<ThreadFile>> {
    let row = sqlx::query!(
        r#"SELECT mf.id, mf.message_id, mf.file_id, mf.part, mf.filename, mf.content_type, mf.size_bytes,
                  m.integration_id, m.external_id
           FROM thread_message_files mf
           JOIN thread_messages m ON m.workspace_id = mf.workspace_id AND m.id = mf.message_id
           WHERE mf.workspace_id = app_workspace_id() AND m.thread_id = $1 AND mf.id = $2"#,
        thread_id,
        id
    )
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|r| ThreadFile {
        file: MessageFile {
            id: r.id,
            message_id: r.message_id,
            file_id: r.file_id,
            part: r.part,
            filename: r.filename,
            content_type: r.content_type,
            size_bytes: r.size_bytes,
        },
        integration_id: r.integration_id,
        external_id: r.external_id,
    }))
}

/// Threads whose snooze has ended, from any workspace (the worker's sweep).
#[derive(Debug, Clone)]
pub struct Woken {
    pub id: Uuid,
    pub workspace_id: WorkspaceId,
    pub at: DateTime<Utc>,
}

pub async fn snooze_ended(
    conn: &mut PgConnection,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<Woken>> {
    Ok(sqlx::query_as!(
        Woken,
        r#"SELECT id, workspace_id AS "workspace_id: WorkspaceId", snoozed_until AS "at!"
           FROM threads WHERE status = 'snoozed' AND snoozed_until <= $1
           ORDER BY snoozed_until LIMIT $2"#,
        now,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// Unsnoozes a thread whose snooze ended at `at` (not one snoozed again since).
pub async fn wake(conn: &mut PgConnection, id: Uuid, at: DateTime<Utc>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE threads SET status = 'open', snoozed_until = NULL, unread = true
           WHERE workspace_id = app_workspace_id() AND id = $1 AND status = 'snoozed' AND snoozed_until = $2"#,
        id,
        at
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// The latest threads linked to any of these records or addresses, for
/// timelines: (thread id, subject, last message time).
pub async fn for_timeline(
    conn: &mut PgConnection,
    viewer: Option<UserId>,
    records: &[Uuid],
    addresses: &[String],
    limit: i64,
) -> Result<Vec<Thread>> {
    Ok(sqlx::query_as!(
        Thread,
        r#"SELECT t.id, t.channel, t.integration_id, t.external_id, t.subject, t.contact_id, t.organisation_id,
                  t.enquiry_id, t.booking_id, t.status, t.snoozed_until, t.assigned_to, t.unread,
                  t.message_count, t.participants, t.preview, t.last_message_at, t.last_inbound_at,
                  t.last_outbound_at, t.created_at, t.version
           FROM threads t
           LEFT JOIN integrations i ON i.workspace_id = t.workspace_id AND i.id = t.integration_id
           WHERE t.workspace_id = app_workspace_id()
             AND ($1::uuid IS NULL OR t.integration_id IS NULL OR i.shared OR i.owner_id = $1)
             AND (t.contact_id = ANY($2) OR t.organisation_id = ANY($2) OR t.enquiry_id = ANY($2)
                  OR t.booking_id = ANY($2) OR t.participants && $3)
           ORDER BY t.last_message_at DESC LIMIT $4"#,
        viewer.map(|v| v.0),
        records,
        addresses,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// The contact an address belongs to: one whose primary address it is
/// first, then the oldest. Returns the contact and their organisation.
pub async fn contact_for(
    conn: &mut PgConnection,
    addresses: &[String],
) -> Result<Option<(Uuid, Option<Uuid>)>> {
    let row = sqlx::query!(
        r#"SELECT id, organisation_id FROM contacts
           WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL AND emails && $1
           ORDER BY emails[1] = ANY($1) DESC, created_at, id LIMIT 1"#,
        addresses
    )
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|r| (r.id, r.organisation_id)))
}
