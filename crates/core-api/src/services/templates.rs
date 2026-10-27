//! Message templates: CRUD, syntax checks on save, live preview and the
//! documented variable list.

use std::collections::BTreeMap;

use core_db::Tx;
use core_db::templates::Template;
use core_domain::{Ctx, Permission};
use core_engines::templates::{self as engine, Kind, Options, Rendered};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::context::{self, Variable};
use super::{Change, begin, check_version, optional_text, record, required_text};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

/// Keys under `system.` belong to platform mail; a workspace may override
/// these ones with its own wording.
pub const OVERRIDABLE_SYSTEM_KEYS: &[&str] =
    &[super::mail::SYSTEM_INVITATION, super::mail::SYSTEM_QUOTE];

fn check_key(key: &str) -> ApiResult<()> {
    let ok = !key.is_empty()
        && key.len() <= 100
        && key.split('.').all(|seg| {
            let mut b = seg.bytes();
            b.next().is_some_and(|c| c.is_ascii_lowercase())
                && b.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
        });
    if !ok {
        return Err(ApiError::invalid(
            "key",
            "use lowercase words separated by dots, e.g. `welcome` or `quote.chase`",
        ));
    }
    if key.starts_with("system.") && !OVERRIDABLE_SYSTEM_KEYS.contains(&key) {
        return Err(ApiError::invalid(
            "key",
            format!(
                "`system.` keys are reserved; you can override: {}",
                OVERRIDABLE_SYSTEM_KEYS.join(", ")
            ),
        ));
    }
    Ok(())
}

fn check_source(kind: Kind, subject: Option<&str>, body: &str) -> ApiResult<()> {
    if kind.has_subject() && subject.is_none_or(|s| s.trim().is_empty()) {
        return Err(ApiError::invalid(
            "subject",
            "is required for this kind of template",
        ));
    }
    if !kind.has_subject() && subject.is_some() {
        return Err(ApiError::invalid(
            "subject",
            "this kind of template has no subject",
        ));
    }
    if body.len() > 200_000 {
        return Err(ApiError::invalid(
            "body",
            "must be at most 200,000 characters",
        ));
    }
    engine::check(kind, subject, body).map_err(template_error)
}

pub(crate) fn template_error(e: engine::TemplateError) -> ApiError {
    let field = if e.part == "subject" {
        "subject"
    } else {
        "body"
    };
    ApiError::invalid(field, e.to_string())
}

fn view_json(t: &Template) -> Value {
    json!({ "kind": t.kind, "key": t.key, "locale": t.locale, "name": t.name, "subject": t.subject,
            "body": t.body, "description": t.description })
}

pub async fn list(state: &AppState, ctx: &Ctx, kind: Option<Kind>) -> ApiResult<Vec<Template>> {
    ctx.authorize(Permission::WorkspaceRead)?;
    let mut tx = begin(state, ctx).await?;
    let rows = core_db::templates::list(&mut tx, kind.map(Kind::as_str)).await?;
    tx.commit().await?;
    Ok(rows)
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<Template> {
    ctx.authorize(Permission::WorkspaceRead)?;
    let mut tx = begin(state, ctx).await?;
    let t = core_db::templates::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(t)
}

pub struct New {
    pub kind: Kind,
    pub key: String,
    pub locale: Option<String>,
    pub name: String,
    pub subject: Option<String>,
    pub body: String,
    pub description: Option<String>,
}

pub async fn create(state: &AppState, ctx: &Ctx, new: New) -> ApiResult<Template> {
    ctx.authorize(Permission::ConfigWrite)?;
    check_key(&new.key)?;
    let locale = new.locale.unwrap_or_else(|| "en".into());
    core_domain::workspace::validate_locale(&locale)?;
    let name = required_text("name", &new.name, 200)?;
    let description = optional_text("description", new.description, 2000)?;
    check_source(new.kind, new.subject.as_deref(), &new.body)?;
    if new.key.starts_with("system.") && new.kind != Kind::Email {
        return Err(ApiError::invalid(
            "kind",
            "system mail templates are emails",
        ));
    }

    let mut tx = begin(state, ctx).await?;
    let id = core_domain::ids::new_id();
    let inserted = core_db::templates::insert(
        &mut tx,
        id,
        new.kind.as_str(),
        &new.key,
        &locale,
        core_db::templates::Values {
            name: &name,
            subject: new.subject.as_deref(),
            body: &new.body,
            description: description.as_deref(),
        },
        ctx.user_id(),
    )
    .await;
    if let Err(err) = inserted {
        return Err(if err.is_unique_violation(Some("templates_key")) {
            ApiError::conflict(
                "template_key_taken",
                format!("a `{}` template in `{locale}` already exists", new.key),
            )
        } else {
            err.into()
        });
    }
    let t = core_db::templates::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "template.created",
            resource: "template",
            object_id: Some(id),
            before: None,
            after: Some(view_json(&t)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(t)
}

#[derive(Debug, Default)]
pub struct Patch {
    pub name: Option<String>,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub description: Option<Option<String>>,
}

pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    patch: Patch,
    if_match: Option<i64>,
) -> ApiResult<Template> {
    ctx.authorize(Permission::ConfigWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = core_db::templates::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(before.version, if_match)?;
    let kind =
        Kind::parse(&before.kind).ok_or_else(|| ApiError::internal("unknown template kind"))?;
    let name = required_text("name", patch.name.as_deref().unwrap_or(&before.name), 200)?;
    let subject = patch.subject.or(before.subject.clone());
    let body = patch.body.unwrap_or(before.body.clone());
    let description = optional_text(
        "description",
        patch.description.unwrap_or(before.description.clone()),
        2000,
    )?;
    check_source(kind, subject.as_deref(), &body)?;
    if !core_db::templates::update(
        &mut tx,
        id,
        core_db::templates::Values {
            name: &name,
            subject: subject.as_deref(),
            body: &body,
            description: description.as_deref(),
        },
        if_match,
        ctx.user_id(),
    )
    .await?
    {
        return Err(ApiError::version_conflict());
    }
    let after = core_db::templates::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "template.updated",
            resource: "template",
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

pub async fn archive(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::ConfigWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = core_db::templates::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    core_db::templates::archive(&mut tx, id, ctx.user_id()).await?;
    // Rules send by key; refuse to leave one with nothing to send.
    if !core_db::templates::key_exists(&mut tx, &before.key).await? {
        let users: Vec<String> = core_db::rules::list(&mut tx)
            .await?
            .into_iter()
            .filter(|r| sends_template(&r.actions, &before.key))
            .map(|r| r.name)
            .collect();
        if !users.is_empty() {
            return Err(ApiError::conflict(
                "template_in_use",
                format!(
                    "used by these rules: {}; change them first",
                    users.join(", ")
                ),
            ));
        }
    }
    record(
        &mut tx,
        ctx,
        Change {
            topic: "template.archived",
            resource: "template",
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

fn sends_template(actions: &Value, key: &str) -> bool {
    actions.as_array().is_some_and(|list| {
        list.iter().any(|a| {
            a.get("type").and_then(Value::as_str) == Some("send_email")
                && a.get("template").and_then(Value::as_str) == Some(key)
        })
    })
}

// Rendering ------------------------------------------------------------------------

pub(crate) async fn partials(tx: &mut Tx, locale: &str) -> ApiResult<BTreeMap<String, String>> {
    Ok(core_db::templates::partials(tx, locale)
        .await?
        .into_iter()
        .collect())
}

/// Renders `subject`/`body` in the workspace in context, with its partials.
pub(crate) async fn render_in(
    tx: &mut Tx,
    kind: Kind,
    subject: Option<&str>,
    body: &str,
    ctx_json: &Value,
    options: &Options,
) -> ApiResult<Rendered> {
    let partials = partials(tx, &options.locale).await?;
    engine::render(kind, subject, body, partials, ctx_json, options)
        .map_err(|e| ApiError::unprocessable("template_error", e.to_string()))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Preview {
    pub subject: Option<String>,
    /// The rendered body (HTML for email).
    pub body: String,
    pub text: String,
    /// Variables the template uses that this record does not have.
    pub warnings: Vec<String>,
}

/// What to render a preview against: a real record, or sample data
/// (optionally with some values overridden).
#[derive(Debug, Default)]
pub struct PreviewWith {
    /// What the template is about; default `thing`.
    pub entity: Option<String>,
    pub record_id: Option<Uuid>,
    pub overrides: Option<Value>,
}

fn merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                merge(b.entry(k).or_insert(Value::Null), v);
            }
        }
        (b, o) => *b = o,
    }
}

pub async fn preview(
    state: &AppState,
    ctx: &Ctx,
    kind: Kind,
    subject: Option<&str>,
    body: &str,
    with: PreviewWith,
) -> ApiResult<Preview> {
    ctx.authorize(Permission::WorkspaceRead)?;
    let mut tx = begin(state, ctx).await?;
    let (workspace, options) = context::workspace(&mut tx, state).await?;
    let entity = with
        .entity
        .unwrap_or_else(|| super::things::ENTITY.to_owned());
    if !context::is_entity(&entity) {
        return Err(ApiError::invalid(
            "entity",
            format!("there are no `{entity}` records"),
        ));
    }
    let object = match with.record_id {
        Some(id) => {
            ctx.authorize(context::read_permission(&entity))?;
            context::load_object(&mut tx, state, &entity, id)
                .await?
                .ok_or(ApiError::NotFound)?
        }
        None => context::sample(&mut tx, state, &entity).await?,
    };
    let topic = format!("{entity}.created");
    let event =
        json!({ "id": core_domain::ids::new_id(), "topic": topic, "at": chrono::Utc::now() });
    let mut ctx_json = context::build(
        workspace,
        &entity,
        Some(object.clone()),
        Some(object),
        Some(event),
    );
    if let Some(over) = with.overrides {
        merge(&mut ctx_json, over);
    }
    let rendered = render_in(&mut tx, kind, subject, body, &ctx_json, &options).await?;
    tx.commit().await?;
    Ok(Preview {
        warnings: engine::missing_variables(kind, subject, body, &ctx_json)
            .into_iter()
            .filter(|v| !v.starts_with("before."))
            .collect(),
        subject: rendered.subject,
        body: rendered.body,
        text: rendered.text,
    })
}

pub async fn preview_saved(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    with: PreviewWith,
) -> ApiResult<Preview> {
    let t = get(state, ctx, id).await?;
    let kind = Kind::parse(&t.kind).ok_or_else(|| ApiError::internal("unknown template kind"))?;
    preview(state, ctx, kind, t.subject.as_deref(), &t.body, with).await
}

pub async fn variables(state: &AppState, ctx: &Ctx, entity: &str) -> ApiResult<Vec<Variable>> {
    ctx.authorize(Permission::WorkspaceRead)?;
    let mut tx = begin(state, ctx).await?;
    let vars = context::variables(&mut tx, entity).await?;
    tx.commit().await?;
    Ok(vars)
}
