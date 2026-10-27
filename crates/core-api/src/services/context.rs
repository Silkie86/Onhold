//! The context tree rules and templates see: `workspace`, the object
//! (`thing`), its state `before` a change, and the triggering `event`.
//! Documented by [`variables`] so the template editor can list them.

use chrono::Utc;
use core_db::Tx;
use core_engines::fields::FieldType;
use core_engines::templates::Options;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::AppState;
use crate::error::{ApiError, ApiResult};

/// The workspace part of the context, and how to format dates and money for it.
pub(crate) async fn workspace(tx: &mut Tx, state: &AppState) -> ApiResult<(Value, Options)> {
    let id = sqlx::query_scalar!(r#"SELECT app_workspace_id() AS "id!""#)
        .fetch_one(&mut **tx)
        .await?;
    let w = core_db::workspaces::get(tx, core_domain::WorkspaceId(id))
        .await?
        .ok_or(ApiError::NotFound)?;
    let options = Options {
        locale: w.locale.clone(),
        timezone: w.default_timezone.parse().unwrap_or(chrono_tz::Tz::UTC),
    };
    let json = json!({
        "id": w.id,
        "name": w.name,
        "slug": w.slug,
        "country": w.country.trim(),
        "currency": w.default_currency.trim(),
        "timezone": w.default_timezone,
        "locale": w.locale,
        "app_url": state.config.public_base_url.as_str(),
    });
    Ok((json, options))
}

/// Adds the object's link in the web app.
pub(crate) fn with_url(state: &AppState, mut thing: Value) -> Value {
    if let Some(id) = thing.get("id").and_then(Value::as_str) {
        let url = state.config.app_url(&format!("/things/{id}"), &[]);
        if let Some(obj) = thing.as_object_mut() {
            obj.insert("url".into(), Value::String(url));
        }
    }
    thing
}

pub(crate) fn build(
    workspace: Value,
    thing: Option<Value>,
    before: Option<Value>,
    event: Option<Value>,
) -> Value {
    json!({
        "workspace": workspace,
        "thing": thing,
        "before": before,
        "event": event,
        "now": Utc::now(),
    })
}

/// Plausible values for each custom field, for previews without a real record.
pub(crate) async fn sample_thing(tx: &mut Tx, state: &AppState) -> ApiResult<Value> {
    let defs = core_db::fields::list(tx, Some(super::things::ENTITY), false).await?;
    let mut custom = Map::new();
    for d in &defs {
        let Some(def) = super::fields::to_engine(d) else {
            continue;
        };
        let value = match def.field_type {
            FieldType::Text | FieldType::LongText => json!(format!("Sample {}", def.label)),
            FieldType::Number => json!(42),
            FieldType::Money => json!({
                "minor": 125_000,
                "currency": def.validation.currency.clone().unwrap_or_else(|| "GBP".into())
            }),
            FieldType::Date => json!("2027-03-07"),
            FieldType::Datetime => json!("2027-03-07T19:30:00Z"),
            FieldType::Boolean => json!(true),
            FieldType::Select => json!(def.validation.options.first().map(|o| o.value.clone())),
            FieldType::MultiSelect => {
                json!(
                    def.validation
                        .options
                        .iter()
                        .take(2)
                        .map(|o| o.value.clone())
                        .collect::<Vec<_>>()
                )
            }
            FieldType::Url => json!("https://example.com"),
            FieldType::Email => json!("someone@example.com"),
            FieldType::Phone => json!("+44 20 7946 0000"),
            FieldType::File | FieldType::Relation => json!(core_domain::ids::new_id()),
        };
        custom.insert(def.key.clone(), value);
    }
    let id = core_domain::ids::new_id();
    Ok(with_url(
        state,
        json!({
            "id": id,
            "name": "Sample record",
            "pipeline_id": null,
            "stage": "new",
            "stage_entered_at": Utc::now(),
            "due_at": Utc::now() + chrono::Duration::days(14),
            "custom": custom,
            "version": 1,
            "created_at": Utc::now(),
            "updated_at": Utc::now(),
        }),
    ))
}

/// One variable a template can use.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Variable {
    pub path: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field_type: Option<String>,
}

fn var(path: &str, description: &str) -> Variable {
    Variable {
        path: path.into(),
        description: description.into(),
        field_type: None,
    }
}

/// Every variable available to templates about `entity`, including its
/// custom fields.
pub(crate) async fn variables(tx: &mut Tx, entity: &str) -> ApiResult<Vec<Variable>> {
    let mut out = vec![
        var("workspace.name", "The workspace's name"),
        var("workspace.slug", "The workspace's short name"),
        var("workspace.country", "ISO country code"),
        var("workspace.currency", "Default currency code"),
        var(
            "workspace.timezone",
            "Default timezone; dates are shown in it",
        ),
        var("workspace.app_url", "Link to the web app"),
        var("now", "When the message is rendered"),
        var(
            "event.topic",
            "The event that triggered the rule, e.g. thing.created",
        ),
        var("event.at", "When the event happened"),
    ];
    if entity == super::things::ENTITY {
        out.extend([
            var("thing.id", "The record's id"),
            var("thing.name", "Name"),
            var("thing.stage", "Current stage key"),
            var(
                "thing.stage_entered_at",
                "When it entered the current stage",
            ),
            var("thing.due_at", "Due date and time"),
            var("thing.url", "Link to the record in the web app"),
            var("thing.created_at", "When it was created"),
            var("before.name", "Name before the change (update rules only)"),
            var(
                "before.stage",
                "Stage before the change (update rules only)",
            ),
        ]);
        for d in core_db::fields::list(tx, Some(entity), false).await? {
            out.push(Variable {
                path: format!("thing.custom.{}", d.key),
                description: d.label,
                field_type: Some(d.field_type),
            });
        }
    }
    Ok(out)
}
