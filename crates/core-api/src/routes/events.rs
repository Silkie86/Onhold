//! The event stream: the changes feed offline and desktop clients sync from,
//! by cursor or live over server-sent events.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::time::Duration;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use core_db::outbox::Event;
use core_domain::Ctx;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::Instant;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::WorkspaceHeader;
use crate::AppState;
use crate::error::{ApiError, ApiResult, ErrorBody};
use crate::extract::WsCtx;
use crate::services::events::{self as svc, EventView};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_events))
        .routes(routes!(stream_events))
}

/// How many events a stream reads per query.
const STREAM_BATCH: i64 = 200;
/// How often a stream checks for events when no notification arrives (the
/// notification connection can drop; this is the fallback).
const POLL_EVERY: Duration = Duration::from_secs(15);
/// Streams end after this long; the browser reconnects with `Last-Event-ID`
/// and the caller's access is checked again.
const STREAM_FOR: Duration = Duration::from_secs(15 * 60);

fn topics(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct EventsQuery {
    /// Events after this position (`seq`); 0 or absent for the oldest kept.
    pub after: Option<i64>,
    /// Comma-separated topics or patterns, e.g. `thing.*,pipeline.updated`.
    pub topics: Option<String>,
    /// 1 to 500, default 100.
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EventPage {
    /// Oldest first.
    pub events: Vec<EventView>,
    /// Pass as `after` next time.
    pub next_after: i64,
    /// True when there may be more right now.
    pub has_more: bool,
    /// The newest position in the stream.
    pub latest: i64,
}

/// What changed, in order. To sync: load everything, remember `latest`, then
/// read from it. Each event names the record that changed; fetch it if you
/// need it. A 410 `cursor_expired` means the events after your cursor are no
/// longer kept: load everything again.
#[utoipa::path(get, path = "/events", tag = "events", params(WorkspaceHeader, EventsQuery),
    responses((status = 200, body = EventPage), (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 410, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn list_events(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    Query(q): Query<EventsQuery>,
) -> ApiResult<Json<EventPage>> {
    let after = q.after.unwrap_or(0).max(0);
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let events = svc::page(&state, &ctx, after, &topics(q.topics.as_deref()), limit).await?;
    let latest = svc::latest(&state, &ctx).await?;
    Ok(Json(EventPage {
        next_after: events.last().map_or(after, |e| e.seq),
        has_more: events.len() as i64 == limit,
        events: events.iter().map(svc::view).collect(),
        latest,
    }))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct StreamQuery {
    /// Start after this position; absent means only new events. The
    /// `Last-Event-ID` header, sent by browsers on reconnect, wins.
    pub after: Option<i64>,
    /// Comma-separated topics or patterns.
    pub topics: Option<String>,
}

struct Live {
    state: AppState,
    ctx: Ctx,
    topics: Vec<String>,
    after: i64,
    pending: VecDeque<Event>,
    notices: broadcast::Receiver<core_domain::WorkspaceId>,
    until: Instant,
}

impl Live {
    /// Waits until the worker says this workspace has new events, the poll
    /// interval passes, or the stream's time is up.
    async fn wait(&mut self) {
        let deadline = (Instant::now() + POLL_EVERY).min(self.until);
        let ws = self.ctx.workspace_id;
        let notices = &mut self.notices;
        let _ = tokio::time::timeout_at(deadline, async {
            loop {
                match notices.recv().await {
                    Ok(id) if id == ws => return,
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => return,
                    Err(RecvError::Closed) => std::future::pending::<()>().await,
                }
            }
        })
        .await;
    }
}

/// The event stream, live. Each message's `id` is its position and its data
/// an event as in `GET /events`. Streams end after 15 minutes; browsers
/// reconnect on their own and resume from `Last-Event-ID`. A
/// `cursor_expired` message means events were missed: load everything again.
#[utoipa::path(get, path = "/events/stream", tag = "events", params(WorkspaceHeader, StreamQuery),
    responses((status = 200, content_type = "text/event-stream", body = EventView), (status = 400, body = ErrorBody),
        (status = 403, body = ErrorBody), (status = 410, body = ErrorBody)),
    security(("session" = []), ("api_key" = [])))]
async fn stream_events(
    State(state): State<AppState>,
    WsCtx(ctx): WsCtx,
    headers: HeaderMap,
    Query(q): Query<StreamQuery>,
) -> ApiResult<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>> {
    let topics = topics(q.topics.as_deref());
    let resume = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<i64>().ok());
    // Listen before the first read so nothing published in between is missed.
    let notices = state.events.subscribe(&state.db).await;
    let after = match resume.or(q.after) {
        Some(a) => a.max(0),
        None => svc::latest(&state, &ctx).await?,
    };
    // The first read happens here so a bad cursor or topic is an HTTP error.
    let first = svc::page(&state, &ctx, after, &topics, STREAM_BATCH).await?;
    let live = Live {
        state,
        ctx,
        topics,
        after,
        pending: first.into(),
        notices,
        until: Instant::now() + STREAM_FOR,
    };

    let stream = futures_util::stream::unfold(Some(live), |live| async move {
        let mut s = live?;
        loop {
            if let Some(e) = s.pending.pop_front() {
                s.after = e.seq;
                let msg = SseEvent::default()
                    .id(e.seq.to_string())
                    .json_data(svc::view(&e))
                    .unwrap_or_else(|_| SseEvent::default().comment("unencodable event"));
                return Some((Ok(msg), Some(s)));
            }
            if Instant::now() >= s.until {
                return None;
            }
            s.wait().await;
            match svc::page(&s.state, &s.ctx, s.after, &s.topics, STREAM_BATCH).await {
                Ok(events) => s.pending.extend(events),
                Err(ApiError::Gone { message, .. }) => {
                    let msg = SseEvent::default().event("cursor_expired").data(message);
                    return Some((Ok(msg), None));
                }
                Err(err) => {
                    tracing::warn!(error = %err, "event stream stopped");
                    return None;
                }
            }
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
