//! The talent portal's reads and writes. Every query here is narrowed to the
//! talent records whose `portal_user_id` is the user in context, in SQL, so
//! a talent member sees their own dates and nothing else of the workspace.

use chrono::{DateTime, Utc};
use core_domain::{BookingStatus, UserId, WorkspaceId};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

/// The talent records the signed-in user is the portal user of.
pub async fn my_talent(conn: &mut PgConnection) -> Result<Vec<(Uuid, String)>> {
    Ok(sqlx::query!(
        r#"SELECT id, name FROM talent
           WHERE workspace_id = app_workspace_id() AND portal_user_id = app_user_id()
             AND deleted_at IS NULL
           ORDER BY name, id"#
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|r| (r.id, r.name))
    .collect())
}

/// One of the user's dates, for one of their talent records.
#[derive(Debug, Clone)]
pub struct Date {
    pub id: Uuid,
    pub talent_id: Uuid,
    pub talent_name: String,
    pub title: String,
    pub status: BookingStatus,
    pub venue: Option<String>,
    pub organisation_name: Option<String>,
    pub timezone: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub all_day: bool,
    pub schedule: Value,
    pub hold_expires_at: Option<DateTime<Utc>>,
    pub fee_minor: Option<i64>,
    pub fee_currency: Option<String>,
    pub answer: Option<String>,
    pub answer_note: Option<String>,
    pub answered_at: Option<DateTime<Utc>>,
    pub enquiry_id: Option<Uuid>,
    pub created_by: Option<UserId>,
}

/// The user's dates overlapping `[from, to)` in these statuses, soonest
/// first (or latest first when `newest_first`).
pub async fn dates(
    conn: &mut PgConnection,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    statuses: &[BookingStatus],
    newest_first: bool,
    limit: i64,
) -> Result<Vec<Date>> {
    Ok(sqlx::query_as!(
        Date,
        r#"SELECT b.id, bt.talent_id, t.name AS talent_name, b.title, b.status AS "status: BookingStatus",
                  coalesce((SELECT name FROM organisations o WHERE o.id = b.venue_id), b.venue_text) AS venue,
                  (SELECT name FROM organisations o WHERE o.id = b.organisation_id) AS organisation_name,
                  b.timezone, b.starts_at, b.ends_at, b.all_day, b.schedule, b.hold_expires_at,
                  b.fee_minor, b.fee_currency, bt.answer, bt.answer_note, bt.answered_at,
                  b.enquiry_id, b.created_by AS "created_by: UserId"
           FROM booking_talent bt
           JOIN bookings b ON b.id = bt.booking_id
           JOIN talent t ON t.id = bt.talent_id
           WHERE bt.workspace_id = app_workspace_id() AND b.deleted_at IS NULL
             AND t.portal_user_id = app_user_id() AND t.deleted_at IS NULL
             AND b.status = ANY($3)
             AND ($1::timestamptz IS NULL OR b.ends_at > $1)
             AND ($2::timestamptz IS NULL OR b.starts_at < $2)
           ORDER BY CASE WHEN $4 THEN b.starts_at END DESC,
                    CASE WHEN NOT $4 THEN b.starts_at END ASC, t.name, b.id
           LIMIT $5"#,
        from,
        to,
        statuses as &[BookingStatus],
        newest_first,
        limit,
    )
    .fetch_all(conn)
    .await?)
}

/// One date of the user's, for each of their talent records on it.
pub async fn date(conn: &mut PgConnection, booking_id: Uuid) -> Result<Vec<Date>> {
    Ok(sqlx::query_as!(
        Date,
        r#"SELECT b.id, bt.talent_id, t.name AS talent_name, b.title, b.status AS "status: BookingStatus",
                  coalesce((SELECT name FROM organisations o WHERE o.id = b.venue_id), b.venue_text) AS venue,
                  (SELECT name FROM organisations o WHERE o.id = b.organisation_id) AS organisation_name,
                  b.timezone, b.starts_at, b.ends_at, b.all_day, b.schedule, b.hold_expires_at,
                  b.fee_minor, b.fee_currency, bt.answer, bt.answer_note, bt.answered_at,
                  b.enquiry_id, b.created_by AS "created_by: UserId"
           FROM booking_talent bt
           JOIN bookings b ON b.id = bt.booking_id
           JOIN talent t ON t.id = bt.talent_id
           WHERE bt.workspace_id = app_workspace_id() AND bt.booking_id = $1 AND b.deleted_at IS NULL
             AND t.portal_user_id = app_user_id() AND t.deleted_at IS NULL
           ORDER BY bt.position, t.name"#,
        booking_id
    )
    .fetch_all(conn)
    .await?)
}

/// Records a talent's answer to a date. False when the date is not theirs.
pub async fn set_answer(
    conn: &mut PgConnection,
    booking_id: Uuid,
    talent_id: Uuid,
    answer: &str,
    note: Option<&str>,
    by: UserId,
) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE booking_talent bt
           SET answer = $3, answer_note = $4, answered_at = now(), answered_by = $5
           FROM talent t
           WHERE bt.workspace_id = app_workspace_id() AND bt.booking_id = $1 AND bt.talent_id = $2
             AND t.id = bt.talent_id AND t.portal_user_id = app_user_id() AND t.deleted_at IS NULL"#,
        booking_id,
        talent_id,
        answer,
        note,
        by.0
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Who should hear about a talent's answer: the enquiry's owner, otherwise
/// whoever made the booking, if they are still a staff member.
pub async fn date_owner(conn: &mut PgConnection, booking_id: Uuid) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT m.user_id
           FROM bookings b
           LEFT JOIN enquiries e ON e.id = b.enquiry_id
           JOIN memberships m ON m.workspace_id = b.workspace_id AND m.deleted_at IS NULL
                AND m.role <> 'talent' AND m.user_id = coalesce(e.owner_id, b.created_by)
           WHERE b.workspace_id = app_workspace_id() AND b.id = $1"#,
        booking_id
    )
    .fetch_optional(conn)
    .await?)
}

/// A document the portal may show: on one of the user's dates, or on one of
/// their talent records, of a kind meant for the talent, with a file.
#[derive(Debug, Clone)]
pub struct Document {
    pub id: Uuid,
    pub kind: String,
    pub title: String,
    pub related_entity: String,
    pub related_id: Uuid,
    pub expires_on: Option<chrono::NaiveDate>,
    pub signature_status: String,
    pub updated_at: DateTime<Utc>,
}

/// Kinds a talent sees on their dates.
pub const DATE_KINDS: &[&str] = &[
    "contract",
    "confirmation",
    "advance_sheet",
    "rider",
    "tech_spec",
    "stage_plot",
];
/// Kinds a talent sees on their own record.
pub const TALENT_KINDS: &[&str] = &[
    "rider",
    "tech_spec",
    "stage_plot",
    "epk",
    "insurance",
    "passport",
    "visa",
];

/// The portal's documents on these bookings (all the user's own, checked
/// here) and the user's talent records, newest first.
pub async fn documents(conn: &mut PgConnection, booking_ids: &[Uuid]) -> Result<Vec<Document>> {
    Ok(sqlx::query_as!(
        Document,
        r#"SELECT d.id, d.kind, d.title, d.related_entity AS "related_entity!", d.related_id AS "related_id!",
                  d.expires_on, d.signature_status, d.updated_at
           FROM documents d
           WHERE d.workspace_id = app_workspace_id() AND d.deleted_at IS NULL AND d.current_version > 0
             AND ((d.related_entity = 'booking' AND d.related_id = ANY($1) AND d.kind = ANY($2)
                   AND EXISTS (SELECT 1 FROM booking_talent bt JOIN talent t ON t.id = bt.talent_id
                               WHERE bt.booking_id = d.related_id AND t.portal_user_id = app_user_id()
                                 AND t.deleted_at IS NULL))
                  OR (d.related_entity = 'talent' AND d.kind = ANY($3)
                      AND d.related_id IN (SELECT id FROM talent WHERE workspace_id = app_workspace_id()
                                           AND portal_user_id = app_user_id() AND deleted_at IS NULL)))
           ORDER BY d.updated_at DESC, d.id
           LIMIT 200"#,
        booking_ids,
        DATE_KINDS as &[&str],
        TALENT_KINDS as &[&str],
    )
    .fetch_all(conn)
    .await?)
}

/// Whether the portal may open this document: on a date of the user's, or
/// on their talent record, of a kind meant for them; or the statement of one
/// of their final settlements when `with_statements`.
pub async fn may_open_document(
    conn: &mut PgConnection,
    id: Uuid,
    with_dates: bool,
    with_statements: bool,
) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1 FROM documents d
             WHERE d.workspace_id = app_workspace_id() AND d.id = $1 AND d.deleted_at IS NULL
               AND ((d.related_entity = 'booking' AND $4 AND d.kind = ANY($2)
                     AND EXISTS (SELECT 1 FROM booking_talent bt JOIN talent t ON t.id = bt.talent_id
                                 JOIN bookings b ON b.id = bt.booking_id AND b.deleted_at IS NULL
                                 WHERE bt.booking_id = d.related_id AND t.portal_user_id = app_user_id()
                                   AND t.deleted_at IS NULL))
                    OR (d.related_entity = 'talent' AND d.kind = ANY($3)
                        AND d.related_id IN (SELECT id FROM talent WHERE workspace_id = app_workspace_id()
                                             AND portal_user_id = app_user_id() AND deleted_at IS NULL))
                    OR ($5 AND EXISTS (SELECT 1 FROM settlements s JOIN talent t ON t.id = s.talent_id
                                       WHERE s.workspace_id = app_workspace_id() AND s.document_id = d.id
                                         AND s.status = 'final' AND s.deleted_at IS NULL
                                         AND t.portal_user_id = app_user_id() AND t.deleted_at IS NULL)))
           ) AS "x!""#,
        id,
        DATE_KINDS as &[&str],
        TALENT_KINDS as &[&str],
        with_dates,
        with_statements,
    )
    .fetch_one(conn)
    .await?)
}

/// A final settlement of one of the user's talent records: what a date paid them.
#[derive(Debug, Clone)]
pub struct Statement {
    pub id: Uuid,
    pub booking_id: Uuid,
    pub booking_title: String,
    pub starts_at: DateTime<Utc>,
    pub talent_id: Uuid,
    pub talent_name: String,
    pub currency: String,
    pub gross_minor: i64,
    pub commission_minor: i64,
    pub expenses_minor: i64,
    pub net_minor: i64,
    pub finalised_at: Option<DateTime<Utc>>,
    pub document_id: Option<Uuid>,
    /// Paid out to the talent so far, in the statement's currency.
    pub paid_minor: i64,
}

pub async fn statements(conn: &mut PgConnection, limit: i64) -> Result<Vec<Statement>> {
    Ok(sqlx::query_as!(
        Statement,
        r#"SELECT s.id, s.booking_id, b.title AS booking_title, b.starts_at, s.talent_id, t.name AS talent_name,
                  s.currency::text AS "currency!", s.gross_minor, s.commission_minor, s.expenses_minor, s.net_minor,
                  s.finalised_at, s.document_id,
                  coalesce((SELECT sum(p.amount_minor) FROM payments p
                            WHERE p.workspace_id = s.workspace_id AND p.settlement_id = s.id
                              AND p.kind = 'payout' AND p.voided_at IS NULL), 0)::bigint AS "paid_minor!"
           FROM settlements s
           JOIN bookings b ON b.id = s.booking_id
           JOIN talent t ON t.id = s.talent_id
           WHERE s.workspace_id = app_workspace_id() AND s.status = 'final' AND s.deleted_at IS NULL
             AND t.portal_user_id = app_user_id() AND t.deleted_at IS NULL
           ORDER BY s.finalised_at DESC, s.id
           LIMIT $1"#,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// The workspace's portal switches.
pub async fn settings(conn: &mut PgConnection) -> Result<Value> {
    Ok(
        sqlx::query_scalar!("SELECT portal FROM workspaces WHERE id = app_workspace_id()")
            .fetch_optional(conn)
            .await?
            .unwrap_or_default(),
    )
}

pub async fn set_settings(
    conn: &mut PgConnection,
    id: WorkspaceId,
    portal: &Value,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        "UPDATE workspaces SET portal = $2, updated_by = $3 WHERE id = $1",
        id.0,
        portal,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Links a talent record to its portal user. False when the record is gone
/// or already opens for someone else.
pub async fn link_talent(conn: &mut PgConnection, talent_id: Uuid, user: UserId) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE talent SET portal_user_id = $2, updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL
             AND (portal_user_id IS NULL OR portal_user_id = $2)"#,
        talent_id,
        user.0
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Ends a talent record's portal access. Returns the user it opened for.
pub async fn unlink_talent(
    conn: &mut PgConnection,
    talent_id: Uuid,
    by: Option<UserId>,
) -> Result<Option<Uuid>> {
    let user = sqlx::query_scalar!(
        r#"SELECT portal_user_id AS "user!" FROM talent
           WHERE workspace_id = app_workspace_id() AND id = $1 AND portal_user_id IS NOT NULL
           FOR UPDATE"#,
        talent_id
    )
    .fetch_optional(&mut *conn)
    .await?;
    if user.is_some() {
        sqlx::query!(
            r#"UPDATE talent SET portal_user_id = NULL, updated_by = $2
               WHERE workspace_id = app_workspace_id() AND id = $1"#,
            talent_id,
            by.map(|u| u.0)
        )
        .execute(conn)
        .await?;
    }
    Ok(user)
}

/// How many talent records still open for this user here.
pub async fn talent_count(conn: &mut PgConnection, user: Uuid) -> Result<i64> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM talent
           WHERE workspace_id = app_workspace_id() AND portal_user_id = $1 AND deleted_at IS NULL"#,
        user
    )
    .fetch_one(conn)
    .await?)
}

/// The talent record an invitation opens, if it is a portal invitation.
pub async fn invitation_talent(
    conn: &mut PgConnection,
    invitation_id: Uuid,
) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar!(
        "SELECT talent_id FROM invitations WHERE workspace_id = app_workspace_id() AND id = $1",
        invitation_id
    )
    .fetch_optional(conn)
    .await?
    .flatten())
}

/// Revokes open portal invitations for a talent record.
pub async fn revoke_invitations(conn: &mut PgConnection, talent_id: Uuid) -> Result<u64> {
    Ok(sqlx::query!(
        r#"UPDATE invitations SET revoked_at = now()
           WHERE workspace_id = app_workspace_id() AND talent_id = $1
             AND accepted_at IS NULL AND revoked_at IS NULL"#,
        talent_id
    )
    .execute(conn)
    .await?
    .rows_affected())
}

/// The talent record's open portal invitation, if any: (email, expires_at).
pub async fn open_invitation(
    conn: &mut PgConnection,
    talent_id: Uuid,
) -> Result<Option<(String, DateTime<Utc>)>> {
    Ok(sqlx::query!(
        r#"SELECT email::text AS "email!", expires_at FROM invitations
           WHERE workspace_id = app_workspace_id() AND talent_id = $1
             AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at > now()
           ORDER BY created_at DESC LIMIT 1"#,
        talent_id
    )
    .fetch_optional(conn)
    .await?
    .map(|r| (r.email, r.expires_at)))
}

/// Ends every talent record's portal access for this user here (they left
/// the workspace).
pub async fn unlink_user(conn: &mut PgConnection, user: UserId, by: Option<UserId>) -> Result<u64> {
    Ok(sqlx::query!(
        r#"UPDATE talent SET portal_user_id = NULL, updated_by = $2
           WHERE workspace_id = app_workspace_id() AND portal_user_id = $1"#,
        user.0,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?
    .rows_affected())
}
