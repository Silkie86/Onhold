//! Talent, the generic bookable, and the members of an act or crew.

use chrono::{DateTime, Utc};
use core_domain::{TalentKind, UserId};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;
use crate::crm::{Candidate, ListParams};

#[derive(Debug, Clone)]
pub struct Talent {
    pub id: Uuid,
    pub kind: TalentKind,
    pub name: String,
    pub legal_name: Option<String>,
    pub talent_type: Option<String>,
    pub bio: Option<String>,
    pub base_location: Option<String>,
    pub default_fee_minor: Option<i64>,
    pub default_fee_currency: Option<String>,
    pub commission_bps: Option<i32>,
    pub status: String,
    pub portal_user_id: Option<Uuid>,
    pub tags: Vec<String>,
    pub custom: Value,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

/// Columns a talent list can sort by (besides `custom`).
pub const SORTS: &[&str] = &[
    "name",
    "kind",
    "legal_name",
    "talent_type",
    "base_location",
    "status",
    "default_fee",
    "commission",
    "created_at",
    "updated_at",
];

pub struct Filter<'a> {
    pub list: ListParams<'a>,
    pub kind: Option<TalentKind>,
    pub status: Option<&'a str>,
}

/// One page, and how many records match in all.
pub async fn list(conn: &mut PgConnection, f: Filter<'_>) -> Result<(Vec<Talent>, i64)> {
    let p = &f.list;
    let rows = sqlx::query!(
        r#"SELECT id, kind AS "kind: TalentKind", name, legal_name, talent_type, bio, base_location,
                  default_fee_minor, default_fee_currency, commission_bps, status, portal_user_id, tags,
                  custom, version, created_at, updated_at, created_by AS "created_by: UserId",
                  updated_by AS "updated_by: UserId", count(*) OVER () AS "total!"
           FROM talent t
           WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
             AND ($1::text IS NULL OR search_text LIKE $2 OR $1 <% search_text
                  OR EXISTS (SELECT 1 FROM jsonb_each_text(t.custom) e
                             WHERE e.key = ANY($3) AND lower(e.value) LIKE $2))
             AND ($4::text IS NULL OR $4 = ANY(tags))
             AND ($5::jsonb IS NULL OR custom @> $5)
             AND ($6::talent_kind IS NULL OR kind = $6)
             AND ($7::text IS NULL OR status = $7)
           ORDER BY
             CASE WHEN $8 = 'relevance' THEN search_text NOT LIKE $2 END ASC,
             CASE WHEN $8 = 'relevance' THEN word_similarity($1, search_text) END DESC NULLS LAST,
             CASE WHEN NOT $9 THEN CASE $8 WHEN 'name' THEN lower(name) WHEN 'kind' THEN kind::text
                 WHEN 'legal_name' THEN lower(legal_name) WHEN 'talent_type' THEN lower(talent_type)
                 WHEN 'base_location' THEN lower(base_location) WHEN 'status' THEN status END END ASC,
             CASE WHEN $9 THEN CASE $8 WHEN 'name' THEN lower(name) WHEN 'kind' THEN kind::text
                 WHEN 'legal_name' THEN lower(legal_name) WHEN 'talent_type' THEN lower(talent_type)
                 WHEN 'base_location' THEN lower(base_location) WHEN 'status' THEN status END END DESC NULLS LAST,
             CASE WHEN NOT $9 THEN CASE $8 WHEN 'default_fee' THEN default_fee_minor
                 WHEN 'commission' THEN commission_bps END END ASC,
             CASE WHEN $9 THEN CASE $8 WHEN 'default_fee' THEN default_fee_minor
                 WHEN 'commission' THEN commission_bps END END DESC NULLS LAST,
             CASE WHEN NOT $9 THEN CASE $8 WHEN 'created_at' THEN created_at
                 WHEN 'updated_at' THEN updated_at END END ASC,
             CASE WHEN $9 THEN CASE $8 WHEN 'created_at' THEN created_at
                 WHEN 'updated_at' THEN updated_at END END DESC NULLS LAST,
             CASE WHEN $8 = 'custom' AND NOT $9 THEN custom -> $10 END ASC,
             CASE WHEN $8 = 'custom' AND $9 THEN custom -> $10 END DESC NULLS LAST,
             lower(name), id
           OFFSET $11 LIMIT $12"#,
        p.q,
        p.pattern(),
        p.searchable,
        p.tag,
        p.custom,
        f.kind as Option<TalentKind>,
        f.status,
        p.sort,
        p.desc,
        p.sort_custom,
        p.offset,
        p.limit
    )
    .fetch_all(conn)
    .await?;
    let total = rows.first().map_or(0, |r| r.total);
    let items = rows
        .into_iter()
        .map(|r| Talent {
            id: r.id,
            kind: r.kind,
            name: r.name,
            legal_name: r.legal_name,
            talent_type: r.talent_type,
            bio: r.bio,
            base_location: r.base_location,
            default_fee_minor: r.default_fee_minor,
            default_fee_currency: r.default_fee_currency,
            commission_bps: r.commission_bps,
            status: r.status,
            portal_user_id: r.portal_user_id,
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

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Talent>> {
    Ok(sqlx::query_as!(
        Talent,
        r#"SELECT id, kind AS "kind: TalentKind", name, legal_name, talent_type, bio, base_location,
                  default_fee_minor, default_fee_currency, commission_bps, status, portal_user_id, tags,
                  custom, version, created_at, updated_at, created_by AS "created_by: UserId",
                  updated_by AS "updated_by: UserId"
           FROM talent WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// The same row, locked for an update in this transaction.
/// A talent's name and commission rate, deleted or not: bookings keep the
/// talent they were made with.
pub async fn commission(conn: &mut PgConnection, id: Uuid) -> Result<Option<Commission>> {
    Ok(sqlx::query_as!(
        Commission,
        r#"SELECT name, commission_bps FROM talent WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// See [`commission`].
#[derive(Debug, Clone)]
pub struct Commission {
    pub name: String,
    pub commission_bps: Option<i32>,
}

pub async fn get_for_update(conn: &mut PgConnection, id: Uuid) -> Result<Option<Talent>> {
    Ok(sqlx::query_as!(
        Talent,
        r#"SELECT id, kind AS "kind: TalentKind", name, legal_name, talent_type, bio, base_location,
                  default_fee_minor, default_fee_currency, commission_bps, status, portal_user_id, tags,
                  custom, version, created_at, updated_at, created_by AS "created_by: UserId",
                  updated_by AS "updated_by: UserId"
           FROM talent WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL
           FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub struct Values<'a> {
    pub kind: TalentKind,
    pub name: &'a str,
    pub legal_name: Option<&'a str>,
    pub talent_type: Option<&'a str>,
    pub bio: Option<&'a str>,
    pub base_location: Option<&'a str>,
    pub default_fee_minor: Option<i64>,
    pub default_fee_currency: Option<&'a str>,
    pub commission_bps: Option<i32>,
    pub status: &'a str,
    pub portal_user_id: Option<Uuid>,
    pub tags: &'a [String],
    pub custom: &'a Value,
}

pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO talent (id, workspace_id, kind, name, legal_name, talent_type, bio, base_location,
                               default_fee_minor, default_fee_currency, commission_bps, status,
                               portal_user_id, tags, custom, created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $15)"#,
        id,
        v.kind as TalentKind,
        v.name,
        v.legal_name,
        v.talent_type,
        v.bio,
        v.base_location,
        v.default_fee_minor,
        v.default_fee_currency,
        v.commission_bps,
        v.status,
        v.portal_user_id,
        v.tags,
        v.custom,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Replaces the row's values. The caller has locked the row and checked its
/// version.
pub async fn update(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE talent SET kind = $2, name = $3, legal_name = $4, talent_type = $5, bio = $6,
                  base_location = $7, default_fee_minor = $8, default_fee_currency = $9,
                  commission_bps = $10, status = $11, portal_user_id = $12, tags = $13, custom = $14,
                  updated_by = $15
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        v.kind as TalentKind,
        v.name,
        v.legal_name,
        v.talent_type,
        v.bio,
        v.base_location,
        v.default_fee_minor,
        v.default_fee_currency,
        v.commission_bps,
        v.status,
        v.portal_user_id,
        v.tags,
        v.custom,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn soft_delete(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE talent SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Talent whose name is the same as or similar to `name` (lower-cased).
pub async fn similar(
    conn: &mut PgConnection,
    name: &str,
    min_similarity: f32,
    except: Option<Uuid>,
    limit: i64,
) -> Result<Vec<Candidate>> {
    Ok(sqlx::query_as!(
        Candidate,
        r#"SELECT id, name, '{}'::text[] AS "emails!", '{}'::text[] AS "phone_keys!",
                  similarity(lower(name), $1) AS "similarity!"
           FROM talent
           WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
             AND lower(name) % $1 AND similarity(lower(name), $1) >= $2
             AND ($3::uuid IS NULL OR id <> $3)
           ORDER BY 5 DESC, id
           LIMIT $4"#,
        name,
        min_similarity,
        except,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// Every live record's id and name, for matching rows during an import.
pub async fn names(conn: &mut PgConnection) -> Result<Vec<(Uuid, String)>> {
    Ok(sqlx::query!(
        r#"SELECT id, name FROM talent WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL"#
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|r| (r.id, r.name))
    .collect())
}

// Members ----------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Member {
    pub id: Uuid,
    pub talent_id: Uuid,
    pub name: String,
    pub role: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub notes: Option<String>,
    pub position: i32,
    pub custom: Value,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub async fn members(conn: &mut PgConnection, talent_id: Uuid) -> Result<Vec<Member>> {
    Ok(sqlx::query_as!(
        Member,
        r#"SELECT id, talent_id, name, role, email, phone, notes, position, custom, version, created_at,
                  updated_at
           FROM talent_members
           WHERE workspace_id = app_workspace_id() AND talent_id = $1 AND deleted_at IS NULL
           ORDER BY position, created_at"#,
        talent_id
    )
    .fetch_all(conn)
    .await?)
}

pub async fn member_for_update(
    conn: &mut PgConnection,
    talent_id: Uuid,
    id: Uuid,
) -> Result<Option<Member>> {
    Ok(sqlx::query_as!(
        Member,
        r#"SELECT id, talent_id, name, role, email, phone, notes, position, custom, version, created_at,
                  updated_at
           FROM talent_members
           WHERE workspace_id = app_workspace_id() AND talent_id = $1 AND id = $2 AND deleted_at IS NULL
           FOR UPDATE"#,
        talent_id,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub struct MemberValues<'a> {
    pub name: &'a str,
    pub role: Option<&'a str>,
    pub email: Option<&'a str>,
    pub phone: Option<&'a str>,
    pub notes: Option<&'a str>,
    pub position: i32,
}

pub async fn insert_member(
    conn: &mut PgConnection,
    id: Uuid,
    talent_id: Uuid,
    v: MemberValues<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO talent_members (id, workspace_id, talent_id, name, role, email, phone, notes,
                                       position, created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $9)"#,
        id,
        talent_id,
        v.name,
        v.role,
        v.email,
        v.phone,
        v.notes,
        v.position,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn update_member(
    conn: &mut PgConnection,
    id: Uuid,
    v: MemberValues<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE talent_members SET name = $2, role = $3, email = $4, phone = $5, notes = $6,
                  position = $7, updated_by = $8
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        v.name,
        v.role,
        v.email,
        v.phone,
        v.notes,
        v.position,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn delete_member(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE talent_members SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Every member the talent has had, removed ones too, for its timeline.
pub async fn member_ids(conn: &mut PgConnection, talent_id: Uuid) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT id FROM talent_members WHERE workspace_id = app_workspace_id() AND talent_id = $1"#,
        talent_id
    )
    .fetch_all(conn)
    .await?)
}
