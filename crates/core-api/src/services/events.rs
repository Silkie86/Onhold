//! The workspace's event stream: every published event in order, readable
//! by cursor (`after=<seq>`) or live over server-sent events. Offline and
//! desktop clients use it as their changes feed: an event names the record
//! that changed; the client fetches it if it cares.
//!
//! Events carry no record data (no `before`/`after`), only what changed and
//! who changed it, so reading the stream never reveals more than the
//! records' own endpoints would.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use core_db::outbox::Event;
use core_domain::{Ctx, Permission, WorkspaceId};
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tokio::sync::{OnceCell, broadcast};
use uuid::Uuid;

use super::begin;
use crate::AppState;
use crate::error::{ApiError, ApiResult};

/// The channel the worker notifies on after publishing, with the workspace id.
pub const CHANNEL: &str = "onhold_events";

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct EventView {
    pub id: Uuid,
    /// Position in the stream; pass the last one you saw as `after`.
    pub seq: i64,
    pub topic: String,
    pub resource: Option<String>,
    pub object_id: Option<Uuid>,
    #[schema(value_type = Object)]
    pub actor: Value,
    pub at: DateTime<Utc>,
}

pub fn view(e: &Event) -> EventView {
    EventView {
        id: e.id,
        seq: e.seq,
        topic: e.topic.clone(),
        resource: e
            .payload
            .get("resource")
            .and_then(Value::as_str)
            .map(str::to_owned),
        object_id: e
            .payload
            .get("object_id")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok()),
        actor: e.payload.get("actor").cloned().unwrap_or(Value::Null),
        at: e.created_at,
    }
}

pub fn check_topics(topics: &[String]) -> ApiResult<()> {
    if topics.len() > 20 {
        return Err(ApiError::invalid("topic", "at most 20 topics"));
    }
    for t in topics {
        if !core_engines::webhooks::is_valid_pattern(t) {
            return Err(ApiError::invalid(
                "topic",
                format!("`{t}` is not a topic or pattern such as `thing.*`"),
            ));
        }
    }
    Ok(())
}

/// Events after `after`, oldest first. Fails with 410 when events the
/// client has not seen were already deleted.
pub async fn page(
    state: &AppState,
    ctx: &Ctx,
    after: i64,
    topics: &[String],
    limit: i64,
) -> ApiResult<Vec<Event>> {
    ctx.authorize(Permission::EventRead)?;
    check_topics(topics)?;
    let mut tx = begin(state, ctx).await?;
    let pruned = core_db::outbox::pruned_through(&mut tx).await?;
    if after > 0 && after < pruned {
        return Err(ApiError::Gone {
            code: "cursor_expired",
            message: "events after this cursor are no longer kept; reload everything and start from `latest`".into(),
        });
    }
    let events = core_db::outbox::list(&mut tx, after, topics, limit.clamp(1, 500)).await?;
    tx.commit().await?;
    Ok(events)
}

/// The newest position in the stream (where a fresh client starts).
pub async fn latest(state: &AppState, ctx: &Ctx) -> ApiResult<i64> {
    ctx.authorize(Permission::EventRead)?;
    let mut tx = begin(state, ctx).await?;
    let (_, max) = core_db::outbox::bounds(&mut tx).await?;
    let pruned = core_db::outbox::pruned_through(&mut tx).await?;
    tx.commit().await?;
    Ok(max.unwrap_or(0).max(pruned))
}

/// Fans the worker's "new events" notifications out to every live stream in
/// this process over one database connection.
pub struct Hub {
    sender: broadcast::Sender<WorkspaceId>,
    started: OnceCell<()>,
}

impl Default for Hub {
    fn default() -> Self {
        Self {
            sender: broadcast::channel(1024).0,
            started: OnceCell::new(),
        }
    }
}

impl Hub {
    pub async fn subscribe(self: &Arc<Self>, pool: &PgPool) -> broadcast::Receiver<WorkspaceId> {
        let rx = self.sender.subscribe();
        self.started
            .get_or_init(|| async {
                tokio::spawn(listen(pool.clone(), self.sender.clone()));
            })
            .await;
        rx
    }
}

async fn listen(pool: PgPool, sender: broadcast::Sender<WorkspaceId>) {
    loop {
        let result: Result<(), sqlx::Error> = async {
            let mut listener = PgListener::connect_with(&pool).await?;
            listener.listen(CHANNEL).await?;
            loop {
                let n = listener.recv().await?;
                if let Ok(id) = n.payload().parse::<Uuid>() {
                    // No receivers is fine.
                    let _ = sender.send(WorkspaceId(id));
                }
            }
        }
        .await;
        if let Err(err) = result {
            tracing::warn!(error = %err, "event listener lost its connection; reconnecting");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
