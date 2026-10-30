//! Projects (tours, seasons, circuits, campaigns): their dates, crew, shared
//! travel and costs, whether the whole run pays, and the day-by-day
//! itinerary.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use super::{IdempotencyHeader, IfMatchHeader, WorkspaceHeader, with_etag};
use crate::AppState;
use crate::error::{ApiResult, ErrorBody};
use crate::extract::{IfMatch, WsCtx};
use crate::services::projects::{
    self, CrewInput, CrewView, DateLink, Itinerary, NewProject, ProjectFilter, ProjectOverview,
    ProjectPage, ProjectPatch, ProjectView,
};
use crate::services::travel::{
    self, MoneyLineInput, MoneyLineView, Owner, TravelInput, TravelView,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_projects, create_project))
        .routes(routes!(get_project, update_project, delete_project))
        .routes(routes!(project_overview))
        .routes(routes!(project_itinerary))
        .routes(routes!(add_date))
        .routes(routes!(remove_date))
        .routes(routes!(add_crew))
        .routes(routes!(update_crew, remove_crew))
        .routes(routes!(add_project_travel))
        .routes(routes!(add_project_line))
}

/// Projects with dates still to come first, soonest first.
#[utoipa::path(get, path = "/projects", tag = "projects", params(WorkspaceHeader, ProjectFilter),
    responses((status = 200, body = ProjectPage), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_projects(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(f): Query<ProjectFilter>,
) -> ApiResult<Json<ProjectPage>> {
    Ok(Json(projects::list(&state, &ctx, &f).await?))
}

/// Start a project (a tour by default), optionally with its first dates.
#[utoipa::path(post, path = "/projects", tag = "projects", params(WorkspaceHeader, IdempotencyHeader),
    request_body = NewProject,
    responses((status = 201, body = ProjectView), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn create_project(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Json(b): Json<NewProject>,
) -> ApiResult<(StatusCode, Json<ProjectView>)> {
    Ok((
        StatusCode::CREATED,
        Json(projects::create(&state, &ctx, b).await?),
    ))
}

#[utoipa::path(get, path = "/projects/{id}", tag = "projects", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = ProjectView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn get_project(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let v = projects::get(&state, &ctx, id).await?;
    Ok(with_etag(v.version, v))
}

#[utoipa::path(patch, path = "/projects/{id}", tag = "projects",
    params(WorkspaceHeader, IfMatchHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = ProjectPatch,
    responses((status = 200, body = ProjectView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 412, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_project(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    IfMatch(if_match): IfMatch,
    Json(b): Json<ProjectPatch>,
) -> ApiResult<Response> {
    let v = projects::update(&state, &ctx, id, b, if_match).await?;
    Ok(with_etag(v.version, v))
}

/// Delete a project. Its dates stay, as bookings of their own.
#[utoipa::path(delete, path = "/projects/{id}", tag = "projects", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn delete_project(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    projects::delete(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Every date with whether it pays, the totals across the project in its
/// currency, the budget against what is booked or spent, the crew, and the
/// project's own travel and costs.
#[utoipa::path(get, path = "/projects/{id}/overview", tag = "projects", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = ProjectOverview), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn project_overview(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ProjectOverview>> {
    Ok(Json(projects::overview(&state, &ctx, id).await?))
}

/// Day by day from the first journey to the last: dates with their running
/// order, journeys, where people sleep, and days off. Day sheets read it.
#[utoipa::path(get, path = "/projects/{id}/itinerary", tag = "projects", params(WorkspaceHeader, ("id" = Uuid, Path)),
    responses((status = 200, body = Itinerary), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn project_itinerary(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Itinerary>> {
    Ok(Json(projects::itinerary(&state, &ctx, id).await?))
}

/// Make a booking one of the project's dates (moving it out of any other project).
#[utoipa::path(post, path = "/projects/{id}/dates", tag = "projects",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = DateLink,
    responses((status = 200, body = ProjectView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn add_date(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<DateLink>,
) -> ApiResult<Json<ProjectView>> {
    Ok(Json(projects::add_date(&state, &ctx, id, b).await?))
}

/// Take a date out of the project; the booking itself stays.
#[utoipa::path(delete, path = "/projects/{id}/dates/{booking_id}", tag = "projects",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path), ("booking_id" = Uuid, Path)),
    responses((status = 200, body = ProjectView), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn remove_date(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path((id, booking_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<ProjectView>> {
    Ok(Json(
        projects::remove_date(&state, &ctx, id, booking_id).await?,
    ))
}

#[utoipa::path(post, path = "/projects/{id}/crew", tag = "projects",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = CrewInput,
    responses((status = 201, body = CrewView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn add_crew(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<CrewInput>,
) -> ApiResult<(StatusCode, Json<CrewView>)> {
    Ok((
        StatusCode::CREATED,
        Json(projects::add_crew(&state, &ctx, id, b).await?),
    ))
}

/// Change a crew member; every field is replaced.
#[utoipa::path(put, path = "/project-crew/{id}", tag = "projects",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = CrewInput,
    responses((status = 200, body = CrewView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn update_crew(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<CrewInput>,
) -> ApiResult<Json<CrewView>> {
    Ok(Json(projects::update_crew(&state, &ctx, id, b).await?))
}

#[utoipa::path(delete, path = "/project-crew/{id}", tag = "projects", params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)),
    responses((status = 204), (status = 404, body = ErrorBody)), security(("session" = []), ("api_key" = [])))]
async fn remove_crew(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    projects::remove_crew(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Travel that belongs to the project rather than one date: the drive
/// between two cities on a day off, the flight home. Change or remove it at
/// `/travel/{id}`.
#[utoipa::path(post, path = "/projects/{id}/travel", tag = "projects",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = TravelInput,
    responses((status = 201, body = TravelView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn add_project_travel(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<TravelInput>,
) -> ApiResult<(StatusCode, Json<TravelView>)> {
    Ok((
        StatusCode::CREATED,
        Json(travel::create_travel(&state, &ctx, Owner::Project(id), b).await?),
    ))
}

/// Income or a cost of the project as a whole: rehearsals, insurance, a
/// tour manager's wage, sponsorship. Change or remove it at `/money-lines/{id}`.
#[utoipa::path(post, path = "/projects/{id}/money", tag = "projects",
    params(WorkspaceHeader, IdempotencyHeader, ("id" = Uuid, Path)), request_body = MoneyLineInput,
    responses((status = 201, body = MoneyLineView), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn add_project_line(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Path(id): Path<Uuid>,
    Json(b): Json<MoneyLineInput>,
) -> ApiResult<(StatusCode, Json<MoneyLineView>)> {
    Ok((
        StatusCode::CREATED,
        Json(travel::create_line(&state, &ctx, Owner::Project(id), b).await?),
    ))
}
