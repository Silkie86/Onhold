//! Enquiries: requests for talent, moving through the enquiry pipeline.

use chrono::{DateTime, NaiveDate, Utc};
use core_domain::UserId;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;
use crate::crm::ListParams;

#[derive(Debug, Clone)]
pub struct Enquiry {
    pub id: Uuid,
    pub title: String,
    pub source: String,
    pub organisation_id: Option<Uuid>,
    pub organisation_name: Option<String>,
    pub contact_id: Option<Uuid>,
    pub contact_name: Option<String>,
    pub venue_id: Option<Uuid>,
    pub venue_name: Option<String>,
    pub venue_text: Option<String>,
    pub starts_on: Option<NaiveDate>,
    pub ends_on: Option<NaiveDate>,
    pub budget_minor: Option<i64>,
    pub budget_currency: Option<String>,
    pub pipeline_id: Option<Uuid>,
    pub stage_id: Option<Uuid>,
    pub stage_entered_at: Option<DateTime<Utc>>,
    pub owner_id: Option<Uuid>,
    pub lost_reason: Option<String>,
    pub notes: Option<String>,
    pub tags: Vec<String>,
    pub custom: Value,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

/// Columns an enquiry list can sort by (besides `custom`).
pub const SORTS: &[&str] = &[
    "title",
    "starts_on",
    "budget",
    "stage_entered_at",
    "created_at",
    "updated_at",
];

pub struct Filter<'a> {
    pub list: ListParams<'a>,
    pub stage_id: Option<Uuid>,
    pub talent_id: Option<Uuid>,
    pub owner_id: Option<Uuid>,
    pub organisation_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
}

/// One page, and how many records match in all.
pub async fn list(conn: &mut PgConnection, f: Filter<'_>) -> Result<(Vec<Enquiry>, i64)> {
    let p = &f.list;
    let rows = sqlx::query!(
        r#"SELECT e.id, e.title, e.source, e.organisation_id,
                  (SELECT name FROM organisations o WHERE o.id = e.organisation_id) AS organisation_name,
                  e.contact_id, (SELECT name FROM contacts c WHERE c.id = e.contact_id) AS contact_name,
                  e.venue_id, (SELECT name FROM organisations o WHERE o.id = e.venue_id) AS venue_name,
                  e.venue_text, e.starts_on, e.ends_on, e.budget_minor, e.budget_currency, e.pipeline_id,
                  e.stage_id, e.stage_entered_at, e.owner_id, e.lost_reason, e.notes, e.tags, e.custom,
                  e.version, e.created_at, e.updated_at, e.created_by AS "created_by: UserId",
                  e.updated_by AS "updated_by: UserId", count(*) OVER () AS "total!"
           FROM enquiries e
           WHERE e.workspace_id = app_workspace_id() AND e.deleted_at IS NULL
             AND ($1::text IS NULL OR e.search_text LIKE $2 OR $1 <% e.search_text
                  OR EXISTS (SELECT 1 FROM jsonb_each_text(e.custom) x
                             WHERE x.key = ANY($3) AND lower(x.value) LIKE $2))
             AND ($4::text IS NULL OR $4 = ANY(e.tags))
             AND ($5::jsonb IS NULL OR e.custom @> $5)
             AND ($11::uuid IS NULL OR e.stage_id = $11)
             AND ($12::uuid IS NULL OR EXISTS (SELECT 1 FROM enquiry_talent t
                                               WHERE t.enquiry_id = e.id AND t.talent_id = $12))
             AND ($13::uuid IS NULL OR e.owner_id = $13)
             AND ($14::uuid IS NULL OR e.organisation_id = $14 OR e.venue_id = $14)
             AND ($15::uuid IS NULL OR e.contact_id = $15)
           ORDER BY
             CASE WHEN $6 = 'relevance' THEN e.search_text NOT LIKE $2 END ASC,
             CASE WHEN $6 = 'relevance' THEN word_similarity($1, e.search_text) END DESC NULLS LAST,
             CASE WHEN NOT $7 AND $6 IN ('title', 'name') THEN lower(e.title) END ASC,
             CASE WHEN $7 AND $6 IN ('title', 'name') THEN lower(e.title) END DESC NULLS LAST,
             CASE WHEN NOT $7 AND $6 = 'starts_on' THEN e.starts_on END ASC NULLS LAST,
             CASE WHEN $7 AND $6 = 'starts_on' THEN e.starts_on END DESC NULLS LAST,
             CASE WHEN NOT $7 AND $6 = 'budget' THEN e.budget_minor END ASC NULLS LAST,
             CASE WHEN $7 AND $6 = 'budget' THEN e.budget_minor END DESC NULLS LAST,
             CASE WHEN NOT $7 THEN CASE $6 WHEN 'created_at' THEN e.created_at WHEN 'updated_at' THEN e.updated_at
                 WHEN 'stage_entered_at' THEN e.stage_entered_at END END ASC,
             CASE WHEN $7 THEN CASE $6 WHEN 'created_at' THEN e.created_at WHEN 'updated_at' THEN e.updated_at
                 WHEN 'stage_entered_at' THEN e.stage_entered_at END END DESC NULLS LAST,
             CASE WHEN $6 = 'custom' AND NOT $7 THEN e.custom -> $8 END ASC,
             CASE WHEN $6 = 'custom' AND $7 THEN e.custom -> $8 END DESC NULLS LAST,
             e.created_at DESC, e.id
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
        f.stage_id,
        f.talent_id,
        f.owner_id,
        f.organisation_id,
        f.contact_id,
    )
    .fetch_all(conn)
    .await?;
    let total = rows.first().map_or(0, |r| r.total);
    let items = rows
        .into_iter()
        .map(|r| Enquiry {
            id: r.id,
            title: r.title,
            source: r.source,
            organisation_id: r.organisation_id,
            organisation_name: r.organisation_name,
            contact_id: r.contact_id,
            contact_name: r.contact_name,
            venue_id: r.venue_id,
            venue_name: r.venue_name,
            venue_text: r.venue_text,
            starts_on: r.starts_on,
            ends_on: r.ends_on,
            budget_minor: r.budget_minor,
            budget_currency: r.budget_currency,
            pipeline_id: r.pipeline_id,
            stage_id: r.stage_id,
            stage_entered_at: r.stage_entered_at,
            owner_id: r.owner_id,
            lost_reason: r.lost_reason,
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

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Enquiry>> {
    Ok(sqlx::query_as!(
        Enquiry,
        r#"SELECT e.id, e.title, e.source, e.organisation_id,
                  (SELECT name FROM organisations o WHERE o.id = e.organisation_id) AS organisation_name,
                  e.contact_id, (SELECT name FROM contacts c WHERE c.id = e.contact_id) AS contact_name,
                  e.venue_id, (SELECT name FROM organisations o WHERE o.id = e.venue_id) AS venue_name,
                  e.venue_text, e.starts_on, e.ends_on, e.budget_minor, e.budget_currency, e.pipeline_id,
                  e.stage_id, e.stage_entered_at, e.owner_id, e.lost_reason, e.notes, e.tags, e.custom,
                  e.version, e.created_at, e.updated_at, e.created_by AS "created_by: UserId",
                  e.updated_by AS "updated_by: UserId"
           FROM enquiries e
           WHERE e.workspace_id = app_workspace_id() AND e.id = $1 AND e.deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// Locks the row for an update in this transaction; returns its version.
pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT version FROM enquiries
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL
           FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub struct Values<'a> {
    pub title: &'a str,
    pub source: &'a str,
    pub organisation_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub venue_id: Option<Uuid>,
    pub venue_text: Option<&'a str>,
    pub starts_on: Option<NaiveDate>,
    pub ends_on: Option<NaiveDate>,
    pub budget_minor: Option<i64>,
    pub budget_currency: Option<&'a str>,
    pub pipeline_id: Option<Uuid>,
    pub stage_id: Option<Uuid>,
    pub stage_entered_at: Option<DateTime<Utc>>,
    pub owner_id: Option<Uuid>,
    pub lost_reason: Option<&'a str>,
    pub notes: Option<&'a str>,
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
        r#"INSERT INTO enquiries (id, workspace_id, title, source, organisation_id, contact_id, venue_id,
                                  venue_text, starts_on, ends_on, budget_minor, budget_currency, pipeline_id,
                                  stage_id, stage_entered_at, owner_id, lost_reason, notes, tags, custom,
                                  created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15,
                   $16, $17, $18, $19, $20, $20)"#,
        id,
        v.title,
        v.source,
        v.organisation_id,
        v.contact_id,
        v.venue_id,
        v.venue_text,
        v.starts_on,
        v.ends_on,
        v.budget_minor,
        v.budget_currency,
        v.pipeline_id,
        v.stage_id,
        v.stage_entered_at,
        v.owner_id,
        v.lost_reason,
        v.notes,
        v.tags,
        v.custom,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Replaces the row's values (the source stays). The caller has locked the
/// row and checked its version.
pub async fn update(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE enquiries SET title = $2, organisation_id = $3, contact_id = $4, venue_id = $5,
                  venue_text = $6, starts_on = $7, ends_on = $8, budget_minor = $9, budget_currency = $10,
                  pipeline_id = $11, stage_id = $12, stage_entered_at = $13, owner_id = $14,
                  lost_reason = $15, notes = $16, tags = $17, custom = $18, updated_by = $19
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        v.title,
        v.organisation_id,
        v.contact_id,
        v.venue_id,
        v.venue_text,
        v.starts_on,
        v.ends_on,
        v.budget_minor,
        v.budget_currency,
        v.pipeline_id,
        v.stage_id,
        v.stage_entered_at,
        v.owner_id,
        v.lost_reason,
        v.notes,
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
        r#"UPDATE enquiries SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// The talent each of these enquiries asks for, in order: (enquiry, talent, name).
pub async fn talent_of(
    conn: &mut PgConnection,
    enquiry_ids: &[Uuid],
) -> Result<Vec<(Uuid, Uuid, String)>> {
    Ok(sqlx::query!(
        r#"SELECT et.enquiry_id, et.talent_id, t.name
           FROM enquiry_talent et JOIN talent t ON t.id = et.talent_id
           WHERE et.workspace_id = app_workspace_id() AND et.enquiry_id = ANY($1)
           ORDER BY et.enquiry_id, et.position, t.name"#,
        enquiry_ids
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|r| (r.enquiry_id, r.talent_id, r.name))
    .collect())
}

/// Replaces the talent an enquiry asks for.
pub async fn set_talent(conn: &mut PgConnection, enquiry_id: Uuid, talent: &[Uuid]) -> Result<()> {
    sqlx::query!(
        "DELETE FROM enquiry_talent WHERE workspace_id = app_workspace_id() AND enquiry_id = $1",
        enquiry_id
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        r#"INSERT INTO enquiry_talent (workspace_id, enquiry_id, talent_id, position)
           SELECT app_workspace_id(), $1, t.id, (t.n - 1)::int
           FROM unnest($2::uuid[]) WITH ORDINALITY AS t (id, n)"#,
        enquiry_id,
        talent
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// How many live enquiries sit in a pipeline.
pub async fn count_in_pipeline(conn: &mut PgConnection, pipeline_id: Uuid) -> Result<i64> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM enquiries
           WHERE workspace_id = app_workspace_id() AND pipeline_id = $1 AND deleted_at IS NULL"#,
        pipeline_id
    )
    .fetch_one(conn)
    .await?)
}
