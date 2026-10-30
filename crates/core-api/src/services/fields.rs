//! Custom field definitions: the per-workspace extra fields every entity
//! form and list is rendered from.

use core_db::Tx;
use core_db::fields::FieldDef as Row;
use core_domain::{Ctx, Permission};
use core_engines::fields::{self as engine, FieldType, Validation};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Change, begin, check_version, optional_text, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

/// Entities that take custom fields. Each module adds its own as it lands.
pub const ENTITIES: &[&str] = &[
    "talent",
    "organisation",
    "contact",
    "enquiry",
    "booking",
    "thing",
];

pub fn check_entity(entity: &str) -> ApiResult<()> {
    if ENTITIES.contains(&entity) {
        Ok(())
    } else {
        Err(ApiError::invalid(
            "entity",
            format!(
                "`{entity}` does not take custom fields; use one of: {}",
                ENTITIES.join(", ")
            ),
        ))
    }
}

/// The engine's view of a stored definition.
pub fn to_engine(row: &Row) -> Option<engine::FieldDef> {
    Some(engine::FieldDef {
        key: row.key.clone(),
        label: row.label.clone(),
        field_type: FieldType::parse(&row.field_type)?,
        required: row.required,
        default_value: row.default_value.clone(),
        validation: serde_json::from_value(row.validation.clone()).unwrap_or_default(),
        archived: row.archived_at.is_some(),
    })
}

/// All of an entity's definitions (archived included) for validating writes.
pub(crate) async fn engine_defs(tx: &mut Tx, entity: &str) -> ApiResult<Vec<engine::FieldDef>> {
    Ok(core_db::fields::list(tx, Some(entity), true)
        .await?
        .iter()
        .filter_map(to_engine)
        .collect())
}

pub async fn list(
    state: &AppState,
    ctx: &Ctx,
    entity: Option<&str>,
    include_archived: bool,
) -> ApiResult<Vec<Row>> {
    ctx.authorize(Permission::WorkspaceRead)?;
    if let Some(e) = entity {
        check_entity(e)?;
    }
    let mut tx = begin(state, ctx).await?;
    let rows = core_db::fields::list(&mut tx, entity, include_archived).await?;
    tx.commit().await?;
    Ok(rows)
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<Row> {
    ctx.authorize(Permission::WorkspaceRead)?;
    let mut tx = begin(state, ctx).await?;
    let row = core_db::fields::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(row)
}

/// Everything about a field except its entity, key and type, which never change.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub label: String,
    pub required: bool,
    pub default_value: Option<Value>,
    pub validation: Validation,
    pub visibility: String,
    pub searchable: bool,
    pub group: Option<String>,
    pub position: Option<i32>,
    pub help_text: Option<String>,
}

fn check_settings(
    key: &str,
    field_type: FieldType,
    s: Settings,
) -> ApiResult<(Settings, engine::FieldDef)> {
    if !matches!(s.visibility.as_str(), "staff" | "portal" | "both") {
        return Err(ApiError::invalid("visibility", "use staff, portal or both"));
    }
    let group = optional_text("group", s.group, 100)?;
    let help_text = optional_text("help_text", s.help_text, 1000)?;
    let def = engine::FieldDef {
        key: key.to_owned(),
        label: s.label.trim().to_owned(),
        field_type,
        required: s.required,
        default_value: s.default_value.clone().filter(|v| !v.is_null()),
        validation: s.validation.clone(),
        archived: false,
    };
    engine::check_definition(&def)?;
    // Store the default in its canonical form.
    let default_value = match &def.default_value {
        Some(v) => {
            Some(engine::normalize(&def, v).map_err(|m| ApiError::invalid("default_value", m))?)
        }
        None => None,
    };
    Ok((
        Settings {
            label: def.label.clone(),
            default_value,
            group,
            help_text,
            ..s
        },
        def,
    ))
}

fn values<'a>(
    s: &'a Settings,
    validation: &'a Value,
    position: i32,
) -> core_db::fields::Values<'a> {
    core_db::fields::Values {
        label: &s.label,
        required: s.required,
        default_value: s.default_value.as_ref(),
        validation,
        visibility: &s.visibility,
        searchable: s.searchable,
        field_group: s.group.as_deref(),
        position,
        help_text: s.help_text.as_deref(),
    }
}

fn view_json(row: &Row) -> Value {
    json!({
        "entity": row.entity, "key": row.key, "label": row.label, "field_type": row.field_type,
        "required": row.required, "default_value": row.default_value, "validation": row.validation,
        "visibility": row.visibility, "searchable": row.searchable, "group": row.field_group,
        "position": row.position, "help_text": row.help_text,
    })
}

pub async fn create(
    state: &AppState,
    ctx: &Ctx,
    entity: &str,
    key: &str,
    field_type: FieldType,
    settings: Settings,
) -> ApiResult<Row> {
    ctx.authorize(Permission::ConfigWrite)?;
    check_entity(entity)?;
    let (settings, _) = check_settings(key, field_type, settings)?;
    let validation = serde_json::to_value(&settings.validation).map_err(ApiError::internal)?;
    let mut tx = begin(state, ctx).await?;
    let position = match settings.position {
        Some(p) => p,
        None => {
            let n = core_db::fields::list(&mut tx, Some(entity), false)
                .await?
                .len();
            i32::try_from(n).unwrap_or(i32::MAX)
        }
    };
    let id = core_domain::ids::new_id();
    let inserted = core_db::fields::insert(
        &mut tx,
        id,
        entity,
        key,
        field_type.as_str(),
        values(&settings, &validation, position),
        ctx.user_id(),
    )
    .await;
    if let Err(err) = inserted {
        return Err(if err.is_unique_violation(None) {
            ApiError::conflict(
                "field_key_taken",
                format!("{entity} already has (or had) a field `{key}`; keys are never reused"),
            )
        } else {
            err.into()
        });
    }
    let row = core_db::fields::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "custom_field.created",
            resource: "custom_field",
            object_id: Some(id),
            before: None,
            after: Some(view_json(&row)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// Fields left `None` keep their value.
#[derive(Debug, Clone, Default)]
pub struct Patch {
    pub label: Option<String>,
    pub required: Option<bool>,
    /// `Some(None)` clears the default.
    pub default_value: Option<Option<Value>>,
    pub validation: Option<Validation>,
    pub visibility: Option<String>,
    pub searchable: Option<bool>,
    pub group: Option<Option<String>>,
    pub position: Option<i32>,
    pub help_text: Option<Option<String>>,
}

pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    patch: Patch,
    if_match: Option<i64>,
) -> ApiResult<Row> {
    ctx.authorize(Permission::ConfigWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = core_db::fields::get(&mut tx, id)
        .await?
        .filter(|r| r.archived_at.is_none())
        .ok_or(ApiError::NotFound)?;
    check_version(before.version, if_match)?;
    let field_type = FieldType::parse(&before.field_type)
        .ok_or_else(|| ApiError::internal("stored field has an unknown type"))?;
    let current = Settings {
        label: before.label.clone(),
        required: before.required,
        default_value: before.default_value.clone(),
        validation: serde_json::from_value(before.validation.clone()).unwrap_or_default(),
        visibility: before.visibility.clone(),
        searchable: before.searchable,
        group: before.field_group.clone(),
        position: Some(before.position),
        help_text: before.help_text.clone(),
    };
    let next = Settings {
        label: patch.label.unwrap_or(current.label),
        required: patch.required.unwrap_or(current.required),
        default_value: patch.default_value.unwrap_or(current.default_value),
        validation: patch.validation.unwrap_or(current.validation),
        visibility: patch.visibility.unwrap_or(current.visibility),
        searchable: patch.searchable.unwrap_or(current.searchable),
        group: patch.group.unwrap_or(current.group),
        position: patch.position.or(current.position),
        help_text: patch.help_text.unwrap_or(current.help_text),
    };
    let (next, _) = check_settings(&before.key, field_type, next)?;
    let validation = serde_json::to_value(&next.validation).map_err(ApiError::internal)?;
    let position = next.position.unwrap_or(before.position);
    if !core_db::fields::update(
        &mut tx,
        id,
        values(&next, &validation, position),
        if_match,
        ctx.user_id(),
    )
    .await?
    {
        return Err(ApiError::version_conflict());
    }
    let after = core_db::fields::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "custom_field.updated",
            resource: "custom_field",
            object_id: Some(id),
            before: Some(view_json(&before)),
            after: Some(view_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// Removes a field from forms and lists. Stored values stay in each row's
/// `custom` data; the key is never reused.
pub async fn archive(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::ConfigWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = core_db::fields::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !core_db::fields::archive(&mut tx, id, ctx.user_id()).await? {
        return Err(ApiError::conflict(
            "already_archived",
            "this field was already removed",
        ));
    }
    record(
        &mut tx,
        ctx,
        Change {
            topic: "custom_field.archived",
            resource: "custom_field",
            object_id: Some(id),
            before: Some(view_json(&before)),
            after: None,
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
