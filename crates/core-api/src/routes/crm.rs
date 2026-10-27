//! The CRM: talent (with members), organisations and contacts; search,
//! tags, duplicate checks, timelines, bulk actions, saved views, schemas
//! and spreadsheet imports.

use axum::Json;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use core_domain::TalentKind;
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{IdempotencyHeader, IfMatchHeader, PageQuery, WorkspaceHeader, with_etag};
use crate::AppState;
use crate::error::{ApiResult, ErrorBody};
use crate::extract::{IfMatch, WsCtx};
use crate::services::contacts::{
    self, ContactDuplicateQuery, ContactPage, ContactPatch, ContactView, NewContact,
};
use crate::services::crm::{Duplicate, ListQuery};
use crate::services::imports::{
    self, DryRunResult, ImportPlan, ImportPreview, ImportView, NewImport,
};
use crate::services::organisations::{
    self, NewOrganisation, OrganisationDuplicateQuery, OrganisationPage, OrganisationPatch,
    OrganisationView,
};
use crate::services::records::{self, BulkRequest, BulkResult};
use crate::services::search::{self as search_svc, SearchHit, TagCount, TimelinePage};
use crate::services::talent::{
    self, NewTalent, TalentDuplicateQuery, TalentMemberInput, TalentMemberView, TalentPage,
    TalentPatch, TalentStatus, TalentView,
};
use crate::services::views::{self, EntitySchema, SavedView, SavedViewInput};

pub fn router() -> OpenApiRouter<AppState> {
    let imports = OpenApiRouter::new()
        .routes(routes!(list_imports, upload_import))
        .layer(DefaultBodyLimit::max(imports::MAX_CSV_BYTES + 64 * 1024));
    OpenApiRouter::new()
        .routes(routes!(list_talent, create_talent))
        .routes(routes!(get_talent, update_talent, delete_talent))
        .routes(routes!(talent_duplicates))
        .routes(routes!(talent_bulk))
        .routes(routes!(talent_timeline))
        .routes(routes!(list_talent_members, add_talent_member))
        .routes(routes!(update_talent_member, remove_talent_member))
        .routes(routes!(list_organisations, create_organisation))
        .routes(routes!(
            get_organisation,
            update_organisation,
            delete_organisation
        ))
        .routes(routes!(organisation_duplicates))
        .routes(routes!(organisation_bulk))
        .routes(routes!(organisation_timeline))
        .routes(routes!(list_contacts, create_contact))
        .routes(routes!(get_contact, update_contact, delete_contact))
        .routes(routes!(contact_duplicates))
        .routes(routes!(contact_bulk))
        .routes(routes!(contact_timeline))
        .routes(routes!(search))
        .routes(routes!(tags))
        .routes(routes!(schema))
        .routes(routes!(list_views, create_view))
        .routes(routes!(update_view, delete_view))
        .merge(imports)
        .routes(routes!(get_import))
        .routes(routes!(dry_run_import))
        .routes(routes!(run_import))
}

// Talent -------------------------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
pub struct TalentFilter {
    pub kind: Option<TalentKind>,
    pub status: Option<TalentStatus>,
}

/// Talent, searched, filtered, sorted and paged.
#[utoipa::path(get, path = "/talent", tag = "talent", params(WorkspaceHeader, ListQuery, TalentFilter),
    responses((status = 200, body = TalentPage), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_talent(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<ListQuery>,
    Query(f): Query<TalentFilter>,
) -> ApiResult<Json<TalentPage>> {
    Ok(Json(
        talent::list(&state, &ctx, &q, f.kind, f.status).await?,
    ))
}

/// Add talent: a person, an act, a space or equipment.
#[utoipa::path(post, path = "/talent", tag = "talent", params(WorkspaceHeader, IdempotencyHeader),
    request_body = NewTalent,
    responses((status = 201, body = TalentView), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_talent(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<NewTalent>,
) -> ApiResult<(StatusCode, Json<TalentView>)> {
    Ok((
        StatusCode::CREATED,
        Json(talent::create(&state, &ctx, b).await?),
    ))
}

#[utoipa::path(get, path = "/talent/{id}", tag = "talent", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = TalentView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_talent(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let v = talent::get(&state, &ctx, id).await?;
    Ok(with_etag(v.version, v))
}

#[utoipa::path(patch, path = "/talent/{id}", tag = "talent",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = TalentPatch,
    responses((status = 200, body = TalentView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody), (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_talent(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<TalentPatch>,
) -> ApiResult<Response> {
    let v = talent::update(&state, &ctx, id, b, if_match).await?;
    Ok(with_etag(v.version, v))
}

/// Delete talent. Bookings and invoices that name it keep working.
#[utoipa::path(delete, path = "/talent/{id}", tag = "talent", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn delete_talent(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    talent::delete(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Existing talent that may be the one being entered (similar names).
#[utoipa::path(post, path = "/talent/duplicates", tag = "talent", params(WorkspaceHeader),
    request_body = TalentDuplicateQuery,
    responses((status = 200, body = Vec<Duplicate>), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn talent_duplicates(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<TalentDuplicateQuery>,
) -> ApiResult<Json<Vec<Duplicate>>> {
    Ok(Json(talent::duplicates(&state, &ctx, &b).await?))
}

/// Delete, tag or untag up to 500 talent records at once.
#[utoipa::path(post, path = "/talent/bulk", tag = "talent", params(WorkspaceHeader, IdempotencyHeader),
    request_body = BulkRequest,
    responses((status = 200, body = BulkResult), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn talent_bulk(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<BulkRequest>,
) -> ApiResult<Json<BulkResult>> {
    Ok(Json(records::bulk(&state, &ctx, talent::ENTITY, b).await?))
}

/// What happened to this talent and its members, newest first.
#[utoipa::path(get, path = "/talent/{id}/timeline", tag = "talent", params(WorkspaceHeader, PageQuery, ("id" = Uuid, Path)),
    responses((status = 200, body = TimelinePage), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn talent_timeline(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<TimelinePage>> {
    Ok(Json(
        search_svc::timeline(&state, &ctx, talent::ENTITY, id, q.before, q.limit()).await?,
    ))
}

/// The people in an act or crew, in order.
#[utoipa::path(get, path = "/talent/{id}/members", tag = "talent", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = Vec<TalentMemberView>), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_talent_members(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<TalentMemberView>>> {
    Ok(Json(talent::members(&state, &ctx, id).await?))
}

#[utoipa::path(post, path = "/talent/{id}/members", tag = "talent", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    request_body = TalentMemberInput,
    responses((status = 201, body = TalentMemberView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn add_talent_member(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<TalentMemberInput>,
) -> ApiResult<(StatusCode, Json<TalentMemberView>)> {
    Ok((
        StatusCode::CREATED,
        Json(talent::add_member(&state, &ctx, id, b).await?),
    ))
}

/// Replace a member's details.
#[utoipa::path(put, path = "/talent/{id}/members/{member_id}", tag = "talent",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path), ("member_id" = Uuid, Path)),
    request_body = TalentMemberInput,
    responses((status = 200, body = TalentMemberView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody), (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_talent_member(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path((id, member_id)): Path<(Uuid, Uuid)>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<TalentMemberInput>,
) -> ApiResult<Response> {
    let v = talent::update_member(&state, &ctx, id, member_id, b, if_match).await?;
    Ok(with_etag(v.version, v))
}

#[utoipa::path(delete, path = "/talent/{id}/members/{member_id}", tag = "talent",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path), ("member_id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn remove_talent_member(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path((id, member_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    talent::remove_member(&state, &ctx, id, member_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// Organisations ------------------------------------------------------------------------------

/// Organisations, searched, filtered, sorted and paged.
#[utoipa::path(get, path = "/organisations", tag = "organisations", params(WorkspaceHeader, ListQuery),
    responses((status = 200, body = OrganisationPage), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_organisations(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<OrganisationPage>> {
    Ok(Json(organisations::list(&state, &ctx, &q).await?))
}

#[utoipa::path(post, path = "/organisations", tag = "organisations", params(WorkspaceHeader, IdempotencyHeader),
    request_body = NewOrganisation,
    responses((status = 201, body = OrganisationView), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_organisation(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<NewOrganisation>,
) -> ApiResult<(StatusCode, Json<OrganisationView>)> {
    Ok((
        StatusCode::CREATED,
        Json(organisations::create(&state, &ctx, b).await?),
    ))
}

#[utoipa::path(get, path = "/organisations/{id}", tag = "organisations", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = OrganisationView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_organisation(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let v = organisations::get(&state, &ctx, id).await?;
    Ok(with_etag(v.version, v))
}

#[utoipa::path(patch, path = "/organisations/{id}", tag = "organisations",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = OrganisationPatch,
    responses((status = 200, body = OrganisationView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody), (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_organisation(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<OrganisationPatch>,
) -> ApiResult<Response> {
    let v = organisations::update(&state, &ctx, id, b, if_match).await?;
    Ok(with_etag(v.version, v))
}

/// Delete an organisation. Its contacts stay, still linked to it.
#[utoipa::path(delete, path = "/organisations/{id}", tag = "organisations", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn delete_organisation(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    organisations::delete(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Existing organisations that may be the one being entered (similar
/// names, the same tax id).
#[utoipa::path(post, path = "/organisations/duplicates", tag = "organisations", params(WorkspaceHeader),
    request_body = OrganisationDuplicateQuery,
    responses((status = 200, body = Vec<Duplicate>), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn organisation_duplicates(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<OrganisationDuplicateQuery>,
) -> ApiResult<Json<Vec<Duplicate>>> {
    Ok(Json(organisations::duplicates(&state, &ctx, &b).await?))
}

/// Delete, tag or untag up to 500 organisations at once.
#[utoipa::path(post, path = "/organisations/bulk", tag = "organisations", params(WorkspaceHeader, IdempotencyHeader),
    request_body = BulkRequest,
    responses((status = 200, body = BulkResult), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn organisation_bulk(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<BulkRequest>,
) -> ApiResult<Json<BulkResult>> {
    Ok(Json(
        records::bulk(&state, &ctx, organisations::ENTITY, b).await?,
    ))
}

/// What happened to this organisation and its contacts, and the messages
/// sent to them, newest first.
#[utoipa::path(get, path = "/organisations/{id}/timeline", tag = "organisations", params(WorkspaceHeader, PageQuery, ("id" = Uuid, Path)),
    responses((status = 200, body = TimelinePage), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn organisation_timeline(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<TimelinePage>> {
    Ok(Json(
        search_svc::timeline(&state, &ctx, organisations::ENTITY, id, q.before, q.limit()).await?,
    ))
}

// Contacts -----------------------------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
pub struct ContactFilter {
    /// Only the contacts at this organisation.
    pub organisation_id: Option<Uuid>,
}

/// Contacts, searched, filtered, sorted and paged.
#[utoipa::path(get, path = "/contacts", tag = "contacts", params(WorkspaceHeader, ListQuery, ContactFilter),
    responses((status = 200, body = ContactPage), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_contacts(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<ListQuery>,
    Query(f): Query<ContactFilter>,
) -> ApiResult<Json<ContactPage>> {
    Ok(Json(
        contacts::list(&state, &ctx, &q, f.organisation_id).await?,
    ))
}

#[utoipa::path(post, path = "/contacts", tag = "contacts", params(WorkspaceHeader, IdempotencyHeader),
    request_body = NewContact,
    responses((status = 201, body = ContactView), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_contact(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<NewContact>,
) -> ApiResult<(StatusCode, Json<ContactView>)> {
    Ok((
        StatusCode::CREATED,
        Json(contacts::create(&state, &ctx, b).await?),
    ))
}

#[utoipa::path(get, path = "/contacts/{id}", tag = "contacts", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = ContactView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_contact(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let v = contacts::get(&state, &ctx, id).await?;
    Ok(with_etag(v.version, v))
}

#[utoipa::path(patch, path = "/contacts/{id}", tag = "contacts",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = ContactPatch,
    responses((status = 200, body = ContactView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody), (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_contact(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<ContactPatch>,
) -> ApiResult<Response> {
    let v = contacts::update(&state, &ctx, id, b, if_match).await?;
    Ok(with_etag(v.version, v))
}

#[utoipa::path(delete, path = "/contacts/{id}", tag = "contacts", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn delete_contact(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    contacts::delete(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Existing contacts that may be the one being entered: the same email or
/// phone number, or a similar name.
#[utoipa::path(post, path = "/contacts/duplicates", tag = "contacts", params(WorkspaceHeader),
    request_body = ContactDuplicateQuery,
    responses((status = 200, body = Vec<Duplicate>), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn contact_duplicates(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<ContactDuplicateQuery>,
) -> ApiResult<Json<Vec<Duplicate>>> {
    Ok(Json(contacts::duplicates(&state, &ctx, &b).await?))
}

/// Delete, tag or untag up to 500 contacts at once.
#[utoipa::path(post, path = "/contacts/bulk", tag = "contacts", params(WorkspaceHeader, IdempotencyHeader),
    request_body = BulkRequest,
    responses((status = 200, body = BulkResult), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn contact_bulk(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<BulkRequest>,
) -> ApiResult<Json<BulkResult>> {
    Ok(Json(
        records::bulk(&state, &ctx, contacts::ENTITY, b).await?,
    ))
}

/// What happened to this contact, and the messages sent to them, newest first.
#[utoipa::path(get, path = "/contacts/{id}/timeline", tag = "contacts", params(WorkspaceHeader, PageQuery, ("id" = Uuid, Path)),
    responses((status = 200, body = TimelinePage), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn contact_timeline(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<TimelinePage>> {
    Ok(Json(
        search_svc::timeline(&state, &ctx, contacts::ENTITY, id, q.before, q.limit()).await?,
    ))
}

// Search, tags, schemas and views ---------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
pub struct SearchQuery {
    pub q: String,
    /// Comma-separated entities to search; default all the caller may read.
    pub entities: Option<String>,
    /// 1 to 50, default 20.
    pub limit: Option<i64>,
}

/// Search talent, organisations and contacts at once, best matches first.
#[utoipa::path(get, path = "/search", tag = "search", params(WorkspaceHeader, SearchQuery),
    responses((status = 200, body = Vec<SearchHit>), (status = 400, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn search(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<SearchQuery>,
) -> ApiResult<Json<Vec<SearchHit>>> {
    let entities: Option<Vec<String>> = q.entities.as_deref().map(|e| {
        e.split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect()
    });
    Ok(Json(
        search_svc::search(
            &state,
            &ctx,
            &q.q,
            entities.as_deref(),
            q.limit.unwrap_or(20),
        )
        .await?,
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct EntityParam {
    /// `talent`, `organisation` or `contact`.
    pub entity: String,
}

/// The tags in use on an entity's records, most used first.
#[utoipa::path(get, path = "/tags", tag = "search", params(WorkspaceHeader, EntityParam),
    responses((status = 200, body = Vec<TagCount>), (status = 400, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn tags(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<EntityParam>,
) -> ApiResult<Json<Vec<TagCount>>> {
    Ok(Json(search_svc::tags(&state, &ctx, &q.entity).await?))
}

/// An entity's fields, core and custom, as forms and lists draw them.
#[utoipa::path(get, path = "/schema/{entity}", tag = "views", params(WorkspaceHeader, ("entity" = String, Path)),
    responses((status = 200, body = EntitySchema), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn schema(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(entity): Path<String>,
) -> ApiResult<Json<EntitySchema>> {
    Ok(Json(views::schema(&state, &ctx, &entity).await?))
}

/// Saved views of an entity's list: shared ones and the caller's own.
#[utoipa::path(get, path = "/views", tag = "views", params(WorkspaceHeader, EntityParam),
    responses((status = 200, body = Vec<SavedView>), (status = 400, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_views(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<EntityParam>,
) -> ApiResult<Json<Vec<SavedView>>> {
    Ok(Json(views::list(&state, &ctx, &q.entity).await?))
}

#[utoipa::path(post, path = "/views", tag = "views", params(WorkspaceHeader, IdempotencyHeader),
    request_body = SavedViewInput,
    responses((status = 201, body = SavedView), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_view(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<SavedViewInput>,
) -> ApiResult<(StatusCode, Json<SavedView>)> {
    Ok((
        StatusCode::CREATED,
        Json(views::create(&state, &ctx, b).await?),
    ))
}

/// Replace a saved view.
#[utoipa::path(put, path = "/views/{id}", tag = "views",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = SavedViewInput,
    responses((status = 200, body = SavedView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody), (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_view(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<SavedViewInput>,
) -> ApiResult<Response> {
    let v = views::update(&state, &ctx, id, b, if_match).await?;
    Ok(with_etag(v.version, v))
}

#[utoipa::path(delete, path = "/views/{id}", tag = "views", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn delete_view(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    views::delete(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// Imports --------------------------------------------------------------------------------------

/// Recent imports the caller may run, newest first.
#[utoipa::path(get, path = "/imports", tag = "imports", params(WorkspaceHeader, PageQuery),
    responses((status = 200, body = Vec<ImportView>)), security(("session" = []), ("api_key" = [])))]
async fn list_imports(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<Vec<ImportView>>> {
    Ok(Json(
        imports::list(&state, &ctx, q.before, q.limit()).await?,
    ))
}

/// Upload a spreadsheet (as CSV text, up to 10 MB). Answers with its
/// columns, the first rows and a suggested mapping of columns to fields.
#[utoipa::path(post, path = "/imports", tag = "imports", params(WorkspaceHeader, IdempotencyHeader),
    request_body = NewImport,
    responses((status = 201, body = ImportPreview), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn upload_import(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<NewImport>,
) -> ApiResult<(StatusCode, Json<ImportPreview>)> {
    Ok((
        StatusCode::CREATED,
        Json(imports::upload(&state, &ctx, b).await?),
    ))
}

/// An import and, once run, its progress and results.
#[utoipa::path(get, path = "/imports/{id}", tag = "imports", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = ImportView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_import(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ImportView>> {
    Ok(Json(imports::get(&state, &ctx, id).await?))
}

/// Check every row against a mapping without importing anything.
#[utoipa::path(post, path = "/imports/{id}/dry-run", tag = "imports", params(WorkspaceHeader, ("id" = Uuid, Path)),
    request_body = ImportPlan,
    responses((status = 200, body = DryRunResult), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn dry_run_import(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<ImportPlan>,
) -> ApiResult<Json<DryRunResult>> {
    Ok(Json(imports::dry_run(&state, &ctx, id, b).await?))
}

/// Import the rows with a mapping. Runs in the background: follow it at
/// `GET /imports/{id}`.
#[utoipa::path(post, path = "/imports/{id}/run", tag = "imports", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    request_body = ImportPlan,
    responses((status = 202, body = ImportView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn run_import(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<ImportPlan>,
) -> ApiResult<(StatusCode, Json<ImportView>)> {
    Ok((
        StatusCode::ACCEPTED,
        Json(imports::run(&state, &ctx, id, b).await?),
    ))
}
