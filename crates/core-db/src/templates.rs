//! Message templates and partials.

use chrono::{DateTime, Utc};
use core_domain::UserId;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Template {
    pub id: Uuid,
    pub kind: String,
    pub key: String,
    pub locale: String,
    pub name: String,
    pub subject: Option<String>,
    pub body: String,
    pub description: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub async fn list(conn: &mut PgConnection, kind: Option<&str>) -> Result<Vec<Template>> {
    Ok(sqlx::query_as!(
        Template,
        r#"SELECT id, kind, key, locale, name, subject, body, description, version, created_at, updated_at
           FROM templates
           WHERE workspace_id = app_workspace_id() AND archived_at IS NULL
             AND ($1::text IS NULL OR kind = $1)
           ORDER BY key, locale"#,
        kind
    )
    .fetch_all(conn)
    .await?)
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Template>> {
    Ok(sqlx::query_as!(
        Template,
        r#"SELECT id, kind, key, locale, name, subject, body, description, version, created_at, updated_at
           FROM templates WHERE workspace_id = app_workspace_id() AND id = $1 AND archived_at IS NULL"#,
        id
    )
    .fetch_optional(conn)
    .await?)
}

/// The best template for `key` in `locale`: an exact match, then the
/// language alone (`pt` for `pt-BR`), then English, then any.
pub async fn find(conn: &mut PgConnection, key: &str, locale: &str) -> Result<Option<Template>> {
    let lang = locale.split('-').next().unwrap_or(locale);
    Ok(sqlx::query_as!(
        Template,
        r#"SELECT id, kind, key, locale, name, subject, body, description, version, created_at, updated_at
           FROM templates
           WHERE workspace_id = app_workspace_id() AND key = $1 AND archived_at IS NULL
           ORDER BY CASE WHEN locale = $2 THEN 0 WHEN locale = $3 THEN 1 WHEN locale = 'en' THEN 2 ELSE 3 END,
                    locale
           LIMIT 1"#,
        key,
        locale,
        lang
    )
    .fetch_optional(conn)
    .await?)
}

/// Whether any active template has this key.
pub async fn key_exists(conn: &mut PgConnection, key: &str) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM templates
                          WHERE workspace_id = app_workspace_id() AND key = $1 AND archived_at IS NULL) AS "e!""#,
        key
    )
    .fetch_one(conn)
    .await?)
}

/// Every active partial's source by key (best locale per key).
pub async fn partials(conn: &mut PgConnection, locale: &str) -> Result<Vec<(String, String)>> {
    let lang = locale.split('-').next().unwrap_or(locale);
    let rows = sqlx::query!(
        r#"SELECT DISTINCT ON (key) key, body
           FROM templates
           WHERE workspace_id = app_workspace_id() AND kind = 'partial' AND archived_at IS NULL
           ORDER BY key, CASE WHEN locale = $1 THEN 0 WHEN locale = $2 THEN 1 WHEN locale = 'en' THEN 2 ELSE 3 END"#,
        locale,
        lang
    )
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(|r| (r.key, r.body)).collect())
}

pub struct Values<'a> {
    pub name: &'a str,
    pub subject: Option<&'a str>,
    pub body: &'a str,
    pub description: Option<&'a str>,
}

pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    kind: &str,
    key: &str,
    locale: &str,
    v: Values<'_>,
    by: Option<UserId>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO templates (id, workspace_id, kind, key, locale, name, subject, body, description,
                                  created_by, updated_by)
           VALUES ($1, app_workspace_id(), $2, $3, $4, $5, $6, $7, $8, $9, $9)"#,
        id,
        kind,
        key,
        locale,
        v.name,
        v.subject,
        v.body,
        v.description,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn update(
    conn: &mut PgConnection,
    id: Uuid,
    v: Values<'_>,
    expected_version: Option<i64>,
    by: Option<UserId>,
) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE templates SET name = $2, subject = $3, body = $4, description = $5, updated_by = $6
           WHERE workspace_id = app_workspace_id() AND id = $1 AND archived_at IS NULL
             AND ($7::bigint IS NULL OR version = $7)"#,
        id,
        v.name,
        v.subject,
        v.body,
        v.description,
        by.map(|u| u.0),
        expected_version
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn archive(conn: &mut PgConnection, id: Uuid, by: Option<UserId>) -> Result<bool> {
    let done = sqlx::query!(
        r#"UPDATE templates SET archived_at = now(), updated_by = $2
           WHERE workspace_id = app_workspace_id() AND id = $1 AND archived_at IS NULL"#,
        id,
        by.map(|u| u.0)
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected() == 1)
}
