//! The pitch log: what was done to win an organisation over, and the
//! answers it gave.

use chrono::{DateTime, NaiveDate, Utc};
use core_domain::UserId;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: Uuid,
    pub organisation_id: Uuid,
    pub contact_id: Option<Uuid>,
    pub contact_name: Option<String>,
    pub kind: String,
    pub type_key: String,
    pub channel: Option<String>,
    pub talent_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub item_name: Option<String>,
    pub note: Option<String>,
    pub happened_at: DateTime<Utc>,
    pub source: String,
    pub template_key: Option<String>,
    /// For a pitch sent from OnHold: the message, its thread and where
    /// sending it has got to.
    pub message_id: Option<Uuid>,
    pub thread_id: Option<Uuid>,
    pub message_status: Option<String>,
    /// When the organisation answered this action, and the answer.
    pub answered_at: Option<DateTime<Utc>>,
    pub answer_message_id: Option<Uuid>,
    pub answer_thread_id: Option<Uuid>,
    /// The answer's conversation's channel: `email`, `sms` or `whatsapp`.
    pub answer_channel: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

/// An organisation's log, latest first.
pub async fn list(
    conn: &mut PgConnection,
    organisation: Uuid,
    offset: i64,
    limit: i64,
) -> Result<(Vec<Entry>, i64)> {
    let rows = sqlx::query!(
        r#"SELECT p.id, p.organisation_id, p.contact_id, c.name AS "contact_name?", p.kind, p.type_key,
                  p.channel, p.talent_id, p.project_id, coalesce(t.name, pr.name) AS item_name, p.note,
                  p.happened_at, p.source, p.template_key, p.message_id, m.thread_id AS "thread_id?",
                  m.status AS "message_status?", p.answered_at, p.answer_message_id,
                  am.thread_id AS "answer_thread_id?", at.channel AS "answer_channel?", p.version, p.created_at, p.updated_at,
                  p.created_by AS "created_by: UserId", p.updated_by AS "updated_by: UserId",
                  count(*) OVER () AS "total!"
           FROM pitch_log p
           LEFT JOIN contacts c ON c.workspace_id = p.workspace_id AND c.id = p.contact_id AND c.deleted_at IS NULL
           LEFT JOIN talent t ON t.workspace_id = p.workspace_id AND t.id = p.talent_id AND t.deleted_at IS NULL
           LEFT JOIN projects pr ON pr.workspace_id = p.workspace_id AND pr.id = p.project_id
                                AND pr.deleted_at IS NULL
           LEFT JOIN thread_messages m ON m.workspace_id = p.workspace_id AND m.id = p.message_id
           LEFT JOIN thread_messages am ON am.workspace_id = p.workspace_id AND am.id = p.answer_message_id
           LEFT JOIN threads at ON at.workspace_id = am.workspace_id AND at.id = am.thread_id
           WHERE p.workspace_id = app_workspace_id() AND p.organisation_id = $1 AND p.deleted_at IS NULL
           ORDER BY p.happened_at DESC, p.id DESC
           OFFSET $2 LIMIT $3"#,
        organisation,
        offset,
        limit
    )
    .fetch_all(conn)
    .await?;
    let total = rows.first().map_or(0, |r| r.total);
    let items = rows
        .into_iter()
        .map(|r| Entry {
            id: r.id,
            organisation_id: r.organisation_id,
            contact_id: r.contact_id,
            contact_name: r.contact_name,
            kind: r.kind,
            type_key: r.type_key,
            channel: r.channel,
            talent_id: r.talent_id,
            project_id: r.project_id,
            item_name: r.item_name,
            note: r.note,
            happened_at: r.happened_at,
            source: r.source,
            template_key: r.template_key,
            message_id: r.message_id,
            thread_id: r.thread_id,
            message_status: r.message_status,
            answered_at: r.answered_at,
            answer_message_id: r.answer_message_id,
            answer_thread_id: r.answer_thread_id,
            answer_channel: r.answer_channel,
            version: r.version,
            created_at: r.created_at,
            updated_at: r.updated_at,
            created_by: r.created_by,
            updated_by: r.updated_by,
        })
        .collect();
    Ok((items, total))
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Entry>> {
    Ok(sqlx::query_as!(
        Entry,
        r#"SELECT p.id, p.organisation_id, p.contact_id, c.name AS "contact_name?", p.kind, p.type_key,
                  p.channel, p.talent_id, p.project_id, coalesce(t.name, pr.name) AS item_name, p.note,
                  p.happened_at, p.source, p.template_key, p.message_id, m.thread_id AS "thread_id?",
                  m.status AS "message_status?", p.answered_at, p.answer_message_id,
                  am.thread_id AS "answer_thread_id?", at.channel AS "answer_channel?", p.version, p.created_at, p.updated_at,
                  p.created_by AS "created_by: UserId", p.updated_by AS "updated_by: UserId"
           FROM pitch_log p
           LEFT JOIN contacts c ON c.workspace_id = p.workspace_id AND c.id = p.contact_id AND c.deleted_at IS NULL
           LEFT JOIN talent t ON t.workspace_id = p.workspace_id AND t.id = p.talent_id AND t.deleted_at IS NULL
           LEFT JOIN projects pr ON pr.workspace_id = p.workspace_id AND pr.id = p.project_id
                                AND pr.deleted_at IS NULL
           LEFT JOIN thread_messages m ON m.workspace_id = p.workspace_id AND m.id = p.message_id
           LEFT JOIN thread_messages am ON am.workspace_id = p.workspace_id AND am.id = p.answer_message_id
           LEFT JOIN threads at ON at.workspace_id = am.workspace_id AND at.id = am.thread_id
           WHERE p.workspace_id = app_workspace_id() AND p.id = $1 AND p.deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// Locks an entry for an update in this transaction; returns its version.
pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT version FROM pitch_log
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL
           FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub struct Values<'a> {
    pub organisation_id: Uuid,
    pub contact_id: Option<Uuid>,
    pub kind: &'a str,
    pub type_key: &'a str,
    pub channel: Option<&'a str>,
    pub talent_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub note: Option<&'a str>,
    pub happened_at: DateTime<Utc>,
    pub template_key: Option<&'a str>,
}

/// How an entry came to be logged, with the message it went as when not
/// by hand.
#[derive(Debug, Clone, Copy)]
pub enum Source {
    Manual,
    /// Sent from OnHold.
    Sent(Uuid),
    /// Spotted in the member's own Sent mail.
    Spotted(Uuid),
}

/// A new entry.
pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    source: Source,
    by: Option<UserId>,
) -> Result<()> {
    let (source, message) = match source {
        Source::Manual => ("manual", None),
        Source::Sent(m) => ("sent", Some(m)),
        Source::Spotted(m) => ("spotted", Some(m)),
    };
    sqlx::query!(
        r#"INSERT INTO pitch_log (id, workspace_id, organisation_id, contact_id, kind, type_key, channel,
                                  talent_id, project_id, note, happened_at, template_key, message_id, source,
                                  created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $14)"#,
        id,
        v.organisation_id,
        v.contact_id,
        v.kind,
        v.type_key,
        v.channel,
        v.talent_id,
        v.project_id,
        v.note,
        v.happened_at,
        v.template_key,
        message,
        source,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Everything but the organisation, the kind and how it came to be logged,
/// which an entry keeps.
pub async fn update(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE pitch_log SET contact_id = $2, type_key = $3, channel = $4, talent_id = $5, project_id = $6,
                  note = $7, happened_at = $8, template_key = $9, updated_by = $10
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        v.contact_id,
        v.type_key,
        v.channel,
        v.talent_id,
        v.project_id,
        v.note,
        v.happened_at,
        v.template_key,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn soft_delete(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE pitch_log SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// How far back an answer can reach to the actions it answers.
pub const ANSWER_WINDOW_DAYS: i32 = 180;

/// Notes `message` as the answer to the organisation's unanswered actions
/// from before it, with `contact` or no one in particular (anyone, when the
/// answer's sender is not known), within the window. Returns the actions.
pub async fn note_answer(
    conn: &mut PgConnection,
    organisation: Uuid,
    contact: Option<Uuid>,
    message: Uuid,
    at: DateTime<Utc>,
) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"UPDATE pitch_log SET answered_at = $4, answer_message_id = $3
           WHERE workspace_id = app_workspace_id() AND organisation_id = $1 AND kind = 'action'
             AND (answered_at IS NULL OR answered_at > $4) AND deleted_at IS NULL
             AND happened_at <= $4 AND happened_at > $4 - make_interval(days => $5)
             AND ($2::uuid IS NULL OR contact_id IS NULL OR contact_id = $2)
           RETURNING id"#,
        organisation,
        contact,
        message,
        at,
        ANSWER_WINDOW_DAYS
    )
    .fetch_all(conn)
    .await?)
}

/// Works out again when an action was answered: the first message from
/// someone at its organisation (the person it was with, if anyone) after
/// it, within [`ANSWER_WINDOW_DAYS`]. Mail is theirs when it comes from or
/// asks for replies to one of their addresses, as reading it in decides;
/// a text or portal message when its thread is with them. For an action
/// logged or changed after their answer had come in. Returns when, if it
/// was.
pub async fn recheck_answer(conn: &mut PgConnection, id: Uuid) -> Result<Option<DateTime<Utc>>> {
    Ok(sqlx::query_scalar!(
        r#"WITH p AS (
               SELECT id, workspace_id, organisation_id, contact_id, happened_at,
                      happened_at + make_interval(days => $2) AS until
               FROM pitch_log
               WHERE workspace_id = app_workspace_id() AND id = $1 AND kind = 'action'
           ),
           c AS (
               SELECT c.id, c.emails FROM contacts c, p
               WHERE c.workspace_id = p.workspace_id AND c.organisation_id = p.organisation_id
                 AND c.deleted_at IS NULL AND (p.contact_id IS NULL OR c.id = p.contact_id)
           ),
           -- Each way in starts from the organisation's people, so only
           -- their own messages are read.
           answer AS (
               (SELECT m.id, m.sent_at FROM p, c, unnest(c.emails) e, thread_messages m
                JOIN threads t ON t.workspace_id = m.workspace_id AND t.id = m.thread_id
                WHERE m.workspace_id = p.workspace_id AND m.direction = 'inbound' AND m.from_address = e
                  AND m.sent_at > p.happened_at AND m.sent_at <= p.until
                  AND NOT m.automatic AND NOT m.bulk AND t.channel = 'email'
                UNION ALL
                SELECT m.id, m.sent_at FROM p, c, unnest(c.emails) e, thread_messages m
                JOIN threads t ON t.workspace_id = m.workspace_id AND t.id = m.thread_id
                WHERE m.workspace_id = p.workspace_id AND m.direction = 'inbound' AND m.reply_to = e
                  AND m.sent_at > p.happened_at AND m.sent_at <= p.until
                  AND NOT m.automatic AND NOT m.bulk AND t.channel = 'email'
                UNION ALL
                SELECT m.id, m.sent_at FROM p, c
                JOIN threads t ON t.contact_id = c.id
                JOIN thread_messages m ON m.workspace_id = t.workspace_id AND m.thread_id = t.id
                WHERE t.workspace_id = p.workspace_id AND t.channel <> 'email'
                  AND m.direction = 'inbound' AND NOT m.automatic AND NOT m.bulk
                  AND m.sent_at > p.happened_at AND m.sent_at <= p.until)
               ORDER BY sent_at, id LIMIT 1
           )
           UPDATE pitch_log l
           SET answer_message_id = (SELECT id FROM answer),
               answered_at = (SELECT sent_at FROM answer)
           FROM p
           WHERE l.workspace_id = p.workspace_id AND l.id = p.id
           RETURNING l.answered_at"#,
        id,
        ANSWER_WINDOW_DAYS
    )
    .fetch_optional(conn)
    .await?
    .flatten())
}

/// Mail the member sent an organisation from their own mailbox, outside
/// OnHold, that is not in the log.
#[derive(Debug, Clone)]
pub struct Unlogged {
    pub message_id: Uuid,
    pub thread_id: Uuid,
    pub contact_id: Option<Uuid>,
    pub contact_name: Option<String>,
    pub subject: Option<String>,
    pub to: Vec<String>,
    pub sent_at: DateTime<Utc>,
}

/// Up to `limit` recent messages sent to the organisation from a mailbox,
/// not written in OnHold, newest first, leaving out those logged, called
/// not a pitch, or sent within a day of an email action already logged
/// by hand (most likely the same pitch). Only mail in mailboxes `viewer`
/// can open (shared, or their own) is listed.
pub async fn unlogged(
    conn: &mut PgConnection,
    organisation: Uuid,
    since: DateTime<Utc>,
    limit: i64,
    viewer: Option<Uuid>,
) -> Result<Vec<Unlogged>> {
    Ok(sqlx::query_as!(
        Unlogged,
        r#"SELECT m.id AS message_id, m.thread_id, c.id AS "contact_id?", c.name AS "contact_name?", m.subject,
                  m.to_addresses AS "to", m.sent_at
           FROM thread_messages m
           JOIN threads t ON t.workspace_id = m.workspace_id AND t.id = m.thread_id
           LEFT JOIN integrations i ON i.workspace_id = t.workspace_id AND i.id = t.integration_id
           LEFT JOIN contacts c ON c.workspace_id = t.workspace_id AND c.id = t.contact_id AND c.deleted_at IS NULL
                               AND c.organisation_id = $1
           WHERE m.workspace_id = app_workspace_id() AND t.organisation_id = $1 AND t.channel = 'email'
             AND m.direction = 'outbound' AND m.author_id IS NULL AND NOT m.automatic AND m.status = 'sent'
             AND ($4::uuid IS NULL OR t.integration_id IS NULL OR i.shared OR i.owner_id = $4)
             AND m.sent_at >= $2
             AND NOT EXISTS (SELECT 1 FROM pitch_log p WHERE p.workspace_id = m.workspace_id
                             AND p.message_id = m.id AND p.deleted_at IS NULL)
             AND NOT EXISTS (SELECT 1 FROM pitch_not_pitches n WHERE n.workspace_id = m.workspace_id
                             AND n.message_id = m.id)
             AND NOT EXISTS (SELECT 1 FROM pitch_log p WHERE p.workspace_id = m.workspace_id
                             AND p.organisation_id = $1 AND p.kind = 'action' AND p.deleted_at IS NULL
                             AND p.message_id IS NULL AND (p.channel IS NULL OR p.channel = 'email')
                             AND p.happened_at BETWEEN m.sent_at - interval '1 day' AND m.sent_at + interval '1 day')
           ORDER BY m.sent_at DESC, m.id DESC
           LIMIT $3"#,
        organisation,
        since,
        limit,
        viewer
    )
    .fetch_all(conn)
    .await?)
}

/// The organisation an unlogged message went to, if it is one: sent from
/// a mailbox outside OnHold, by email, to an organisation, in a mailbox
/// `viewer` can open.
pub async fn unlogged_organisation(
    conn: &mut PgConnection,
    message: Uuid,
    viewer: Option<Uuid>,
) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT t.organisation_id AS "organisation_id!"
           FROM thread_messages m
           JOIN threads t ON t.workspace_id = m.workspace_id AND t.id = m.thread_id
           LEFT JOIN integrations i ON i.workspace_id = t.workspace_id AND i.id = t.integration_id
           WHERE m.workspace_id = app_workspace_id() AND m.id = $1 AND t.organisation_id IS NOT NULL
             AND t.channel = 'email' AND m.direction = 'outbound' AND m.author_id IS NULL
             AND NOT m.automatic AND m.status = 'sent'
             AND ($2::uuid IS NULL OR t.integration_id IS NULL OR i.shared OR i.owner_id = $2)"#,
        message,
        viewer
    )
    .fetch_optional(conn)
    .await?)
}

/// Whether a message is in the log already.
pub async fn logged(conn: &mut PgConnection, message: Uuid) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM pitch_log WHERE workspace_id = app_workspace_id()
                          AND message_id = $1 AND deleted_at IS NULL) AS "logged!""#,
        message
    )
    .fetch_one(conn)
    .await?)
}

/// Notes that a message in Sent is not a pitch.
pub async fn not_a_pitch(conn: &mut PgConnection, message: Uuid, by: Option<UserId>) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO pitch_not_pitches (workspace_id, message_id, created_by)
           VALUES (app_workspace_id(), $1, $2) ON CONFLICT DO NOTHING"#,
        message,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// An organisation's actions since its last answer: how many, the latest,
/// whether an answer to any came in, and who and what the latest were about.
#[derive(Debug, Clone)]
pub struct Run {
    pub organisation_id: Uuid,
    pub organisation_name: String,
    pub actions: i64,
    pub last_at: DateTime<Utc>,
    pub answered: bool,
    pub contact_id: Option<Uuid>,
    pub contact_name: Option<String>,
    pub talent_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub item_name: Option<String>,
}

/// Each organisation's run of actions since its last logged answer (all
/// organisations, or one), leaving out those marked do not contact and
/// runs that ended before `since`.
pub async fn runs(
    conn: &mut PgConnection,
    organisation: Option<Uuid>,
    since: DateTime<Utc>,
) -> Result<Vec<Run>> {
    Ok(sqlx::query_as!(
        Run,
        r#"WITH last_answer AS (
               SELECT organisation_id, max(happened_at) AS at FROM pitch_log
               WHERE workspace_id = app_workspace_id() AND kind = 'outcome' AND deleted_at IS NULL
                 AND ($1::uuid IS NULL OR organisation_id = $1)
               GROUP BY organisation_id
           ), run AS (
               SELECT p.organisation_id, count(*) AS actions, max(p.happened_at) AS last_at,
                      bool_or(p.answered_at IS NOT NULL) AS answered,
                      (array_agg(p.contact_id ORDER BY p.happened_at DESC, p.id DESC)
                          FILTER (WHERE p.contact_id IS NOT NULL))[1] AS contact_id,
                      (array_agg(p.talent_id ORDER BY p.happened_at DESC, p.id DESC)
                          FILTER (WHERE p.talent_id IS NOT NULL OR p.project_id IS NOT NULL))[1] AS talent_id,
                      (array_agg(p.project_id ORDER BY p.happened_at DESC, p.id DESC)
                          FILTER (WHERE p.talent_id IS NOT NULL OR p.project_id IS NOT NULL))[1] AS project_id
               FROM pitch_log p
               LEFT JOIN last_answer a ON a.organisation_id = p.organisation_id
               LEFT JOIN thread_messages m ON m.workspace_id = p.workspace_id AND m.id = p.message_id
               WHERE p.workspace_id = app_workspace_id() AND p.kind = 'action' AND p.deleted_at IS NULL
                 AND ($1::uuid IS NULL OR p.organisation_id = $1)
                 AND (a.at IS NULL OR p.happened_at > a.at)
                 -- A pitch OnHold failed to send wasn't done.
                 AND m.status IS DISTINCT FROM 'failed'
                 -- Only organisations with something done lately.
                 AND p.organisation_id IN (SELECT organisation_id FROM pitch_log
                                           WHERE workspace_id = app_workspace_id() AND kind = 'action'
                                             AND deleted_at IS NULL AND happened_at >= $2)
               GROUP BY p.organisation_id
           )
           SELECT r.organisation_id AS "organisation_id!", o.name AS organisation_name,
                  r.actions AS "actions!", r.last_at AS "last_at!", r.answered AS "answered!",
                  c.id AS "contact_id?", c.name AS "contact_name?", r.talent_id, r.project_id,
                  coalesce(t.name, pr.name) AS item_name
           FROM run r
           JOIN organisations o ON o.workspace_id = app_workspace_id() AND o.id = r.organisation_id
                               AND o.deleted_at IS NULL AND NOT o.do_not_contact
           -- The person, while they are still there.
           LEFT JOIN contacts c ON c.workspace_id = o.workspace_id AND c.id = r.contact_id AND c.deleted_at IS NULL
                               AND c.organisation_id = o.id
           LEFT JOIN talent t ON t.workspace_id = o.workspace_id AND t.id = r.talent_id AND t.deleted_at IS NULL
           LEFT JOIN projects pr ON pr.workspace_id = o.workspace_id AND pr.id = r.project_id
                                AND pr.deleted_at IS NULL
           WHERE r.last_at >= $2
           ORDER BY r.last_at, r.organisation_id"#,
        organisation,
        since
    )
    .fetch_all(conn)
    .await?)
}

/// An organisation with a follow-up date set by hand.
#[derive(Debug, Clone)]
pub struct Dated {
    pub organisation_id: Uuid,
    pub organisation_name: String,
    pub follow_up_on: NaiveDate,
}

/// Organisations to follow up on or before `until` (all, or one), by the
/// date set on them, leaving out those marked do not contact.
pub async fn dated(
    conn: &mut PgConnection,
    organisation: Option<Uuid>,
    until: NaiveDate,
) -> Result<Vec<Dated>> {
    Ok(sqlx::query_as!(
        Dated,
        r#"SELECT id AS organisation_id, name AS organisation_name, follow_up_on AS "follow_up_on!"
           FROM organisations
           WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL AND NOT do_not_contact
             AND follow_up_on IS NOT NULL AND follow_up_on <= $2
             AND ($1::uuid IS NULL OR id = $1)
           ORDER BY follow_up_on, id"#,
        organisation,
        until
    )
    .fetch_all(conn)
    .await?)
}
