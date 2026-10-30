//! Bookings: holds, pending and confirmed engagements, with the talent each
//! one takes. The database refuses two confirmed bookings of one talent at
//! once (see the `booking_talent_no_double_booking` constraint).

use chrono::{DateTime, Utc};
use core_domain::{BookingStatus, UserId, WorkspaceId};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;
use crate::crm::ListParams;

/// The constraint that refuses a double booking.
pub const NO_DOUBLE_BOOKING: &str = "booking_talent_no_double_booking";

#[derive(Debug, Clone)]
pub struct Booking {
    pub id: Uuid,
    pub enquiry_id: Option<Uuid>,
    pub title: String,
    pub status: BookingStatus,
    pub status_changed_at: DateTime<Utc>,
    pub organisation_id: Option<Uuid>,
    pub organisation_name: Option<String>,
    pub contact_id: Option<Uuid>,
    pub contact_name: Option<String>,
    pub venue_id: Option<Uuid>,
    pub venue_name: Option<String>,
    pub venue_text: Option<String>,
    pub timezone: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub all_day: bool,
    pub schedule: Value,
    pub hold_expires_at: Option<DateTime<Utc>>,
    pub fee_minor: Option<i64>,
    pub fee_currency: Option<String>,
    pub deposit_bps: Option<i32>,
    pub cancel_reason: Option<String>,
    pub clash_override: bool,
    pub override_reason: Option<String>,
    pub override_by: Option<Uuid>,
    pub notes: Option<String>,
    pub tags: Vec<String>,
    pub custom: Value,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

/// Columns a booking list can sort by (besides `custom`).
pub const SORTS: &[&str] = &[
    "starts_at",
    "title",
    "status",
    "fee",
    "created_at",
    "updated_at",
];

pub struct Filter<'a> {
    pub list: ListParams<'a>,
    /// Bookings overlapping `[from, to)`.
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    /// Any of these statuses; empty means all.
    pub statuses: &'a [BookingStatus],
    pub talent_id: Option<Uuid>,
    pub organisation_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub enquiry_id: Option<Uuid>,
}

/// One page, and how many records match in all.
pub async fn list(conn: &mut PgConnection, f: Filter<'_>) -> Result<(Vec<Booking>, i64)> {
    let p = &f.list;
    let rows = sqlx::query!(
        r#"SELECT b.id, b.enquiry_id, b.title, b.status AS "status: BookingStatus", b.status_changed_at,
                  b.organisation_id,
                  (SELECT name FROM organisations o WHERE o.id = b.organisation_id) AS organisation_name,
                  b.contact_id, (SELECT name FROM contacts c WHERE c.id = b.contact_id) AS contact_name,
                  b.venue_id, (SELECT name FROM organisations o WHERE o.id = b.venue_id) AS venue_name,
                  b.venue_text, b.timezone, b.starts_at, b.ends_at, b.all_day, b.schedule, b.hold_expires_at,
                  b.fee_minor, b.fee_currency, b.deposit_bps, b.cancel_reason, b.clash_override,
                  b.override_reason, b.override_by, b.notes, b.tags, b.custom, b.version, b.created_at,
                  b.updated_at, b.created_by AS "created_by: UserId", b.updated_by AS "updated_by: UserId",
                  count(*) OVER () AS "total!"
           FROM bookings b
           WHERE b.workspace_id = app_workspace_id() AND b.deleted_at IS NULL
             AND ($1::text IS NULL OR b.search_text LIKE $2 OR $1 <% b.search_text
                  OR EXISTS (SELECT 1 FROM jsonb_each_text(b.custom) x
                             WHERE x.key = ANY($3) AND lower(x.value) LIKE $2))
             AND ($4::text IS NULL OR $4 = ANY(b.tags))
             AND ($5::jsonb IS NULL OR b.custom @> $5)
             AND ($11::timestamptz IS NULL OR b.ends_at > $11)
             AND ($12::timestamptz IS NULL OR b.starts_at < $12)
             AND (cardinality($13::booking_status[]) = 0 OR b.status = ANY($13))
             AND ($14::uuid IS NULL OR EXISTS (SELECT 1 FROM booking_talent t
                                               WHERE t.booking_id = b.id AND t.talent_id = $14))
             AND ($15::uuid IS NULL OR b.organisation_id = $15 OR b.venue_id = $15)
             AND ($16::uuid IS NULL OR b.contact_id = $16)
             AND ($17::uuid IS NULL OR b.enquiry_id = $17)
           ORDER BY
             CASE WHEN $6 = 'relevance' THEN b.search_text NOT LIKE $2 END ASC,
             CASE WHEN $6 = 'relevance' THEN word_similarity($1, b.search_text) END DESC NULLS LAST,
             CASE WHEN NOT $7 AND $6 IN ('title', 'name') THEN lower(b.title) END ASC,
             CASE WHEN $7 AND $6 IN ('title', 'name') THEN lower(b.title) END DESC NULLS LAST,
             CASE WHEN NOT $7 AND $6 = 'status' THEN b.status::text END ASC,
             CASE WHEN $7 AND $6 = 'status' THEN b.status::text END DESC,
             CASE WHEN NOT $7 AND $6 = 'fee' THEN b.fee_minor END ASC NULLS LAST,
             CASE WHEN $7 AND $6 = 'fee' THEN b.fee_minor END DESC NULLS LAST,
             CASE WHEN NOT $7 THEN CASE $6 WHEN 'starts_at' THEN b.starts_at WHEN 'created_at' THEN b.created_at
                 WHEN 'updated_at' THEN b.updated_at END END ASC,
             CASE WHEN $7 THEN CASE $6 WHEN 'starts_at' THEN b.starts_at WHEN 'created_at' THEN b.created_at
                 WHEN 'updated_at' THEN b.updated_at END END DESC NULLS LAST,
             CASE WHEN $6 = 'custom' AND NOT $7 THEN b.custom -> $8 END ASC,
             CASE WHEN $6 = 'custom' AND $7 THEN b.custom -> $8 END DESC NULLS LAST,
             b.starts_at, b.id
           OFFSET $9 LIMIT $10"#,
        p.q,
        p.pattern(),
        p.searchable,
        p.tag,
        p.custom,
        p.sort,
        p.desc,
        p.sort_custom,
        p.offset,
        p.limit,
        f.from,
        f.to,
        f.statuses as &[BookingStatus],
        f.talent_id,
        f.organisation_id,
        f.contact_id,
        f.enquiry_id,
    )
    .fetch_all(conn)
    .await?;
    let total = rows.first().map_or(0, |r| r.total);
    let items = rows
        .into_iter()
        .map(|r| Booking {
            id: r.id,
            enquiry_id: r.enquiry_id,
            title: r.title,
            status: r.status,
            status_changed_at: r.status_changed_at,
            organisation_id: r.organisation_id,
            organisation_name: r.organisation_name,
            contact_id: r.contact_id,
            contact_name: r.contact_name,
            venue_id: r.venue_id,
            venue_name: r.venue_name,
            venue_text: r.venue_text,
            timezone: r.timezone,
            starts_at: r.starts_at,
            ends_at: r.ends_at,
            all_day: r.all_day,
            schedule: r.schedule,
            hold_expires_at: r.hold_expires_at,
            fee_minor: r.fee_minor,
            fee_currency: r.fee_currency,
            deposit_bps: r.deposit_bps,
            cancel_reason: r.cancel_reason,
            clash_override: r.clash_override,
            override_reason: r.override_reason,
            override_by: r.override_by,
            notes: r.notes,
            tags: r.tags,
            custom: r.custom,
            version: r.version,
            created_at: r.created_at,
            updated_at: r.updated_at,
            created_by: r.created_by,
            updated_by: r.updated_by,
        })
        .collect();
    Ok((items, total))
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Booking>> {
    Ok(sqlx::query_as!(
        Booking,
        r#"SELECT b.id, b.enquiry_id, b.title, b.status AS "status: BookingStatus", b.status_changed_at,
                  b.organisation_id,
                  (SELECT name FROM organisations o WHERE o.id = b.organisation_id) AS organisation_name,
                  b.contact_id, (SELECT name FROM contacts c WHERE c.id = b.contact_id) AS contact_name,
                  b.venue_id, (SELECT name FROM organisations o WHERE o.id = b.venue_id) AS venue_name,
                  b.venue_text, b.timezone, b.starts_at, b.ends_at, b.all_day, b.schedule, b.hold_expires_at,
                  b.fee_minor, b.fee_currency, b.deposit_bps, b.cancel_reason, b.clash_override,
                  b.override_reason, b.override_by, b.notes, b.tags, b.custom, b.version, b.created_at,
                  b.updated_at, b.created_by AS "created_by: UserId", b.updated_by AS "updated_by: UserId"
           FROM bookings b
           WHERE b.workspace_id = app_workspace_id() AND b.id = $1 AND b.deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// Locks the row for an update in this transaction; returns its version.
pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT version FROM bookings
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL
           FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// What a person edits; status and its details change through [`set_status`].
pub struct Values<'a> {
    pub enquiry_id: Option<Uuid>,
    pub title: &'a str,
    pub organisation_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub venue_id: Option<Uuid>,
    pub venue_text: Option<&'a str>,
    pub timezone: &'a str,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub all_day: bool,
    pub schedule: &'a Value,
    pub fee_minor: Option<i64>,
    pub fee_currency: Option<&'a str>,
    pub deposit_bps: Option<i32>,
    pub notes: Option<&'a str>,
    pub tags: &'a [String],
    pub custom: &'a Value,
}

pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    status: BookingStatus,
    hold_expires_at: Option<DateTime<Utc>>,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO bookings (id, workspace_id, enquiry_id, title, status, hold_expires_at, organisation_id,
                                 contact_id, venue_id, venue_text, timezone, starts_at, ends_at, all_day,
                                 schedule, fee_minor, fee_currency, deposit_bps, notes, tags, custom,
                                 created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                   $17, $18, $19, $20, $21, $21)"#,
        id,
        v.enquiry_id,
        v.title,
        status as BookingStatus,
        hold_expires_at,
        v.organisation_id,
        v.contact_id,
        v.venue_id,
        v.venue_text,
        v.timezone,
        v.starts_at,
        v.ends_at,
        v.all_day,
        v.schedule,
        v.fee_minor,
        v.fee_currency,
        v.deposit_bps,
        v.notes,
        v.tags,
        v.custom,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Replaces the editable values. The caller has locked the row and checked
/// its version.
pub async fn update(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE bookings SET enquiry_id = $2, title = $3, organisation_id = $4, contact_id = $5,
                  venue_id = $6, venue_text = $7, timezone = $8, starts_at = $9, ends_at = $10, all_day = $11,
                  schedule = $12, fee_minor = $13, fee_currency = $14, deposit_bps = $15, notes = $16,
                  tags = $17, custom = $18, updated_by = $19
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        v.enquiry_id,
        v.title,
        v.organisation_id,
        v.contact_id,
        v.venue_id,
        v.venue_text,
        v.timezone,
        v.starts_at,
        v.ends_at,
        v.all_day,
        v.schedule,
        v.fee_minor,
        v.fee_currency,
        v.deposit_bps,
        v.notes,
        v.tags,
        v.custom,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// A change of status and what goes with it.
pub struct StatusChange<'a> {
    pub status: BookingStatus,
    pub hold_expires_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<&'a str>,
    pub clash_override: bool,
    pub override_reason: Option<&'a str>,
    pub override_by: Option<Uuid>,
}

pub async fn set_status(
    conn: &mut PgConnection,
    id: Uuid,
    c: StatusChange<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE bookings SET status = $2, status_changed_at = now(), hold_expires_at = $3,
                  cancel_reason = $4, clash_override = $5, override_reason = $6, override_by = $7,
                  updated_by = $8
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        c.status as BookingStatus,
        c.hold_expires_at,
        c.cancel_reason,
        c.clash_override,
        c.override_reason,
        c.override_by,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Sets when a hold lapses (or that it never does).
pub async fn set_hold_expiry(
    conn: &mut PgConnection,
    id: Uuid,
    at: Option<DateTime<Utc>>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE bookings SET hold_expires_at = $2, updated_by = $3
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        at,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn soft_delete(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE bookings SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// The talent each of these bookings takes, in order: (booking, talent, name).
pub async fn talent_of(
    conn: &mut PgConnection,
    booking_ids: &[Uuid],
) -> Result<Vec<(Uuid, Uuid, String)>> {
    Ok(sqlx::query!(
        r#"SELECT bt.booking_id, bt.talent_id, t.name
           FROM booking_talent bt JOIN talent t ON t.id = bt.talent_id
           WHERE bt.workspace_id = app_workspace_id() AND bt.booking_id = ANY($1)
           ORDER BY bt.booking_id, bt.position, t.name"#,
        booking_ids
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|r| (r.booking_id, r.talent_id, r.name))
    .collect())
}

/// Replaces the talent a booking takes. Fails with the double-booking
/// constraint if the booking is firm and one of them is already booked.
pub async fn set_talent(conn: &mut PgConnection, booking_id: Uuid, talent: &[Uuid]) -> Result<()> {
    sqlx::query!(
        r#"DELETE FROM booking_talent
           WHERE workspace_id = app_workspace_id() AND booking_id = $1 AND NOT (talent_id = ANY($2))"#,
        booking_id,
        talent
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        r#"INSERT INTO booking_talent (workspace_id, booking_id, talent_id, position, during)
           SELECT app_workspace_id(), $1, t.id, (t.n - 1)::int, 'empty'
           FROM unnest($2::uuid[]) WITH ORDINALITY AS t (id, n)
           ON CONFLICT (booking_id, talent_id) DO UPDATE SET position = excluded.position"#,
        booking_id,
        talent
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Another booking of the same talent at an overlapping time.
#[derive(Debug, Clone)]
pub struct Overlap {
    pub booking_id: Uuid,
    pub title: String,
    pub status: BookingStatus,
    pub clash_override: bool,
    pub talent_id: Uuid,
    pub talent_name: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

/// Live bookings (holds, pending and confirmed) of any of these talent
/// overlapping `[starts, ends)`, other than `except`.
pub async fn overlapping(
    conn: &mut PgConnection,
    talent: &[Uuid],
    starts: DateTime<Utc>,
    ends: DateTime<Utc>,
    except: Option<Uuid>,
) -> Result<Vec<Overlap>> {
    Ok(sqlx::query_as!(
        Overlap,
        r#"SELECT b.id AS booking_id, b.title, b.status AS "status: BookingStatus", b.clash_override,
                  bt.talent_id, t.name AS talent_name, b.starts_at, b.ends_at
           FROM booking_talent bt
           JOIN bookings b ON b.id = bt.booking_id
           JOIN talent t ON t.id = bt.talent_id
           WHERE bt.workspace_id = app_workspace_id() AND bt.talent_id = ANY($1)
             AND bt.during && tstzrange($2, $3, '[)')
             AND b.deleted_at IS NULL AND b.status IN ('hold', 'pending', 'confirmed')
             AND ($4::uuid IS NULL OR b.id <> $4)
           ORDER BY b.starts_at, t.name"#,
        talent,
        starts,
        ends,
        except
    )
    .fetch_all(conn)
    .await?)
}

/// A booking the worker has work for, in some workspace.
#[derive(Debug, Clone)]
pub struct DueItem {
    pub id: Uuid,
    pub workspace_id: WorkspaceId,
    /// The time that made it due (expiry or end), which keys the job.
    pub at: DateTime<Utc>,
}

/// Holds whose expiry has passed, across workspaces (worker role).
pub async fn holds_expired(
    conn: &mut PgConnection,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<DueItem>> {
    Ok(sqlx::query_as!(
        DueItem,
        r#"SELECT b.id, b.workspace_id AS "workspace_id: WorkspaceId", b.hold_expires_at AS "at!"
           FROM bookings b JOIN workspaces w ON w.id = b.workspace_id
           WHERE b.status = 'hold' AND b.deleted_at IS NULL AND b.hold_expires_at <= $1
             AND w.status IN ('trial', 'active')
           ORDER BY b.hold_expires_at
           LIMIT $2"#,
        now,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// Confirmed bookings that have ended, across workspaces (worker role).
pub async fn ended(
    conn: &mut PgConnection,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<DueItem>> {
    Ok(sqlx::query_as!(
        DueItem,
        r#"SELECT b.id, b.workspace_id AS "workspace_id: WorkspaceId", b.ends_at AS at
           FROM bookings b JOIN workspaces w ON w.id = b.workspace_id
           WHERE b.status = 'confirmed' AND b.deleted_at IS NULL AND b.ends_at <= $1
             AND w.status IN ('trial', 'active')
           ORDER BY b.ends_at
           LIMIT $2"#,
        now,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// A booking whose date (plus a rule's offset) has arrived.
#[derive(Debug, Clone)]
pub struct Reached {
    pub id: Uuid,
    pub at: DateTime<Utc>,
}

/// Live bookings whose `field` (`starts_at`, `ends_at` or
/// `hold_expires_at`) plus the offset falls in `(from, to]`, earliest first,
/// for date-offset rules. Cancelled and expired bookings are left out.
pub async fn date_between(
    conn: &mut PgConnection,
    field: &str,
    offset_minutes: i32,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<Reached>> {
    Ok(sqlx::query_as!(
        Reached,
        r#"SELECT id AS "id!", at AS "at!" FROM (
               SELECT id, CASE $1 WHEN 'starts_at' THEN starts_at WHEN 'ends_at' THEN ends_at
                                  WHEN 'hold_expires_at' THEN hold_expires_at END
                          + make_interval(mins => $2::int) AS at
               FROM bookings
               WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                 AND status NOT IN ('cancelled', 'expired')
           ) b
           WHERE at > $3 AND at <= $4
           ORDER BY at, id
           LIMIT $5"#,
        field,
        offset_minutes,
        from,
        to,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// Like [`date_between`] for the custom date or date-time field `key`. A
/// bare date counts as midnight in the booking's own timezone.
pub async fn custom_date_between(
    conn: &mut PgConnection,
    key: &str,
    offset_minutes: i32,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<Reached>> {
    Ok(sqlx::query_as!(
        Reached,
        r#"SELECT id AS "id!", at AS "at!" FROM (
               SELECT id,
                      CASE
                          WHEN custom->>$1 ~ '^\d{4}-\d{2}-\d{2}$'
                              THEN ((custom->>$1)::date::timestamp AT TIME ZONE timezone)
                          WHEN custom->>$1 ~ '^\d{4}-\d{2}-\d{2}T'
                              THEN (custom->>$1)::timestamptz
                      END + make_interval(mins => $2::int) AS at
               FROM bookings
               WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
                 AND status NOT IN ('cancelled', 'expired') AND custom ? $1
           ) b
           WHERE at > $3 AND at <= $4
           ORDER BY at, id
           LIMIT $5"#,
        key,
        offset_minutes,
        from,
        to,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// Records that a firm booking stands despite a clash, and why.
pub async fn set_override(
    conn: &mut PgConnection,
    id: Uuid,
    reason: &str,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE bookings SET clash_override = true, override_reason = $2, override_by = $3, updated_by = $3
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        reason,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}
