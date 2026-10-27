//! Workspace configuration: custom fields, pipelines, message templates,
//! automation rules and the automations kill switch.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use chrono::{DateTime, Utc};
use core_engines::fields::{FieldType, Validation};
use core_engines::pipeline::Outcome;
use core_engines::templates::Kind;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{
    IdempotencyHeader, IfMatchHeader, PageQuery, WorkspaceHeader, next_before, nullable, with_etag,
};
use crate::AppState;
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::extract::{IfMatch, WsCtx};
use crate::services::automation::{ActionResult, DryRun};
use crate::services::context::Variable;
use crate::services::templates::{Preview, PreviewWith};
use crate::services::{automation, fields, pipelines, rules, templates};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_fields, create_field))
        .routes(routes!(get_field, update_field, archive_field))
        .routes(routes!(list_pipelines, create_pipeline))
        .routes(routes!(get_pipeline, update_pipeline, delete_pipeline))
        .routes(routes!(list_templates, create_template))
        .routes(routes!(template_variables))
        .routes(routes!(preview_draft))
        .routes(routes!(get_template, update_template, archive_template))
        .routes(routes!(preview_template))
        .routes(routes!(list_rules, create_rule))
        .routes(routes!(get_rule, update_rule, delete_rule))
        .routes(routes!(dry_run_rule))
        .routes(routes!(rule_log))
        .routes(routes!(get_automations, set_automations))
}

// Custom fields --------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct FieldView {
    pub id: Uuid,
    /// What the field belongs to, e.g. `thing`.
    pub entity: String,
    /// Where the value lives: `custom.<key>` on the record. Permanent.
    pub key: String,
    pub label: String,
    pub field_type: FieldType,
    pub required: bool,
    #[schema(value_type = Option<Object>)]
    pub default_value: Option<Value>,
    pub validation: Validation,
    /// `staff`, `portal` or `both`.
    pub visibility: String,
    pub searchable: bool,
    pub group: Option<String>,
    pub position: i32,
    pub help_text: Option<String>,
    pub archived: bool,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn field_view(r: core_db::fields::FieldDef) -> ApiResult<FieldView> {
    Ok(FieldView {
        field_type: FieldType::parse(&r.field_type)
            .ok_or_else(|| ApiError::internal("stored field has an unknown type"))?,
        validation: serde_json::from_value(r.validation).unwrap_or_default(),
        id: r.id,
        entity: r.entity,
        key: r.key,
        label: r.label,
        required: r.required,
        default_value: r.default_value,
        visibility: r.visibility,
        searchable: r.searchable,
        group: r.field_group,
        position: r.position,
        help_text: r.help_text,
        archived: r.archived_at.is_some(),
        version: r.version,
        created_at: r.created_at,
        updated_at: r.updated_at,
    })
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct FieldListQuery {
    /// Only this entity's fields, e.g. `thing`.
    pub entity: Option<String>,
    /// Include archived fields (default false).
    #[serde(default)]
    pub include_archived: bool,
}

/// Custom field definitions, in display order.
#[utoipa::path(get, path = "/custom-fields", tag = "custom-fields", params(WorkspaceHeader, FieldListQuery),
    responses((status = 200, body = Vec<FieldView>), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_fields(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<FieldListQuery>,
) -> ApiResult<Json<Vec<FieldView>>> {
    let rows = fields::list(&state, &ctx, q.entity.as_deref(), q.include_archived).await?;
    Ok(Json(
        rows.into_iter().map(field_view).collect::<ApiResult<_>>()?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateFieldRequest {
    pub entity: String,
    /// Lowercase letters, digits and underscores, starting with a letter. Permanent.
    pub key: String,
    pub label: String,
    /// Permanent.
    pub field_type: FieldType,
    #[serde(default)]
    pub required: bool,
    #[schema(value_type = Option<Object>)]
    pub default_value: Option<Value>,
    #[serde(default)]
    pub validation: Validation,
    /// `staff` (default), `portal` or `both`.
    pub visibility: Option<String>,
    #[serde(default)]
    pub searchable: bool,
    pub group: Option<String>,
    /// Defaults to after the last field.
    pub position: Option<i32>,
    pub help_text: Option<String>,
}

/// Add a custom field. Existing records are not changed; a required field
/// is enforced on new records and on edits that touch it.
#[utoipa::path(post, path = "/custom-fields", tag = "custom-fields", params(WorkspaceHeader, IdempotencyHeader),
    request_body = CreateFieldRequest,
    responses((status = 201, body = FieldView), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_field(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<CreateFieldRequest>,
) -> ApiResult<(StatusCode, Json<FieldView>)> {
    let settings = fields::Settings {
        label: b.label,
        required: b.required,
        default_value: b.default_value,
        validation: b.validation,
        visibility: b.visibility.unwrap_or_else(|| "staff".into()),
        searchable: b.searchable,
        group: b.group,
        position: b.position,
        help_text: b.help_text,
    };
    let row = fields::create(&state, &ctx, &b.entity, &b.key, b.field_type, settings).await?;
    Ok((StatusCode::CREATED, Json(field_view(row)?)))
}

#[utoipa::path(get, path = "/custom-fields/{id}", tag = "custom-fields", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = FieldView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_field(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let row = fields::get(&state, &ctx, id).await?;
    Ok(with_etag(row.version, field_view(row)?))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct FieldPatchRequest {
    pub label: Option<String>,
    pub required: Option<bool>,
    /// `null` removes the default.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<Object>)]
    pub default_value: Option<Option<Value>>,
    pub validation: Option<Validation>,
    pub visibility: Option<String>,
    pub searchable: Option<bool>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub group: Option<Option<String>>,
    pub position: Option<i32>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub help_text: Option<Option<String>>,
}

/// Change a field's settings. Its entity, key and type never change.
#[utoipa::path(patch, path = "/custom-fields/{id}", tag = "custom-fields",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = FieldPatchRequest,
    responses((status = 200, body = FieldView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody), (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_field(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<FieldPatchRequest>,
) -> ApiResult<Response> {
    let patch = fields::Patch {
        label: b.label,
        required: b.required,
        default_value: b.default_value,
        validation: b.validation,
        visibility: b.visibility,
        searchable: b.searchable,
        group: b.group,
        position: b.position,
        help_text: b.help_text,
    };
    let row = fields::update(&state, &ctx, id, patch, if_match).await?;
    Ok(with_etag(row.version, field_view(row)?))
}

/// Archive a field. Values already stored are kept but hidden; the key
/// cannot be reused.
#[utoipa::path(delete, path = "/custom-fields/{id}", tag = "custom-fields", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn archive_field(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    fields::archive(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// Pipelines ------------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct StageView {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    pub position: i32,
    pub colour: Option<String>,
    pub sla_days: Option<i32>,
    pub outcome: Outcome,
    /// Custom fields that must have a value before a record enters this stage.
    pub required_fields: Vec<String>,
    /// Stage keys a record may move to from here; absent means any.
    pub allowed_next: Option<Vec<String>>,
    pub archived: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PipelineView {
    pub id: Uuid,
    pub entity: String,
    pub name: String,
    pub is_default: bool,
    /// In order; archived stages last.
    pub stages: Vec<StageView>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn pipeline_view(p: pipelines::WithStages) -> PipelineView {
    let mut stages: Vec<StageView> = p
        .stages
        .into_iter()
        .map(|s| StageView {
            outcome: serde_json::from_value(Value::String(s.outcome)).unwrap_or_default(),
            id: s.id,
            key: s.key,
            name: s.name,
            position: s.position,
            colour: s.colour,
            sla_days: s.sla_days,
            required_fields: s.required_fields,
            allowed_next: s.allowed_next,
            archived: s.archived_at.is_some(),
        })
        .collect();
    stages.sort_by_key(|s| (s.archived, s.position));
    PipelineView {
        id: p.pipeline.id,
        entity: p.pipeline.entity,
        name: p.pipeline.name,
        is_default: p.pipeline.is_default,
        stages,
        version: p.pipeline.version,
        created_at: p.pipeline.created_at,
        updated_at: p.pipeline.updated_at,
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct StageRequest {
    /// Lowercase letters, digits and underscores. Stages are matched by key on update.
    pub key: String,
    pub name: String,
    /// A CSS colour such as `#2f80ed`.
    pub colour: Option<String>,
    pub sla_days: Option<i32>,
    #[serde(default)]
    pub outcome: Outcome,
    #[serde(default)]
    pub required_fields: Vec<String>,
    pub allowed_next: Option<Vec<String>>,
}

impl From<StageRequest> for pipelines::StageInput {
    fn from(s: StageRequest) -> Self {
        Self {
            key: s.key,
            name: s.name,
            colour: s.colour,
            sla_days: s.sla_days,
            outcome: s.outcome,
            required_fields: s.required_fields,
            allowed_next: s.allowed_next,
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct EntityQuery {
    /// e.g. `thing`.
    pub entity: Option<String>,
}

/// Pipelines and their stages.
#[utoipa::path(get, path = "/pipelines", tag = "pipelines", params(WorkspaceHeader, EntityQuery),
    responses((status = 200, body = Vec<PipelineView>), (status = 400, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_pipelines(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<EntityQuery>,
) -> ApiResult<Json<Vec<PipelineView>>> {
    let list = pipelines::list(&state, &ctx, q.entity.as_deref()).await?;
    Ok(Json(list.into_iter().map(pipeline_view).collect()))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreatePipelineRequest {
    pub entity: String,
    pub name: String,
    /// Make this the entity's default. An entity's first pipeline always is.
    #[serde(default)]
    pub is_default: bool,
    /// At least one, in order.
    pub stages: Vec<StageRequest>,
}

#[utoipa::path(post, path = "/pipelines", tag = "pipelines", params(WorkspaceHeader, IdempotencyHeader),
    request_body = CreatePipelineRequest,
    responses((status = 201, body = PipelineView), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_pipeline(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<CreatePipelineRequest>,
) -> ApiResult<(StatusCode, Json<PipelineView>)> {
    let stages = b.stages.into_iter().map(Into::into).collect();
    let p = pipelines::create(&state, &ctx, &b.entity, &b.name, b.is_default, stages).await?;
    Ok((StatusCode::CREATED, Json(pipeline_view(p))))
}

#[utoipa::path(get, path = "/pipelines/{id}", tag = "pipelines", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = PipelineView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_pipeline(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let p = pipelines::get(&state, &ctx, id).await?;
    Ok(with_etag(p.pipeline.version, pipeline_view(p)))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct PipelinePatchRequest {
    pub name: Option<String>,
    /// `true` makes this the entity's default pipeline.
    #[serde(default)]
    pub make_default: bool,
    /// The full list of stages, in order. New keys are added; missing keys
    /// are archived (records in them stay until moved).
    pub stages: Option<Vec<StageRequest>>,
}

#[utoipa::path(patch, path = "/pipelines/{id}", tag = "pipelines",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = PipelinePatchRequest,
    responses((status = 200, body = PipelineView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody), (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_pipeline(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<PipelinePatchRequest>,
) -> ApiResult<Response> {
    let patch = pipelines::Patch {
        name: b.name,
        make_default: b.make_default,
        stages: b.stages.map(|s| s.into_iter().map(Into::into).collect()),
    };
    let p = pipelines::update(&state, &ctx, id, patch, if_match).await?;
    Ok(with_etag(p.pipeline.version, pipeline_view(p)))
}

/// Delete a pipeline. Refused while records use it or while it is the default.
#[utoipa::path(delete, path = "/pipelines/{id}", tag = "pipelines", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn delete_pipeline(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    pipelines::delete(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// Templates ------------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct TemplateView {
    pub id: Uuid,
    pub kind: Kind,
    /// What rules and the platform refer to it by, e.g. `quote.sent`.
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

fn template_view(t: core_db::templates::Template) -> ApiResult<TemplateView> {
    Ok(TemplateView {
        kind: Kind::parse(&t.kind)
            .ok_or_else(|| ApiError::internal("stored template has an unknown kind"))?,
        id: t.id,
        key: t.key,
        locale: t.locale,
        name: t.name,
        subject: t.subject,
        body: t.body,
        description: t.description,
        version: t.version,
        created_at: t.created_at,
        updated_at: t.updated_at,
    })
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct TemplateListQuery {
    pub kind: Option<Kind>,
}

#[utoipa::path(get, path = "/templates", tag = "templates", params(WorkspaceHeader, TemplateListQuery),
    responses((status = 200, body = Vec<TemplateView>)), security(("session" = []), ("api_key" = [])))]
async fn list_templates(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<TemplateListQuery>,
) -> ApiResult<Json<Vec<TemplateView>>> {
    let rows = templates::list(&state, &ctx, q.kind).await?;
    Ok(Json(
        rows.into_iter()
            .map(template_view)
            .collect::<ApiResult<_>>()?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateTemplateRequest {
    pub kind: Kind,
    /// Dot-separated lowercase segments, e.g. `quote.sent`. Permanent.
    pub key: String,
    /// BCP 47, e.g. `en-GB`. Defaults to `en`, which also serves every
    /// English locale that has no template of its own.
    pub locale: Option<String>,
    pub name: String,
    /// Required for email.
    pub subject: Option<String>,
    /// Jinja-style: `{{ thing.name }}`, `{% if %}`, filters `money`, `date`,
    /// `datetime`, `address`, and `{% include "partial.key" %}`.
    pub body: String,
    pub description: Option<String>,
}

/// Add a template. Syntax errors are refused with the line they are on.
#[utoipa::path(post, path = "/templates", tag = "templates", params(WorkspaceHeader, IdempotencyHeader),
    request_body = CreateTemplateRequest,
    responses((status = 201, body = TemplateView), (status = 400, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_template(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<CreateTemplateRequest>,
) -> ApiResult<(StatusCode, Json<TemplateView>)> {
    let t = templates::create(
        &state,
        &ctx,
        templates::New {
            kind: b.kind,
            key: b.key,
            locale: b.locale,
            name: b.name,
            subject: b.subject,
            body: b.body,
            description: b.description,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(template_view(t)?)))
}

#[utoipa::path(get, path = "/templates/{id}", tag = "templates", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = TemplateView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_template(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let t = templates::get(&state, &ctx, id).await?;
    Ok(with_etag(t.version, template_view(t)?))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct TemplatePatchRequest {
    pub name: Option<String>,
    pub subject: Option<String>,
    pub body: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub description: Option<Option<String>>,
}

#[utoipa::path(patch, path = "/templates/{id}", tag = "templates",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = TemplatePatchRequest,
    responses((status = 200, body = TemplateView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody), (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_template(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<TemplatePatchRequest>,
) -> ApiResult<Response> {
    let patch = templates::Patch {
        name: b.name,
        subject: b.subject,
        body: b.body,
        description: b.description,
    };
    let t = templates::update(&state, &ctx, id, patch, if_match).await?;
    Ok(with_etag(t.version, template_view(t)?))
}

/// Archive a template. Refused while a rule sends it.
#[utoipa::path(delete, path = "/templates/{id}", tag = "templates", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn archive_template(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    templates::archive(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct PreviewRequest {
    /// Render against this record; otherwise sample data is used.
    pub thing_id: Option<Uuid>,
    /// Values merged over the context, e.g. `{"thing": {"name": "Test"}}`.
    #[schema(value_type = Option<Object>)]
    pub overrides: Option<Value>,
}

/// Render a saved template.
#[utoipa::path(post, path = "/templates/{id}/preview", tag = "templates", params(WorkspaceHeader, ("id" = Uuid, Path)),
    request_body = PreviewRequest,
    responses((status = 200, body = Preview), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn preview_template(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<PreviewRequest>,
) -> ApiResult<Json<Preview>> {
    let with = PreviewWith {
        thing_id: b.thing_id,
        overrides: b.overrides,
    };
    Ok(Json(
        templates::preview_saved(&state, &ctx, id, with).await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct DraftPreviewRequest {
    pub kind: Kind,
    pub subject: Option<String>,
    pub body: String,
    pub thing_id: Option<Uuid>,
    #[schema(value_type = Option<Object>)]
    pub overrides: Option<Value>,
}

/// Render an unsaved template, for live preview while editing.
#[utoipa::path(post, path = "/templates/preview", tag = "templates", params(WorkspaceHeader),
    request_body = DraftPreviewRequest,
    responses((status = 200, body = Preview), (status = 400, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn preview_draft(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<DraftPreviewRequest>,
) -> ApiResult<Json<Preview>> {
    let with = PreviewWith {
        thing_id: b.thing_id,
        overrides: b.overrides,
    };
    Ok(Json(
        templates::preview(&state, &ctx, b.kind, b.subject.as_deref(), &b.body, with).await?,
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct VariablesQuery {
    /// Defaults to `thing`.
    pub entity: Option<String>,
}

/// Every variable a template can use, with a description of each.
#[utoipa::path(get, path = "/templates/variables", tag = "templates", params(WorkspaceHeader, VariablesQuery),
    responses((status = 200, body = Vec<Variable>), (status = 400, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn template_variables(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<VariablesQuery>,
) -> ApiResult<Json<Vec<Variable>>> {
    let entity = q.entity.as_deref().unwrap_or("thing");
    Ok(Json(templates::variables(&state, &ctx, entity).await?))
}

// Rules ----------------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct RuleView {
    pub id: Uuid,
    pub name: String,
    pub enabled: bool,
    /// Log what would happen without doing it.
    pub dry_run: bool,
    /// Fire at most once per record.
    pub once_per_object: bool,
    /// `{"kind": "event", "topic": "thing.created"}`,
    /// `{"kind": "date_offset", "entity": "thing", "field": "due_at", "offset_minutes": -1440}` or
    /// `{"kind": "schedule", "cron": "0 9 * * 1"}`.
    #[schema(value_type = Object)]
    pub trigger: Value,
    /// `{"all": [...]}`, `{"any": [...]}`, `{"not": {...}}` or
    /// `{"field": "thing.custom.budget", "op": "gt", "value": 1000}`.
    #[schema(value_type = Object)]
    pub conditions: Value,
    /// `[{"type": "send_email", "template": "quote.sent", "to": {"field": "thing.custom.email"}}, ...]`
    #[schema(value_type = Vec<Object>)]
    pub actions: Value,
    pub next_run_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<core_db::rules::Rule> for RuleView {
    fn from(r: core_db::rules::Rule) -> Self {
        Self {
            id: r.id,
            name: r.name,
            enabled: r.enabled,
            dry_run: r.dry_run,
            once_per_object: r.once_per_object,
            trigger: r.trigger,
            conditions: r.conditions,
            actions: r.actions,
            next_run_at: r.next_run_at,
            version: r.version,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[utoipa::path(get, path = "/rules", tag = "rules", params(WorkspaceHeader),
    responses((status = 200, body = Vec<RuleView>)), security(("session" = []), ("api_key" = [])))]
async fn list_rules(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<Vec<RuleView>>> {
    Ok(Json(
        rules::list(&state, &ctx)
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateRuleRequest {
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub dry_run: bool,
    /// Fire at most once per record (default true). Turn off for rules that
    /// should act on every update.
    #[serde(default = "yes")]
    pub once_per_object: bool,
    #[schema(value_type = Object)]
    pub trigger: Value,
    /// Omit to always match.
    #[serde(default = "always")]
    #[schema(value_type = Object)]
    pub conditions: Value,
    #[schema(value_type = Vec<Object>)]
    pub actions: Value,
}

fn yes() -> bool {
    true
}

fn always() -> Value {
    serde_json::json!({ "all": [] })
}

/// Add a rule. Every template, field, stage and role it names is checked.
#[utoipa::path(post, path = "/rules", tag = "rules", params(WorkspaceHeader, IdempotencyHeader),
    request_body = CreateRuleRequest,
    responses((status = 201, body = RuleView), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_rule(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<CreateRuleRequest>,
) -> ApiResult<(StatusCode, Json<RuleView>)> {
    let input = rules::Input {
        name: b.name,
        enabled: b.enabled,
        dry_run: b.dry_run,
        once_per_object: b.once_per_object,
        trigger: b.trigger,
        conditions: b.conditions,
        actions: b.actions,
    };
    let r = rules::create(&state, &ctx, input).await?;
    Ok((StatusCode::CREATED, Json(r.into())))
}

#[utoipa::path(get, path = "/rules/{id}", tag = "rules", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = RuleView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_rule(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let r = rules::get(&state, &ctx, id).await?;
    Ok(with_etag(r.version, RuleView::from(r)))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct RulePatchRequest {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub dry_run: Option<bool>,
    pub once_per_object: Option<bool>,
    #[schema(value_type = Option<Object>)]
    pub trigger: Option<Value>,
    #[schema(value_type = Option<Object>)]
    pub conditions: Option<Value>,
    #[schema(value_type = Option<Vec<Object>>)]
    pub actions: Option<Value>,
}

#[utoipa::path(patch, path = "/rules/{id}", tag = "rules",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = RulePatchRequest,
    responses((status = 200, body = RuleView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody), (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_rule(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<RulePatchRequest>,
) -> ApiResult<Response> {
    let patch = rules::Patch {
        name: b.name,
        enabled: b.enabled,
        dry_run: b.dry_run,
        once_per_object: b.once_per_object,
        trigger: b.trigger,
        conditions: b.conditions,
        actions: b.actions,
    };
    let r = rules::update(&state, &ctx, id, patch, if_match).await?;
    Ok(with_etag(r.version, RuleView::from(r)))
}

#[utoipa::path(delete, path = "/rules/{id}", tag = "rules", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn delete_rule(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    rules::delete(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct DryRunRequest {
    /// Test against this record; otherwise against sample data.
    pub thing_id: Option<Uuid>,
}

/// Test a rule: whether it would match, why, and what it would do. Nothing is changed or sent.
#[utoipa::path(post, path = "/rules/{id}/dry-run", tag = "rules", params(WorkspaceHeader, ("id" = Uuid, Path)),
    request_body = DryRunRequest,
    responses((status = 200, body = DryRun), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn dry_run_rule(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<DryRunRequest>,
) -> ApiResult<Json<DryRun>> {
    Ok(Json(
        automation::dry_run(&state, &ctx, id, b.thing_id).await?,
    ))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RuleRunView {
    pub id: Uuid,
    pub event_id: Option<Uuid>,
    pub object_id: Option<Uuid>,
    /// `acted`, `dry_run`, `skipped` or `failed`.
    pub outcome: String,
    pub reason: Option<String>,
    pub actions: Vec<ActionResult>,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RuleLogPage {
    pub runs: Vec<RuleRunView>,
    pub next_before: Option<Uuid>,
}

/// What a rule did and why, newest first.
#[utoipa::path(get, path = "/rules/{id}/log", tag = "rules", params(WorkspaceHeader, PageQuery, ("id" = Uuid, Path)),
    responses((status = 200, body = RuleLogPage), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn rule_log(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<RuleLogPage>> {
    let limit = q.limit();
    let rows = rules::log(&state, &ctx, id, q.before, limit).await?;
    let next_before = next_before(&rows, limit, |r| r.id);
    Ok(Json(RuleLogPage {
        runs: rows
            .into_iter()
            .map(|r| RuleRunView {
                id: r.id,
                event_id: r.event_id,
                object_id: r.object_id,
                outcome: r.outcome,
                reason: r.reason,
                actions: serde_json::from_value(r.actions).unwrap_or_default(),
                at: r.created_at,
            })
            .collect(),
        next_before,
    }))
}

// Kill switch ----------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AutomationsState {
    /// While paused no rule runs; events are still recorded and webhooks still sent.
    pub paused: bool,
}

#[utoipa::path(get, path = "/automations", tag = "rules", params(WorkspaceHeader),
    responses((status = 200, body = AutomationsState)), security(("session" = []), ("api_key" = [])))]
async fn get_automations(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
) -> ApiResult<Json<AutomationsState>> {
    Ok(Json(AutomationsState {
        paused: rules::automations_paused(&state, &ctx).await?,
    }))
}

/// Pause or resume every rule in the workspace.
#[utoipa::path(put, path = "/automations", tag = "rules", params(WorkspaceHeader, IdempotencyHeader),
    request_body = AutomationsState,
    responses((status = 200, body = AutomationsState), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn set_automations(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<AutomationsState>,
) -> ApiResult<Json<AutomationsState>> {
    Ok(Json(AutomationsState {
        paused: rules::set_automations_paused(&state, &ctx, b.paused).await?,
    }))
}
