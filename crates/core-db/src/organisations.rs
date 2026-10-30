//! Organisations: venues, promoters, brands, festivals, agencies.

use chrono::{DateTime, NaiveDate, Utc};
use core_domain::UserId;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;
use crate::crm::{Candidate, ListParams};

#[derive(Debug, Clone)]
pub struct Organisation {
    pub id: Uuid,
    pub name: String,
    pub org_type: Option<String>,
    pub addresses: Value,
    pub tax_id: Option<String>,
    pub default_currency: Option<String>,
    pub payment_terms_days: Option<i32>,
    pub notes: Option<String>,
    pub tags: Vec<String>,
    pub custom: Value,
    pub country_code: Option<String>,
    pub city: Option<String>,
    pub region_key: Option<String>,
    pub relationship: Option<String>,
    pub status_key: Option<String>,
    pub follow_up_on: Option<NaiveDate>,
    pub do_not_contact: bool,
    pub buys_from_others: Vec<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
    /// Live contacts at the organisation.
    pub contact_count: i64,
}

/// Columns an organisation list can sort by (besides `custom`).
pub const SORTS: &[&str] = &[
    "name",
    "org_type",
    "tax_id",
    "default_currency",
    "payment_terms_days",
    "contact_count",
    "country_code",
    "city",
    "relationship",
    "status_key",
    "follow_up_on",
    "created_at",
    "updated_at",
];

/// One page, and how many records match in all.
pub async fn list(conn: &mut PgConnection, p: ListParams<'_>) -> Result<(Vec<Organisation>, i64)> {
    let rows = sqlx::query!(
        r#"SELECT id, name, org_type, addresses, tax_id, default_currency, payment_terms_days, notes, tags,
                  custom, country_code, city, region_key, relationship, status_key, follow_up_on, do_not_contact,
                  buys_from_others, version, created_at, updated_at, created_by AS "created_by: UserId",
                  updated_by AS "updated_by: UserId",
                  (SELECT count(*) FROM contacts c
                   WHERE c.organisation_id = o.id AND c.deleted_at IS NULL) AS "contact_count!",
                  count(*) OVER () AS "total!"
           FROM organisations o
           WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
             AND ($1::text IS NULL OR search_text LIKE $2 OR $1 <% search_text
                  OR EXISTS (SELECT 1 FROM jsonb_each_text(o.custom) e
                             WHERE e.key = ANY($3) AND lower(e.value) LIKE $2))
             AND ($4::text IS NULL OR $4 = ANY(tags))
             AND ($5::jsonb IS NULL OR custom @> $5)
           ORDER BY
             CASE WHEN $6 = 'relevance' THEN search_text NOT LIKE $2 END ASC,
             CASE WHEN $6 = 'relevance' THEN word_similarity($1, search_text) END DESC NULLS LAST,
             CASE WHEN NOT $7 THEN CASE $6 WHEN 'name' THEN lower(name) WHEN 'org_type' THEN lower(org_type)
                 WHEN 'tax_id' THEN tax_id WHEN 'default_currency' THEN default_currency::text
                 WHEN 'country_code' THEN country_code::text WHEN 'city' THEN lower(city)
                 WHEN 'relationship' THEN relationship WHEN 'status_key' THEN status_key END END ASC,
             CASE WHEN $7 THEN CASE $6 WHEN 'name' THEN lower(name) WHEN 'org_type' THEN lower(org_type)
                 WHEN 'tax_id' THEN tax_id WHEN 'default_currency' THEN default_currency::text
                 WHEN 'country_code' THEN country_code::text WHEN 'city' THEN lower(city)
                 WHEN 'relationship' THEN relationship WHEN 'status_key' THEN status_key END END DESC NULLS LAST,
             CASE WHEN NOT $7 THEN CASE $6 WHEN 'payment_terms_days' THEN payment_terms_days::bigint
                 WHEN 'contact_count' THEN (SELECT count(*) FROM contacts c
                     WHERE c.organisation_id = o.id AND c.deleted_at IS NULL) END END ASC,
             CASE WHEN $7 THEN CASE $6 WHEN 'payment_terms_days' THEN payment_terms_days::bigint
                 WHEN 'contact_count' THEN (SELECT count(*) FROM contacts c
                     WHERE c.organisation_id = o.id AND c.deleted_at IS NULL) END END DESC NULLS LAST,
             CASE WHEN NOT $7 THEN CASE $6 WHEN 'created_at' THEN created_at
                 WHEN 'updated_at' THEN updated_at END END ASC,
             CASE WHEN $7 THEN CASE $6 WHEN 'created_at' THEN created_at
                 WHEN 'updated_at' THEN updated_at END END DESC NULLS LAST,
             CASE WHEN $6 = 'follow_up_on' AND NOT $7 THEN follow_up_on END ASC,
             CASE WHEN $6 = 'follow_up_on' AND $7 THEN follow_up_on END DESC NULLS LAST,
             CASE WHEN $6 = 'custom' AND NOT $7 THEN custom -> $8 END ASC,
             CASE WHEN $6 = 'custom' AND $7 THEN custom -> $8 END DESC NULLS LAST,
             lower(name), id
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
        p.limit
    )
    .fetch_all(conn)
    .await?;
    let total = rows.first().map_or(0, |r| r.total);
    let items = rows
        .into_iter()
        .map(|r| Organisation {
            id: r.id,
            name: r.name,
            org_type: r.org_type,
            addresses: r.addresses,
            tax_id: r.tax_id,
            default_currency: r.default_currency,
            payment_terms_days: r.payment_terms_days,
            notes: r.notes,
            tags: r.tags,
            custom: r.custom,
            country_code: r.country_code,
            city: r.city,
            region_key: r.region_key,
            relationship: r.relationship,
            status_key: r.status_key,
            follow_up_on: r.follow_up_on,
            do_not_contact: r.do_not_contact,
            buys_from_others: r.buys_from_others,
            version: r.version,
            created_at: r.created_at,
            updated_at: r.updated_at,
            created_by: r.created_by,
            updated_by: r.updated_by,
            contact_count: r.contact_count,
        })
        .collect();
    Ok((items, total))
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Organisation>> {
    Ok(sqlx::query_as!(
        Organisation,
        r#"SELECT id, name, org_type, addresses, tax_id, default_currency, payment_terms_days, notes, tags,
                  custom, country_code, city, region_key, relationship, status_key, follow_up_on, do_not_contact,
                  buys_from_others, version, created_at, updated_at, created_by AS "created_by: UserId",
                  updated_by AS "updated_by: UserId",
                  (SELECT count(*) FROM contacts c
                   WHERE c.organisation_id = o.id AND c.deleted_at IS NULL) AS "contact_count!"
           FROM organisations o
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// The same row, locked for an update in this transaction.
pub async fn get_for_update(conn: &mut PgConnection, id: Uuid) -> Result<Option<Organisation>> {
    Ok(sqlx::query_as!(
        Organisation,
        r#"SELECT id, name, org_type, addresses, tax_id, default_currency, payment_terms_days, notes, tags,
                  custom, country_code, city, region_key, relationship, status_key, follow_up_on, do_not_contact,
                  buys_from_others, version, created_at, updated_at, created_by AS "created_by: UserId",
                  updated_by AS "updated_by: UserId", 0::bigint AS "contact_count!"
           FROM organisations
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL
           FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// Names of these organisations (deleted ones too, so old links still read).
pub async fn names_of(conn: &mut PgConnection, ids: &[Uuid]) -> Result<Vec<(Uuid, String)>> {
    Ok(sqlx::query!(
        r#"SELECT id, name FROM organisations WHERE workspace_id = app_workspace_id() AND id = ANY($1)"#,
        ids
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|r| (r.id, r.name))
    .collect())
}

pub struct Values<'a> {
    pub name: &'a str,
    pub org_type: Option<&'a str>,
    pub addresses: &'a Value,
    pub tax_id: Option<&'a str>,
    pub default_currency: Option<&'a str>,
    pub payment_terms_days: Option<i32>,
    pub notes: Option<&'a str>,
    pub tags: &'a [String],
    pub custom: &'a Value,
    pub country_code: Option<&'a str>,
    pub city: Option<&'a str>,
    pub region_key: Option<&'a str>,
    pub relationship: Option<&'a str>,
    pub status_key: Option<&'a str>,
    pub follow_up_on: Option<NaiveDate>,
    pub do_not_contact: bool,
    pub buys_from_others: &'a [String],
}

pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO organisations (id, workspace_id, name, org_type, addresses, tax_id, default_currency,
                                      payment_terms_days, notes, tags, custom, created_by, updated_by,
                                      country_code, city, region_key, relationship, status_key,
                                      follow_up_on, do_not_contact, buys_from_others)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $11,
                   $12, $13, $14, $15, $16, $17, $18, $19)"#,
        id,
        v.name,
        v.org_type,
        v.addresses,
        v.tax_id,
        v.default_currency,
        v.payment_terms_days,
        v.notes,
        v.tags,
        v.custom,
        by.map(|u| u.0),
        v.country_code,
        v.city,
        v.region_key,
        v.relationship,
        v.status_key,
        v.follow_up_on,
        v.do_not_contact,
        v.buys_from_others
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
        r#"UPDATE organisations SET name = $2, org_type = $3, addresses = $4, tax_id = $5,
                  default_currency = $6, payment_terms_days = $7, notes = $8, tags = $9, custom = $10,
                  updated_by = $11, country_code = $12, city = $13, region_key = $14, relationship = $15,
                  status_key = $16, follow_up_on = $17, do_not_contact = $18, buys_from_others = $19
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        v.name,
        v.org_type,
        v.addresses,
        v.tax_id,
        v.default_currency,
        v.payment_terms_days,
        v.notes,
        v.tags,
        v.custom,
        by.map(|u| u.0),
        v.country_code,
        v.city,
        v.region_key,
        v.relationship,
        v.status_key,
        v.follow_up_on,
        v.do_not_contact,
        v.buys_from_others
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn soft_delete(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE organisations SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Organisations with a similar name (lower-cased) or the same tax id.
pub async fn similar(
    conn: &mut PgConnection,
    name: &str,
    tax_id: Option<&str>,
    min_similarity: f32,
    except: Option<Uuid>,
    limit: i64,
) -> Result<Vec<(Candidate, bool)>> {
    Ok(sqlx::query!(
        r#"SELECT id, name, similarity(lower(name), $1) AS "similarity!",
                  ($2::text IS NOT NULL AND upper(replace(tax_id, ' ', '')) = upper(replace($2, ' ', '')))
                      AS "same_tax_id!"
           FROM organisations
           WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
             AND ((lower(name) % $1 AND similarity(lower(name), $1) >= $3)
                  OR ($2::text IS NOT NULL AND upper(replace(tax_id, ' ', '')) = upper(replace($2, ' ', ''))))
             AND ($4::uuid IS NULL OR id <> $4)
           ORDER BY 4 DESC, 3 DESC, id
           LIMIT $5"#,
        name,
        tax_id,
        min_similarity,
        except,
        limit
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|r| {
        (
            Candidate {
                id: r.id,
                name: r.name,
                emails: Vec::new(),
                phone_keys: Vec::new(),
                similarity: r.similarity,
            },
            r.same_tax_id,
        )
    })
    .collect())
}

/// Every live record's id and name, for matching rows during an import.
pub async fn names(conn: &mut PgConnection) -> Result<Vec<(Uuid, String)>> {
    Ok(sqlx::query!(
        r#"SELECT id, name FROM organisations WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL"#
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|r| (r.id, r.name))
    .collect())
}

/// One entry of an organisation's interested or potential match list.
#[derive(Debug, Clone)]
pub struct Item {
    pub organisation_id: Uuid,
    /// `interested` or `match`.
    pub list: String,
    pub talent_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    /// The talent's or project's name.
    pub name: String,
}

/// The lists of these organisations, in order. Deleted talent and projects
/// are left out.
pub async fn items_of(conn: &mut PgConnection, ids: &[Uuid]) -> Result<Vec<Item>> {
    Ok(sqlx::query_as!(
        Item,
        r#"SELECT i.organisation_id, i.list, i.talent_id, i.project_id,
                  coalesce(t.name, p.name) AS "name!"
           FROM organisation_items i
           LEFT JOIN talent t ON t.id = i.talent_id AND t.deleted_at IS NULL
           LEFT JOIN projects p ON p.id = i.project_id AND p.deleted_at IS NULL
           WHERE i.workspace_id = app_workspace_id() AND i.organisation_id = ANY($1)
             AND (t.id IS NOT NULL OR p.id IS NOT NULL)
           ORDER BY i.organisation_id, i.list, i.position"#,
        ids
    )
    .fetch_all(conn)
    .await?)
}

/// Replaces one of an organisation's lists: (talent id, project id) pairs,
/// one of each set, in order.
pub async fn set_items(
    conn: &mut PgConnection,
    organisation_id: Uuid,
    list: &str,
    items: &[(Option<Uuid>, Option<Uuid>)],
) -> Result<()> {
    sqlx::query!(
        r#"DELETE FROM organisation_items
           WHERE workspace_id = app_workspace_id() AND organisation_id = $1 AND list = $2"#,
        organisation_id,
        list
    )
    .execute(&mut *conn)
    .await?;
    let talent: Vec<Option<Uuid>> = items.iter().map(|i| i.0).collect();
    let projects: Vec<Option<Uuid>> = items.iter().map(|i| i.1).collect();
    sqlx::query!(
        r#"INSERT INTO organisation_items (workspace_id, organisation_id, list, talent_id, project_id, position)
           SELECT app_workspace_id(), $1, $2, i.talent_id, i.project_id, (i.n - 1)::integer
           FROM unnest($3::uuid[], $4::uuid[]) WITH ORDINALITY AS i (talent_id, project_id, n)"#,
        organisation_id,
        list,
        &talent as &[Option<Uuid>],
        &projects as &[Option<Uuid>]
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Moves an organisation along as deals come in, without going back: an
/// enquiry makes it a prospect unless it is a client already, and a
/// confirmed booking makes it a client. The status follows when it is
/// unset (enquiry) or not booked yet (confirmed), to the given keys. Says
/// whether anything changed.
pub async fn advance(
    conn: &mut PgConnection,
    id: Uuid,
    relationship: &str,
    status_key: Option<&str>,
    replace_statuses: &[String],
    listed_statuses: &[String],
    by: Option<UserId>,
) -> Result<bool> {
    // A status no longer listed reads as none, so it gives way too.
    let done = sqlx::query!(
        r#"UPDATE organisations
           SET relationship = CASE WHEN relationship = 'client' THEN relationship ELSE $2 END,
               status_key = CASE WHEN $3::text IS NOT NULL
                                  AND (status_key IS NULL OR status_key = ANY($4) OR NOT (status_key = ANY($6)))
                                 THEN $3 ELSE status_key END,
               updated_by = $5
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL
             AND (relationship IS DISTINCT FROM (CASE WHEN relationship = 'client' THEN relationship ELSE $2 END)
                  OR ($3::text IS NOT NULL AND status_key IS DISTINCT FROM $3
                      AND (status_key IS NULL OR status_key = ANY($4) OR NOT (status_key = ANY($6)))))"#,
        id,
        relationship,
        status_key,
        replace_statuses,
        by.map(|u| u.0),
        listed_statuses
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Which of these talent exist (and are not deleted).
pub async fn live_talent(conn: &mut PgConnection, ids: &[Uuid]) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT id FROM talent WHERE workspace_id = app_workspace_id() AND id = ANY($1) AND deleted_at IS NULL"#,
        ids
    )
    .fetch_all(conn)
    .await?)
}

/// Which of these projects exist (and are not deleted).
pub async fn live_projects(conn: &mut PgConnection, ids: &[Uuid]) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT id FROM projects WHERE workspace_id = app_workspace_id() AND id = ANY($1) AND deleted_at IS NULL"#,
        ids
    )
    .fetch_all(conn)
    .await?)
}
