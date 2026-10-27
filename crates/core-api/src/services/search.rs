//! Global search across the CRM, enquiries and bookings (messages join in M6),
//! the workspace's tags, and the activity timeline of a record.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use core_domain::{Ctx, Permission};
use serde::Serialize;
use serde_json::{Map, Value};
use utoipa::ToSchema;
use uuid::Uuid;

use super::begin;
use crate::AppState;
use crate::error::{ApiError, ApiResult};

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchHit {
    /// `talent`, `organisation`, `contact`, `enquiry` or `booking`.
    pub entity: String,
    pub id: Uuid,
    pub name: String,
    /// A second line to tell similar names apart.
    pub detail: Option<String>,
}

fn read_permission(entity: &str) -> Option<Permission> {
    match entity {
        "talent" => Some(Permission::TalentRead),
        "organisation" => Some(Permission::OrganisationRead),
        "contact" => Some(Permission::ContactRead),
        "enquiry" => Some(Permission::EnquiryRead),
        "booking" => Some(Permission::BookingRead),
        _ => None,
    }
}

pub(crate) fn authorize_read(ctx: &Ctx, entity: &str) -> ApiResult<()> {
    let p = read_permission(entity).ok_or_else(|| {
        ApiError::invalid(
            "entity",
            format!(
                "use one of: {}, enquiry, booking",
                super::crm::ENTITIES.join(", ")
            ),
        )
    })?;
    ctx.authorize(p)?;
    Ok(())
}

/// The best matches across every record type the caller may read.
pub async fn search(
    state: &AppState,
    ctx: &Ctx,
    q: &str,
    entities: Option<&[String]>,
    limit: i64,
) -> ApiResult<Vec<SearchHit>> {
    let q = q.trim().to_lowercase();
    if q.chars().count() > 200 {
        return Err(ApiError::invalid("q", "must be at most 200 characters"));
    }
    let wanted = |e: &str| {
        entities.is_none_or(|list| list.iter().any(|x| x == e))
            && read_permission(e).is_some_and(|p| ctx.permissions.contains(&p))
    };
    let work = (wanted("enquiry"), wanted("booking"));
    if q.is_empty() || !(super::crm::ENTITIES.iter().any(|e| wanted(e)) || work.0 || work.1) {
        return Ok(Vec::new());
    }
    let mut tx = begin(state, ctx).await?;
    let mut keys = HashMap::new();
    for e in super::crm::ENTITIES {
        if wanted(e) {
            keys.insert(*e, super::crm::searchable(&mut tx, e).await?);
        }
    }
    let limit = limit.clamp(1, 50);
    let mut hits = core_db::search::search(
        &mut tx,
        &q,
        core_db::search::Scope {
            talent: keys.get("talent").map(Vec::as_slice),
            organisations: keys.get("organisation").map(Vec::as_slice),
            contacts: keys.get("contact").map(Vec::as_slice),
        },
        limit,
    )
    .await?;
    if work.0 || work.1 {
        hits.extend(core_db::search::search_work(&mut tx, &q, work.0, work.1, limit).await?);
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.name.cmp(&b.name))
        });
        hits.truncate(limit as usize);
    }
    tx.commit().await?;
    Ok(hits
        .into_iter()
        .map(|h| SearchHit {
            entity: h.entity,
            id: h.id,
            name: h.name,
            detail: h.detail,
        })
        .collect())
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TagCount {
    pub tag: String,
    /// Records carrying it.
    pub count: i64,
}

/// The tags in use on an entity's records, most used first.
pub async fn tags(state: &AppState, ctx: &Ctx, entity: &str) -> ApiResult<Vec<TagCount>> {
    authorize_read(ctx, entity)?;
    let mut tx = begin(state, ctx).await?;
    let rows = core_db::search::tags(&mut tx, entity).await?;
    tx.commit().await?;
    Ok(rows
        .into_iter()
        .map(|t| TagCount {
            tag: t.tag,
            count: t.count,
        })
        .collect())
}

// Timeline ----------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TimelineActor {
    /// `user`, `api_key`, `rule` or `system`.
    pub kind: String,
    pub id: Option<Uuid>,
    /// The member's name, when the actor is a current member.
    pub name: Option<String>,
    /// Set when an operator acted on the workspace's behalf.
    pub impersonator_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FieldChange {
    /// `name`, `emails`, `custom.genre`…
    pub field: String,
    #[schema(value_type = Object)]
    pub before: Value,
    #[schema(value_type = Object)]
    pub after: Value,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TimelineMessage {
    pub id: Uuid,
    pub channel: String,
    pub to: String,
    pub subject: Option<String>,
    /// `sent` or `failed`.
    pub status: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TimelineEntry {
    pub id: Uuid,
    pub at: DateTime<Utc>,
    /// The audit action (`contact.updated`, `talent_member.created`), or
    /// `message.sent` for a message.
    pub action: String,
    /// What the entry is about: `contact`, `talent_member`…
    pub resource: String,
    pub object_id: Option<Uuid>,
    /// The name of what the entry is about, when it is not the record
    /// itself (a member of the act, a contact at the organisation).
    pub object_name: Option<String>,
    pub actor: Option<TimelineActor>,
    /// For updates: what changed.
    pub changes: Vec<FieldChange>,
    pub message: Option<TimelineMessage>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TimelinePage {
    pub entries: Vec<TimelineEntry>,
    /// Pass as `before` for older entries; absent at the end.
    pub next_before: Option<Uuid>,
}

/// Fields that change on every write and say nothing to a reader.
const BOOKKEEPING: &[&str] = &[
    "version",
    "updated_at",
    "updated_by",
    "created_at",
    "created_by",
    "email",
    "phone",
    "organisation_name",
    "contact_count",
];

/// What differs between two snapshots, custom fields one by one.
pub fn diff(before: &Value, after: &Value) -> Vec<FieldChange> {
    let empty = Map::new();
    let b = before.as_object().unwrap_or(&empty);
    let a = after.as_object().unwrap_or(&empty);
    let mut keys: Vec<&String> = b.keys().chain(a.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut out = Vec::new();
    for key in keys {
        if BOOKKEEPING.contains(&key.as_str()) {
            continue;
        }
        let (bv, av) = (
            b.get(key).unwrap_or(&Value::Null),
            a.get(key).unwrap_or(&Value::Null),
        );
        if bv == av {
            continue;
        }
        if key == "custom" {
            for change in diff(bv, av) {
                out.push(FieldChange {
                    field: format!("custom.{}", change.field),
                    ..change
                });
            }
        } else {
            out.push(FieldChange {
                field: key.clone(),
                before: bv.clone(),
                after: av.clone(),
            });
        }
    }
    out
}

fn actor(e: &core_db::audit::ObjectEntry) -> Option<TimelineActor> {
    let impersonator_id = e.impersonator_id.map(|u| u.0);
    if let Some(u) = e.actor_user_id {
        Some(TimelineActor {
            kind: "user".into(),
            id: Some(u.0),
            name: e.actor_name.clone(),
            impersonator_id,
        })
    } else if let Some(k) = e.actor_api_key_id {
        Some(TimelineActor {
            kind: "api_key".into(),
            id: Some(k),
            name: None,
            impersonator_id,
        })
    } else if let Some(r) = e.actor_rule_id {
        Some(TimelineActor {
            kind: "rule".into(),
            id: Some(r),
            name: None,
            impersonator_id,
        })
    } else {
        Some(TimelineActor {
            kind: "system".into(),
            id: None,
            name: None,
            impersonator_id,
        })
    }
}

fn snapshot_name(e: &core_db::audit::ObjectEntry) -> Option<String> {
    e.after
        .as_ref()
        .or(e.before.as_ref())
        .and_then(|v| v.get("name"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// A record's history, newest first: its own changes, those of the records
/// that belong to it (an act's members, an organisation's contacts), and
/// the messages sent to it.
pub async fn timeline(
    state: &AppState,
    ctx: &Ctx,
    entity: &str,
    id: Uuid,
    before: Option<Uuid>,
    limit: i64,
) -> ApiResult<TimelinePage> {
    authorize_read(ctx, entity)?;
    let limit = limit.clamp(1, 200);
    let mut tx = begin(state, ctx).await?;
    let mut objects = vec![id];
    let mut addresses: Vec<String> = Vec::new();
    match entity {
        "talent" => {
            super::talent::load(&mut tx, id)
                .await?
                .ok_or(ApiError::NotFound)?;
            objects.extend(core_db::talent::member_ids(&mut tx, id).await?);
        }
        "organisation" => {
            super::organisations::load(&mut tx, id)
                .await?
                .ok_or(ApiError::NotFound)?;
            if ctx.permissions.contains(&Permission::ContactRead) {
                let contacts = core_db::contacts::ids_at(&mut tx, id).await?;
                for c in &contacts {
                    if let Some(v) = super::contacts::load(&mut tx, *c).await? {
                        addresses.extend(v.emails);
                    }
                }
                objects.extend(contacts);
            }
        }
        "enquiry" => {
            core_db::enquiries::get(&mut tx, id)
                .await?
                .ok_or(ApiError::NotFound)?;
            if ctx.permissions.contains(&Permission::BookingRead) {
                let (bookings, _) = core_db::bookings::list(
                    &mut tx,
                    core_db::bookings::Filter {
                        list: no_search(),
                        from: None,
                        to: None,
                        statuses: &[],
                        talent_id: None,
                        organisation_id: None,
                        contact_id: None,
                        enquiry_id: Some(id),
                    },
                )
                .await?;
                objects.extend(bookings.into_iter().map(|b| b.id));
            }
        }
        "booking" => {
            core_db::bookings::get(&mut tx, id)
                .await?
                .ok_or(ApiError::NotFound)?;
            if ctx.permissions.contains(&Permission::QuoteRead) {
                let (quotes, _) = core_db::quotes::list(&mut tx, Some(id), &[], 0, 200).await?;
                objects.extend(quotes.into_iter().map(|q| q.id));
            }
        }
        _ => {
            let c = super::contacts::load(&mut tx, id)
                .await?
                .ok_or(ApiError::NotFound)?;
            addresses = c.emails;
        }
    }
    let audit = core_db::audit::for_objects(&mut tx, &objects, before, limit).await?;
    let messages = if addresses.is_empty() {
        Vec::new()
    } else {
        core_db::messages::to_addresses(&mut tx, &addresses, before, limit).await?
    };
    tx.commit().await?;

    let mut entries: Vec<TimelineEntry> = audit
        .into_iter()
        .map(|e| {
            let changes = match (&e.before, &e.after) {
                (Some(b), Some(a)) => diff(b, a),
                _ => Vec::new(),
            };
            TimelineEntry {
                id: e.id,
                at: e.at,
                action: e.action.clone(),
                resource: e.resource.clone(),
                object_id: e.object_id,
                object_name: (e.object_id != Some(id))
                    .then(|| snapshot_name(&e))
                    .flatten(),
                actor: actor(&e),
                changes,
                message: None,
            }
        })
        .chain(messages.into_iter().map(|m| TimelineEntry {
            id: m.id,
            at: m.created_at,
            action: "message.sent".into(),
            resource: "message".into(),
            object_id: Some(m.id),
            object_name: None,
            actor: m.rule_id.map(|r| TimelineActor {
                kind: "rule".into(),
                id: Some(r),
                name: None,
                impersonator_id: None,
            }),
            changes: Vec::new(),
            message: Some(TimelineMessage {
                id: m.id,
                channel: m.channel,
                to: m.to_address,
                subject: m.subject,
                status: m.status,
            }),
        }))
        .collect();
    // Ids are time-ordered (UUIDv7), so they order the merged sources.
    entries.sort_by_key(|e| std::cmp::Reverse(e.id));
    entries.truncate(limit as usize);
    let next_before = (entries.len() as i64 == limit)
        .then(|| entries.last().map(|e| e.id))
        .flatten();
    Ok(TimelinePage {
        entries,
        next_before,
    })
}

/// A whole list in one page, unsearched.
fn no_search() -> core_db::crm::ListParams<'static> {
    core_db::crm::ListParams {
        q: None,
        searchable: &[],
        tag: None,
        custom: None,
        sort: "created_at",
        sort_custom: None,
        desc: false,
        offset: 0,
        limit: 200,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn diffs_list_changed_fields_and_custom_fields_one_by_one() {
        let before =
            json!({"name": "Jo", "version": 1, "tags": [], "custom": {"genre": "jazz", "size": 2}});
        let after =
            json!({"name": "Jo", "version": 2, "tags": ["vip"], "custom": {"genre": "soul"}});
        let changes = diff(&before, &after);
        let fields: Vec<&str> = changes.iter().map(|c| c.field.as_str()).collect();
        assert_eq!(fields, ["custom.genre", "custom.size", "tags"]);
        assert_eq!(changes[0].before, json!("jazz"));
        assert_eq!(changes[1].after, Value::Null);
    }
}
