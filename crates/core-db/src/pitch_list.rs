//! The pitching list: each organisation that may be pitched, with what the
//! ranking needs to know about it, for one item (talent or a project) or
//! for none. The scoring itself is done by the caller.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

/// What to list. `talent` and `project` pick the item, at most one.
#[derive(Debug, Clone, Default)]
pub struct Filter<'a> {
    pub talent: Option<Uuid>,
    pub project: Option<Uuid>,
    pub q: Option<&'a str>,
    pub tag: Option<&'a str>,
    pub country: Option<&'a str>,
    pub city: Option<&'a str>,
    pub status_key: Option<&'a str>,
    pub relationship: Option<&'a str>,
    /// A region: organisations it was picked for, and those with none
    /// picked in one of its countries.
    pub region: Option<(&'a str, &'a [String])>,
    /// Every region's key, so one picked that no longer exists counts as
    /// none.
    pub region_keys: &'a [String],
    /// Bookings of the item from now until then count as nearby.
    pub nearby_until: DateTime<Utc>,
    pub limit: i64,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: Uuid,
    pub name: String,
    pub city: Option<String>,
    pub country_code: Option<String>,
    pub region_key: Option<String>,
    pub relationship: Option<String>,
    pub status_key: Option<String>,
    pub follow_up_on: Option<NaiveDate>,
    pub tags: Vec<String>,
    pub buys_from_others: Vec<String>,
    /// Someone there has an email address or a phone number.
    pub reachable: bool,
    /// The item is on its "interested in" list, or its "potential matches".
    pub interested: bool,
    pub matched: bool,
    /// Confirmed or done bookings of the item with it (as buyer or venue),
    /// and the date of the latest one that has happened.
    pub booked: i64,
    pub booked_last: Option<DateTime<Utc>>,
    /// The item's next date in the same city, if there is one soon.
    pub nearby_at: Option<DateTime<Utc>>,
    /// The latest answer about the item (or about nothing in particular),
    /// and the latest action, with how many actions there were since that
    /// answer and whether one of them was answered.
    pub outcome_key: Option<String>,
    pub outcome_at: Option<DateTime<Utc>>,
    pub action_at: Option<DateTime<Utc>>,
    pub actions_since: i64,
    pub answered_since: bool,
    pub total: i64,
}

/// Organisations matching the filter, up to `limit`, by name; those marked
/// do not contact are never listed.
pub async fn candidates(conn: &mut PgConnection, f: &Filter<'_>) -> Result<Vec<Candidate>> {
    let pattern = f.q.map(crate::crm::like_contains);
    Ok(sqlx::query_as!(
        Candidate,
        r#"WITH item_bookings AS (
               SELECT b.organisation_id, b.venue_id, b.starts_at, b.status
               FROM bookings b
               WHERE b.workspace_id = app_workspace_id() AND b.deleted_at IS NULL
                 AND (($1::uuid IS NOT NULL AND EXISTS (
                          SELECT 1 FROM booking_talent bt
                          WHERE bt.workspace_id = b.workspace_id AND bt.booking_id = b.id AND bt.talent_id = $1))
                      OR ($2::uuid IS NOT NULL AND EXISTS (
                          SELECT 1 FROM project_bookings pb
                          WHERE pb.workspace_id = b.workspace_id AND pb.booking_id = b.id AND pb.project_id = $2)))
           ), worked AS (
               SELECT o_id, count(*) AS n, max(starts_at) FILTER (WHERE starts_at <= now()) AS last
               FROM (SELECT organisation_id AS o_id, starts_at FROM item_bookings
                     WHERE status IN ('confirmed', 'completed') AND organisation_id IS NOT NULL
                     UNION ALL
                     SELECT venue_id, starts_at FROM item_bookings
                     WHERE status IN ('confirmed', 'completed') AND venue_id IS NOT NULL
                       AND venue_id IS DISTINCT FROM organisation_id) x
               GROUP BY o_id
           ), upcoming AS (
               SELECT lower(btrim(v.city)) AS city, min(ib.starts_at) AS at
               FROM item_bookings ib
               JOIN organisations v ON v.workspace_id = app_workspace_id()
                                   AND v.id = coalesce(ib.venue_id, ib.organisation_id)
               WHERE ib.status IN ('hold', 'pending', 'confirmed') AND v.city IS NOT NULL
                 AND ib.starts_at > now() AND ib.starts_at <= $10
               GROUP BY lower(btrim(v.city))
           ), about AS (
               SELECT p.organisation_id, p.kind, p.type_key, p.happened_at, p.id, p.answered_at
               FROM pitch_log p
               LEFT JOIN thread_messages m ON m.workspace_id = p.workspace_id AND m.id = p.message_id
               WHERE p.workspace_id = app_workspace_id() AND p.deleted_at IS NULL
                 -- A pitch OnHold failed to send wasn't done.
                 AND m.status IS DISTINCT FROM 'failed'
                 AND (($1::uuid IS NULL AND $2::uuid IS NULL)
                      OR (p.talent_id IS NULL AND p.project_id IS NULL)
                      OR p.talent_id = $1 OR p.project_id = $2)
           ), answers AS (
               SELECT DISTINCT ON (organisation_id) organisation_id, type_key, happened_at
               FROM about WHERE kind = 'outcome'
               ORDER BY organisation_id, happened_at DESC, id DESC
           ), actions AS (
               SELECT a.organisation_id, max(a.happened_at) AS last,
                      count(*) FILTER (WHERE o.happened_at IS NULL OR a.happened_at > o.happened_at) AS since,
                      coalesce(bool_or(a.answered_at IS NOT NULL)
                          FILTER (WHERE o.happened_at IS NULL OR a.happened_at > o.happened_at), false) AS answered
               FROM about a LEFT JOIN answers o ON o.organisation_id = a.organisation_id
               WHERE a.kind = 'action'
               GROUP BY a.organisation_id
           )
           SELECT o.id, o.name, o.city, o.country_code::text AS country_code, o.region_key, o.relationship,
                  o.status_key, o.follow_up_on, o.tags, o.buys_from_others,
                  EXISTS (SELECT 1 FROM contacts c
                          WHERE c.workspace_id = o.workspace_id AND c.organisation_id = o.id
                            AND c.deleted_at IS NULL
                            AND (cardinality(c.emails) > 0 OR cardinality(c.phones) > 0)) AS "reachable!",
                  EXISTS (SELECT 1 FROM organisation_items i
                          WHERE i.workspace_id = o.workspace_id AND i.organisation_id = o.id
                            AND i.list = 'interested' AND (i.talent_id = $1 OR i.project_id = $2)) AS "interested!",
                  EXISTS (SELECT 1 FROM organisation_items i
                          WHERE i.workspace_id = o.workspace_id AND i.organisation_id = o.id
                            AND i.list = 'match' AND (i.talent_id = $1 OR i.project_id = $2)) AS "matched!",
                  coalesce(w.n, 0) AS "booked!", w.last AS "booked_last?",
                  u.at AS "nearby_at?",
                  ans.type_key AS "outcome_key?", ans.happened_at AS "outcome_at?",
                  act.last AS "action_at?", coalesce(act.since, 0) AS "actions_since!",
                  coalesce(act.answered, false) AS "answered_since!",
                  count(*) OVER () AS "total!"
           FROM organisations o
           LEFT JOIN worked w ON w.o_id = o.id
           LEFT JOIN upcoming u ON u.city = lower(btrim(o.city))
                               AND NOT EXISTS (SELECT 1 FROM item_bookings ib
                                               WHERE (ib.organisation_id = o.id OR ib.venue_id = o.id)
                                                 AND ib.starts_at = u.at)
           LEFT JOIN answers ans ON ans.organisation_id = o.id
           LEFT JOIN actions act ON act.organisation_id = o.id
           WHERE o.workspace_id = app_workspace_id() AND o.deleted_at IS NULL AND NOT o.do_not_contact
             AND ($3::text IS NULL OR o.search_text LIKE $4 OR $3 <% o.search_text)
             AND ($5::text IS NULL OR $5 = ANY (o.tags))
             AND ($6::text IS NULL OR o.country_code = $6)
             AND ($7::text IS NULL OR lower(btrim(o.city)) = lower(btrim($7)))
             AND ($8::text IS NULL OR o.status_key = $8)
             AND ($9::text IS NULL OR o.relationship = $9)
             AND ($12::text IS NULL OR o.region_key = $12
                  OR ((o.region_key IS NULL OR NOT o.region_key = ANY ($14))
                      AND o.country_code::text = ANY ($13)))
           ORDER BY lower(o.name), o.id
           LIMIT $11"#,
        f.talent,
        f.project,
        f.q.map(str::to_lowercase),
        pattern.map(|p| p.to_lowercase()),
        f.tag,
        f.country,
        f.city,
        f.status_key,
        f.relationship,
        f.nearby_until,
        f.limit,
        f.region.map(|(key, _)| key),
        f.region.map_or(&[][..], |(_, countries)| countries),
        f.region_keys
    )
    .fetch_all(conn)
    .await?)
}

/// A talent's or a project's name and tags; `None`
/// when there is no such item.
pub async fn item(
    conn: &mut PgConnection,
    talent: Option<Uuid>,
    project: Option<Uuid>,
) -> Result<Option<(String, Vec<String>)>> {
    Ok(sqlx::query!(
        r#"SELECT t.name AS "name!", t.tags AS "tags!" FROM talent t
           WHERE t.workspace_id = app_workspace_id() AND t.id = $1 AND t.deleted_at IS NULL
           UNION ALL
           SELECT p.name, p.tags FROM projects p
           WHERE p.workspace_id = app_workspace_id() AND p.id = $2 AND p.deleted_at IS NULL"#,
        talent,
        project
    )
    .fetch_optional(conn)
    .await?
    .map(|r| (r.name, r.tags)))
}
