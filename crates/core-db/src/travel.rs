//! Travel on a booking (flights, trains, ground transport, hotels) and the
//! money lines beside its fee (buyouts and other income, costs).

use chrono::{DateTime, NaiveDate, Utc};
use core_domain::UserId;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct TravelItem {
    pub id: Uuid,
    pub booking_id: Uuid,
    pub kind: String,
    pub status: String,
    pub carrier: Option<String>,
    pub number: Option<String>,
    pub reference: Option<String>,
    pub from_place: Option<String>,
    pub from_code: Option<String>,
    pub to_place: Option<String>,
    pub to_code: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub start_tz: String,
    pub ends_at: Option<DateTime<Utc>>,
    pub end_tz: Option<String>,
    pub travellers: Vec<String>,
    pub notes: Option<String>,
    pub cost_minor: Option<i64>,
    pub cost_currency: Option<String>,
    pub paid_by: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

/// The travel of these bookings, in time order.
pub async fn list(conn: &mut PgConnection, booking_ids: &[Uuid]) -> Result<Vec<TravelItem>> {
    Ok(sqlx::query_as!(
        TravelItem,
        r#"SELECT id, booking_id, kind, status, carrier, number, reference, from_place, from_code,
                  to_place, to_code, starts_at, start_tz, ends_at, end_tz, travellers, notes,
                  cost_minor, cost_currency::text AS cost_currency, paid_by, version, created_at,
                  updated_at, created_by AS "created_by: UserId", updated_by AS "updated_by: UserId"
           FROM travel_items
           WHERE workspace_id = app_workspace_id() AND booking_id = ANY($1) AND deleted_at IS NULL
           ORDER BY starts_at, created_at, id"#,
        booking_ids
    )
    .fetch_all(conn)
    .await?)
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<TravelItem>> {
    Ok(sqlx::query_as!(
        TravelItem,
        r#"SELECT id, booking_id, kind, status, carrier, number, reference, from_place, from_code,
                  to_place, to_code, starts_at, start_tz, ends_at, end_tz, travellers, notes,
                  cost_minor, cost_currency::text AS cost_currency, paid_by, version, created_at,
                  updated_at, created_by AS "created_by: UserId", updated_by AS "updated_by: UserId"
           FROM travel_items
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// Locks the row for an update in this transaction; returns its version.
pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT version FROM travel_items
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL
           FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub struct TravelValues<'a> {
    pub kind: &'a str,
    pub status: &'a str,
    pub carrier: Option<&'a str>,
    pub number: Option<&'a str>,
    pub reference: Option<&'a str>,
    pub from_place: Option<&'a str>,
    pub from_code: Option<&'a str>,
    pub to_place: Option<&'a str>,
    pub to_code: Option<&'a str>,
    pub starts_at: DateTime<Utc>,
    pub start_tz: &'a str,
    pub ends_at: Option<DateTime<Utc>>,
    pub end_tz: Option<&'a str>,
    pub travellers: &'a [String],
    pub notes: Option<&'a str>,
    pub cost_minor: Option<i64>,
    pub cost_currency: Option<&'a str>,
    pub paid_by: &'a str,
}

pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    booking_id: Uuid,
    v: TravelValues<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO travel_items (id, workspace_id, booking_id, kind, status, carrier, number, reference,
                                     from_place, from_code, to_place, to_code, starts_at, start_tz, ends_at,
                                     end_tz, travellers, notes, cost_minor, cost_currency, paid_by,
                                     created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15,
                   $16, $17, $18, $19, $20, $21, $21)"#,
        id,
        booking_id,
        v.kind,
        v.status,
        v.carrier,
        v.number,
        v.reference,
        v.from_place,
        v.from_code,
        v.to_place,
        v.to_code,
        v.starts_at,
        v.start_tz,
        v.ends_at,
        v.end_tz,
        v.travellers,
        v.notes,
        v.cost_minor,
        v.cost_currency,
        v.paid_by,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn update(
    conn: &mut PgConnection,
    id: Uuid,
    v: TravelValues<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE travel_items SET kind = $2, status = $3, carrier = $4, number = $5, reference = $6,
                  from_place = $7, from_code = $8, to_place = $9, to_code = $10, starts_at = $11,
                  start_tz = $12, ends_at = $13, end_tz = $14, travellers = $15, notes = $16,
                  cost_minor = $17, cost_currency = $18, paid_by = $19, updated_by = $20
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        v.kind,
        v.status,
        v.carrier,
        v.number,
        v.reference,
        v.from_place,
        v.from_code,
        v.to_place,
        v.to_code,
        v.starts_at,
        v.start_tz,
        v.ends_at,
        v.end_tz,
        v.travellers,
        v.notes,
        v.cost_minor,
        v.cost_currency,
        v.paid_by,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn soft_delete(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE travel_items SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

// Money lines ------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct MoneyLine {
    pub id: Uuid,
    pub booking_id: Uuid,
    pub direction: String,
    pub category: String,
    pub description: String,
    pub amount_minor: i64,
    pub currency: String,
    pub spent_on: Option<NaiveDate>,
    pub notes: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

/// The money lines of these bookings: income first, then costs, oldest first.
pub async fn lines(conn: &mut PgConnection, booking_ids: &[Uuid]) -> Result<Vec<MoneyLine>> {
    Ok(sqlx::query_as!(
        MoneyLine,
        r#"SELECT id, booking_id, direction, category, description, amount_minor,
                  currency::text AS "currency!", spent_on, notes, version, created_at, updated_at,
                  created_by AS "created_by: UserId", updated_by AS "updated_by: UserId"
           FROM money_lines
           WHERE workspace_id = app_workspace_id() AND booking_id = ANY($1) AND deleted_at IS NULL
           ORDER BY direction DESC, created_at, id"#,
        booking_ids
    )
    .fetch_all(conn)
    .await?)
}

pub async fn line(conn: &mut PgConnection, id: Uuid) -> Result<Option<MoneyLine>> {
    Ok(sqlx::query_as!(
        MoneyLine,
        r#"SELECT id, booking_id, direction, category, description, amount_minor,
                  currency::text AS "currency!", spent_on, notes, version, created_at, updated_at,
                  created_by AS "created_by: UserId", updated_by AS "updated_by: UserId"
           FROM money_lines
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn lock_line(conn: &mut PgConnection, id: Uuid) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT version FROM money_lines
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL
           FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub struct LineValues<'a> {
    pub direction: &'a str,
    pub category: &'a str,
    pub description: &'a str,
    pub amount_minor: i64,
    pub currency: &'a str,
    pub spent_on: Option<NaiveDate>,
    pub notes: Option<&'a str>,
}

pub async fn insert_line(
    conn: &mut PgConnection,
    id: Uuid,
    booking_id: Uuid,
    v: LineValues<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO money_lines (id, workspace_id, booking_id, direction, category, description,
                                    amount_minor, currency, spent_on, notes, created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)"#,
        id,
        booking_id,
        v.direction,
        v.category,
        v.description,
        v.amount_minor,
        v.currency,
        v.spent_on,
        v.notes,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn update_line(
    conn: &mut PgConnection,
    id: Uuid,
    v: LineValues<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE money_lines SET direction = $2, category = $3, description = $4, amount_minor = $5,
                  currency = $6, spent_on = $7, notes = $8, updated_by = $9
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        v.direction,
        v.category,
        v.description,
        v.amount_minor,
        v.currency,
        v.spent_on,
        v.notes,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn delete_line(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE money_lines SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}
