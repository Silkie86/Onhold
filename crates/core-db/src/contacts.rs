//! Contacts: people, at an organisation or on their own.

use chrono::{DateTime, Utc};
use core_domain::UserId;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;
use crate::crm::{Candidate, ListParams};

#[derive(Debug, Clone)]
pub struct Contact {
    pub id: Uuid,
    pub organisation_id: Option<Uuid>,
    /// The organisation's name, for showing beside the contact.
    pub organisation_name: Option<String>,
    /// The organisation's country (ISO code), for writing phones internationally.
    pub organisation_country: Option<String>,
    pub name: String,
    pub emails: Vec<String>,
    pub phones: Vec<String>,
    pub role: Option<String>,
    pub preferred_channel: Option<String>,
    pub email_opt_in: bool,
    pub sms_opt_in: bool,
    pub whatsapp_opt_in: bool,
    pub notes: Option<String>,
    pub tags: Vec<String>,
    pub custom: Value,
    pub links: Vec<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

/// Columns a contact list can sort by (besides `custom`).
pub const SORTS: &[&str] = &[
    "name",
    "email",
    "phone",
    "role",
    "organisation",
    "preferred_channel",
    "created_at",
    "updated_at",
];

pub struct Filter<'a> {
    pub list: ListParams<'a>,
    pub organisation_id: Option<Uuid>,
}

/// One page, and how many records match in all.
pub async fn list(conn: &mut PgConnection, f: Filter<'_>) -> Result<(Vec<Contact>, i64)> {
    let p = &f.list;
    let rows = sqlx::query!(
        r#"SELECT c.id, c.organisation_id, o.name AS "organisation_name?",
                  o.country_code::text AS "organisation_country?", c.name, c.emails, c.phones, c.role,
                  c.preferred_channel, c.email_opt_in, c.sms_opt_in, c.whatsapp_opt_in, c.notes, c.tags,
                  c.custom, c.links, c.version, c.created_at, c.updated_at, c.created_by AS "created_by: UserId",
                  c.updated_by AS "updated_by: UserId", count(*) OVER () AS "total!"
           FROM contacts c LEFT JOIN organisations o ON o.id = c.organisation_id
           WHERE c.workspace_id = app_workspace_id() AND c.deleted_at IS NULL
             AND ($1::text IS NULL OR c.search_text LIKE $2 OR $1 <% c.search_text
                  OR lower(o.name) LIKE $2
                  OR EXISTS (SELECT 1 FROM jsonb_each_text(c.custom) e
                             WHERE e.key = ANY($3) AND lower(e.value) LIKE $2))
             AND ($4::text IS NULL OR $4 = ANY(c.tags))
             AND ($5::jsonb IS NULL OR c.custom @> $5)
             AND ($6::uuid IS NULL OR c.organisation_id = $6)
           ORDER BY
             CASE WHEN $7 = 'relevance' THEN c.search_text NOT LIKE $2 END ASC,
             CASE WHEN $7 = 'relevance' THEN word_similarity($1, c.search_text) END DESC NULLS LAST,
             CASE WHEN NOT $8 THEN CASE $7 WHEN 'name' THEN lower(c.name) WHEN 'email' THEN c.emails[1]
                 WHEN 'phone' THEN c.phones[1] WHEN 'role' THEN lower(c.role)
                 WHEN 'organisation' THEN lower(o.name) WHEN 'preferred_channel' THEN c.preferred_channel
                 END END ASC,
             CASE WHEN $8 THEN CASE $7 WHEN 'name' THEN lower(c.name) WHEN 'email' THEN c.emails[1]
                 WHEN 'phone' THEN c.phones[1] WHEN 'role' THEN lower(c.role)
                 WHEN 'organisation' THEN lower(o.name) WHEN 'preferred_channel' THEN c.preferred_channel
                 END END DESC NULLS LAST,
             CASE WHEN NOT $8 THEN CASE $7 WHEN 'created_at' THEN c.created_at
                 WHEN 'updated_at' THEN c.updated_at END END ASC,
             CASE WHEN $8 THEN CASE $7 WHEN 'created_at' THEN c.created_at
                 WHEN 'updated_at' THEN c.updated_at END END DESC NULLS LAST,
             CASE WHEN $7 = 'custom' AND NOT $8 THEN c.custom -> $9 END ASC,
             CASE WHEN $7 = 'custom' AND $8 THEN c.custom -> $9 END DESC NULLS LAST,
             lower(c.name), c.id
           OFFSET $10 LIMIT $11"#,
        p.q,
        p.pattern(),
        p.searchable,
        p.tag,
        p.custom,
        f.organisation_id,
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
        .map(|r| Contact {
            id: r.id,
            organisation_id: r.organisation_id,
            organisation_name: r.organisation_name,
            organisation_country: r.organisation_country,
            name: r.name,
            emails: r.emails,
            phones: r.phones,
            role: r.role,
            preferred_channel: r.preferred_channel,
            email_opt_in: r.email_opt_in,
            sms_opt_in: r.sms_opt_in,
            whatsapp_opt_in: r.whatsapp_opt_in,
            notes: r.notes,
            tags: r.tags,
            custom: r.custom,
            links: r.links,
            version: r.version,
            created_at: r.created_at,
            updated_at: r.updated_at,
            created_by: r.created_by,
            updated_by: r.updated_by,
        })
        .collect();
    Ok((items, total))
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Contact>> {
    Ok(sqlx::query_as!(
        Contact,
        r#"SELECT c.id, c.organisation_id, o.name AS "organisation_name?",
                  o.country_code::text AS "organisation_country?", c.name, c.emails, c.phones, c.role,
                  c.preferred_channel, c.email_opt_in, c.sms_opt_in, c.whatsapp_opt_in, c.notes, c.tags,
                  c.custom, c.links, c.version, c.created_at, c.updated_at, c.created_by AS "created_by: UserId",
                  c.updated_by AS "updated_by: UserId"
           FROM contacts c LEFT JOIN organisations o ON o.id = c.organisation_id
           WHERE c.workspace_id = app_workspace_id() AND c.id = $1 AND c.deleted_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// The same row, locked for an update in this transaction.
pub async fn get_for_update(conn: &mut PgConnection, id: Uuid) -> Result<Option<Contact>> {
    Ok(sqlx::query_as!(
        Contact,
        r#"SELECT c.id, c.organisation_id, NULL::text AS "organisation_name?",
                  NULL::text AS "organisation_country?", c.name, c.emails, c.phones,
                  c.role, c.preferred_channel, c.email_opt_in, c.sms_opt_in, c.whatsapp_opt_in, c.notes,
                  c.tags, c.custom, c.links, c.version, c.created_at, c.updated_at,
                  c.created_by AS "created_by: UserId", c.updated_by AS "updated_by: UserId"
           FROM contacts c
           WHERE c.workspace_id = app_workspace_id() AND c.id = $1 AND c.deleted_at IS NULL
           FOR UPDATE"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

pub struct Values<'a> {
    pub organisation_id: Option<Uuid>,
    pub name: &'a str,
    pub emails: &'a [String],
    pub phones: &'a [String],
    pub phone_keys: &'a [String],
    pub role: Option<&'a str>,
    pub preferred_channel: Option<&'a str>,
    pub email_opt_in: bool,
    pub sms_opt_in: bool,
    pub whatsapp_opt_in: bool,
    pub notes: Option<&'a str>,
    pub tags: &'a [String],
    pub custom: &'a Value,
    pub links: &'a [String],
}

pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO contacts (id, workspace_id, organisation_id, name, emails, phones, phone_keys, role,
                                 preferred_channel, email_opt_in, sms_opt_in, whatsapp_opt_in, notes, tags,
                                 custom, created_by, updated_by, links)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $15,
                   $16)"#,
        id,
        v.organisation_id,
        v.name,
        v.emails,
        v.phones,
        v.phone_keys,
        v.role,
        v.preferred_channel,
        v.email_opt_in,
        v.sms_opt_in,
        v.whatsapp_opt_in,
        v.notes,
        v.tags,
        v.custom,
        by.map(|u| u.0),
        v.links
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
        r#"UPDATE contacts SET organisation_id = $2, name = $3, emails = $4, phones = $5, phone_keys = $6,
                  role = $7, preferred_channel = $8, email_opt_in = $9, sms_opt_in = $10,
                  whatsapp_opt_in = $11, notes = $12, tags = $13, custom = $14, updated_by = $15,
                  links = $16
           WHERE workspace_id = app_workspace_id() AND id = $1"#,
        id,
        v.organisation_id,
        v.name,
        v.emails,
        v.phones,
        v.phone_keys,
        v.role,
        v.preferred_channel,
        v.email_opt_in,
        v.sms_opt_in,
        v.whatsapp_opt_in,
        v.notes,
        v.tags,
        v.custom,
        by.map(|u| u.0),
        v.links
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn soft_delete(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE contacts SET deleted_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND deleted_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Contacts sharing an email address or phone number with these, or with a
/// similar name (lower-cased). Matching addresses and numbers are returned.
pub async fn similar(
    conn: &mut PgConnection,
    name: &str,
    emails: &[String],
    phone_keys: &[String],
    min_similarity: f32,
    except: Option<Uuid>,
    limit: i64,
) -> Result<Vec<Candidate>> {
    Ok(sqlx::query_as!(
        Candidate,
        r#"SELECT id, name,
                  ARRAY(SELECT unnest(emails) INTERSECT SELECT unnest($2::text[])) AS "emails!",
                  ARRAY(SELECT unnest(phone_keys) INTERSECT SELECT unnest($3::text[])) AS "phone_keys!",
                  similarity(lower(name), $1) AS "similarity!"
           FROM contacts
           WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL
             AND (emails && $2 OR phone_keys && $3
                  OR (lower(name) % $1 AND similarity(lower(name), $1) >= $4))
             AND ($5::uuid IS NULL OR id <> $5)
           ORDER BY cardinality(ARRAY(SELECT unnest(emails) INTERSECT SELECT unnest($2::text[]))) DESC,
                    cardinality(ARRAY(SELECT unnest(phone_keys) INTERSECT SELECT unnest($3::text[]))) DESC,
                    5 DESC, id
           LIMIT $6"#,
        name,
        emails,
        phone_keys,
        min_similarity,
        except,
        limit
    )
    .fetch_all(conn)
    .await?)
}

/// What an import matches rows against: every live contact's id, name,
/// organisation, emails and phone keys.
#[derive(Debug, Clone)]
pub struct Key {
    pub id: Uuid,
    pub name: String,
    pub organisation_id: Option<Uuid>,
    pub emails: Vec<String>,
    pub phone_keys: Vec<String>,
}

pub async fn keys(conn: &mut PgConnection) -> Result<Vec<Key>> {
    Ok(sqlx::query_as!(
        Key,
        r#"SELECT id, name, organisation_id, emails, phone_keys FROM contacts
           WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL"#
    )
    .fetch_all(conn)
    .await?)
}

/// The contacts of an organisation, for its timeline.
pub async fn ids_at(conn: &mut PgConnection, organisation_id: Uuid) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT id FROM contacts
           WHERE workspace_id = app_workspace_id() AND organisation_id = $1 AND deleted_at IS NULL"#,
        organisation_id
    )
    .fetch_all(conn)
    .await?)
}
