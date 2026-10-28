//! Acting on a CRM record by its entity's name: what bulk actions, imports
//! and automation rules share.

use core_db::Tx;
use core_domain::{Ctx, Permission};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{begin, contacts, organisations, talent};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

/// The most records one bulk action touches.
pub const MAX_BULK: usize = 500;

pub fn write_permission(entity: &str) -> Option<Permission> {
    match entity {
        talent::ENTITY => Some(Permission::TalentWrite),
        organisations::ENTITY => Some(Permission::OrganisationWrite),
        contacts::ENTITY => Some(Permission::ContactWrite),
        _ => None,
    }
}

pub fn is_crm(entity: &str) -> bool {
    super::crm::ENTITIES.contains(&entity)
}

/// The record as events, rules and templates see it.
pub(crate) async fn load_json(tx: &mut Tx, entity: &str, id: Uuid) -> ApiResult<Option<Value>> {
    Ok(match entity {
        talent::ENTITY => talent::load(tx, id).await?.map(|v| talent::to_json(&v)),
        organisations::ENTITY => organisations::load(tx, id)
            .await?
            .map(|v| organisations::to_json(&v)),
        contacts::ENTITY => contacts::load(tx, id).await?.map(|v| contacts::to_json(&v)),
        _ => None,
    })
}

/// The record's tags.
async fn tags_of(tx: &mut Tx, entity: &str, id: Uuid) -> ApiResult<Vec<String>> {
    let json = load_json(tx, entity, id).await?.ok_or(ApiError::NotFound)?;
    Ok(serde_json::from_value(json.get("tags").cloned().unwrap_or_default()).unwrap_or_default())
}

/// A change to some of a record's fields: its name, tags or custom values.
#[derive(Debug, Clone, Default)]
pub(crate) struct Edit {
    pub name: Option<String>,
    pub tags: Option<Vec<String>>,
    pub custom: Option<Map<String, Value>>,
}

pub(crate) async fn edit(
    tx: &mut Tx,
    ctx: &Ctx,
    entity: &str,
    id: Uuid,
    e: Edit,
    event_extra: Option<Value>,
) -> ApiResult<()> {
    match entity {
        talent::ENTITY => {
            let patch = talent::TalentPatch {
                name: e.name,
                tags: e.tags,
                custom: e.custom,
                ..Default::default()
            };
            talent::update_in_tx(tx, ctx, id, patch, None, event_extra).await?;
        }
        organisations::ENTITY => {
            let patch = organisations::OrganisationPatch {
                name: e.name,
                tags: e.tags,
                custom: e.custom,
                ..Default::default()
            };
            organisations::update_in_tx(tx, ctx, id, patch, None, event_extra).await?;
        }
        contacts::ENTITY => {
            let patch = contacts::ContactPatch {
                name: e.name,
                tags: e.tags,
                custom: e.custom,
                ..Default::default()
            };
            contacts::update_in_tx(tx, ctx, id, patch, None, event_extra).await?;
        }
        _ => return Err(ApiError::NotFound),
    }
    Ok(())
}

pub(crate) async fn delete(
    tx: &mut Tx,
    ctx: &Ctx,
    entity: &str,
    id: Uuid,
    event_extra: Option<Value>,
) -> ApiResult<()> {
    match entity {
        talent::ENTITY => talent::delete_in_tx(tx, ctx, id, event_extra).await,
        organisations::ENTITY => organisations::delete_in_tx(tx, ctx, id, event_extra).await,
        contacts::ENTITY => contacts::delete_in_tx(tx, ctx, id, event_extra).await,
        _ => Err(ApiError::NotFound),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BulkAction {
    Delete,
    AddTags,
    RemoveTags,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct BulkRequest {
    pub action: BulkAction,
    /// Up to 500 records.
    pub ids: Vec<Uuid>,
    /// For `add_tags` and `remove_tags`.
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BulkFailure {
    pub id: Uuid,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BulkResult {
    /// Records changed.
    pub done: usize,
    /// Records left unchanged, and why.
    pub failed: Vec<BulkFailure>,
}

/// One action on many records, all in one transaction. A record that
/// cannot take the action is reported and skipped; the rest go ahead.
pub async fn bulk(
    state: &AppState,
    ctx: &Ctx,
    entity: &str,
    req: BulkRequest,
) -> ApiResult<BulkResult> {
    let permission = write_permission(entity).ok_or(ApiError::NotFound)?;
    ctx.authorize(permission)?;
    if req.ids.is_empty() || req.ids.len() > MAX_BULK {
        return Err(ApiError::invalid(
            "ids",
            format!("give 1 to {MAX_BULK} ids"),
        ));
    }
    let mut check = super::crm::Check::default();
    let tags = check.tags(&req.tags);
    if req.action != BulkAction::Delete && tags.is_empty() {
        check.issue("tags", "name at least one tag");
    }
    check.finish()?;

    let mut tx = begin(state, ctx).await?;
    let mut done = 0;
    let mut failed = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut ids = req.ids.clone();
    ids.retain(|id| seen.insert(*id));
    for id in ids {
        let result = match req.action {
            BulkAction::Delete => delete(&mut tx, ctx, entity, id, None).await,
            BulkAction::AddTags | BulkAction::RemoveTags => {
                match tags_of(&mut tx, entity, id).await {
                    Ok(mut current) => {
                        if req.action == BulkAction::AddTags {
                            current.extend(tags.iter().cloned());
                        } else {
                            current.retain(|t| !tags.contains(t));
                        }
                        let e = Edit {
                            tags: Some(current),
                            ..Default::default()
                        };
                        edit(&mut tx, ctx, entity, id, e, None).await
                    }
                    Err(err) => Err(err),
                }
            }
        };
        match result {
            Ok(()) => done += 1,
            Err(err @ ApiError::Internal(_)) => return Err(err),
            Err(ApiError::NotFound) => failed.push(BulkFailure {
                id,
                message: "not found".into(),
            }),
            Err(err) => failed.push(BulkFailure {
                id,
                message: err.to_string(),
            }),
        }
    }
    tx.commit().await?;
    Ok(BulkResult { done, failed })
}
