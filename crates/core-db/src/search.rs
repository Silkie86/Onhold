//! Global search across the CRM, and the workspace's tags.

use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;
use crate::crm::like_contains;

#[derive(Debug, Clone)]
pub struct Hit {
    /// `talent`, `organisation` or `contact`.
    pub entity: String,
    pub id: Uuid,
    pub name: String,
    /// A second line: the talent's type, the organisation's type, the
    /// contact's role and organisation.
    pub detail: Option<String>,
    pub score: f32,
}

/// Which entities to search and the searchable custom fields of each.
pub struct Scope<'a> {
    pub talent: Option<&'a [String]>,
    pub organisations: Option<&'a [String]>,
    pub contacts: Option<&'a [String]>,
}

/// Best matches first. `q` is lower-cased and trimmed. Exact names rank
/// above names starting with `q`, then records containing it anywhere,
/// then fuzzy matches.
pub async fn search(
    conn: &mut PgConnection,
    q: &str,
    scope: Scope<'_>,
    limit: i64,
) -> Result<Vec<Hit>> {
    let contains = like_contains(q);
    let prefix = format!("{}%", &contains[1..]);
    let none: &[String] = &[];
    Ok(sqlx::query_as!(
        Hit,
        r#"SELECT entity AS "entity!", id AS "id!", name AS "name!", detail, score AS "score!" FROM (
               SELECT 'talent' AS entity, t.id, t.name, coalesce(t.talent_type, t.kind::text) AS detail,
                      (CASE WHEN lower(t.name) = $1 THEN 3 WHEN lower(t.name) LIKE $3 THEN 2
                            WHEN t.search_text LIKE $2 THEN 1 ELSE 0 END
                       + word_similarity($1, t.search_text))::real AS score
               FROM talent t
               WHERE $4 AND t.workspace_id = app_workspace_id() AND t.deleted_at IS NULL
                 AND (t.search_text LIKE $2 OR $1 <% t.search_text
                      OR EXISTS (SELECT 1 FROM jsonb_each_text(t.custom) e
                                 WHERE e.key = ANY($5) AND lower(e.value) LIKE $2))
               UNION ALL
               SELECT 'organisation', o.id, o.name, o.org_type,
                      (CASE WHEN lower(o.name) = $1 THEN 3 WHEN lower(o.name) LIKE $3 THEN 2
                            WHEN o.search_text LIKE $2 THEN 1 ELSE 0 END
                       + word_similarity($1, o.search_text))::real
               FROM organisations o
               WHERE $6 AND o.workspace_id = app_workspace_id() AND o.deleted_at IS NULL
                 AND (o.search_text LIKE $2 OR $1 <% o.search_text
                      OR EXISTS (SELECT 1 FROM jsonb_each_text(o.custom) e
                                 WHERE e.key = ANY($7) AND lower(e.value) LIKE $2))
               UNION ALL
               SELECT 'contact', c.id, c.name,
                      nullif(concat_ws(' · ', c.role, org.name, c.emails[1]), ''),
                      (CASE WHEN lower(c.name) = $1 THEN 3 WHEN lower(c.name) LIKE $3 THEN 2
                            WHEN c.search_text LIKE $2 THEN 1 ELSE 0 END
                       + word_similarity($1, c.search_text))::real
               FROM contacts c LEFT JOIN organisations org ON org.id = c.organisation_id
               WHERE $8 AND c.workspace_id = app_workspace_id() AND c.deleted_at IS NULL
                 AND (c.search_text LIKE $2 OR $1 <% c.search_text
                      OR EXISTS (SELECT 1 FROM jsonb_each_text(c.custom) e
                                 WHERE e.key = ANY($9) AND lower(e.value) LIKE $2))
           ) hits
           ORDER BY score DESC, name, id
           LIMIT $10"#,
        q,
        contains,
        prefix,
        scope.talent.is_some(),
        scope.talent.unwrap_or(none),
        scope.organisations.is_some(),
        scope.organisations.unwrap_or(none),
        scope.contacts.is_some(),
        scope.contacts.unwrap_or(none),
        limit
    )
    .fetch_all(conn)
    .await?)
}

#[derive(Debug, Clone)]
pub struct TagCount {
    pub tag: String,
    pub count: i64,
}

/// Every tag in use on an entity's live records, most used first.
pub async fn tags(conn: &mut PgConnection, entity: &str) -> Result<Vec<TagCount>> {
    Ok(sqlx::query_as!(
        TagCount,
        r#"SELECT tag AS "tag!", count(*) AS "count!" FROM (
               SELECT unnest(tags) AS tag FROM talent
               WHERE $1 = 'talent' AND workspace_id = app_workspace_id() AND deleted_at IS NULL
               UNION ALL
               SELECT unnest(tags) FROM organisations
               WHERE $1 = 'organisation' AND workspace_id = app_workspace_id() AND deleted_at IS NULL
               UNION ALL
               SELECT unnest(tags) FROM contacts
               WHERE $1 = 'contact' AND workspace_id = app_workspace_id() AND deleted_at IS NULL
           ) t
           GROUP BY tag
           ORDER BY 2 DESC, 1"#,
        entity
    )
    .fetch_all(conn)
    .await?)
}
