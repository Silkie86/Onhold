//! Outbound webhooks: endpoints, their signing secrets, the delivery log,
//! replays and test pings. Deliveries themselves are jobs the worker runs
//! with [`prepare_delivery`] and [`record_delivery`].

use core_db::webhooks::{Delivery, Endpoint};
use core_domain::crypto::Keyring;
use core_domain::{Ctx, Permission, WorkspaceId};
use core_engines::webhooks as engine;
use core_integrations::http::WebhookClient;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Change, begin, check_version, optional_text, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

/// Sent only to the endpoint being tested.
pub const PING_TOPIC: &str = "webhook.ping";

fn aad(endpoint: Uuid) -> Vec<u8> {
    [b"webhook-secret:".as_slice(), endpoint.as_bytes()].concat()
}

fn seal(keyring: &Keyring, endpoint: Uuid, secret: &str) -> Vec<u8> {
    keyring.encrypt(secret.as_bytes(), &aad(endpoint))
}

fn open(keyring: &Keyring, endpoint: Uuid, sealed: &[u8]) -> ApiResult<String> {
    let plain = keyring
        .decrypt(sealed, &aad(endpoint))
        .map_err(ApiError::internal)?;
    String::from_utf8(plain).map_err(ApiError::internal)
}

fn check_url(state: &AppState, url: &str) -> ApiResult<String> {
    let client =
        WebhookClient::new(state.config.webhooks_allow_private).map_err(ApiError::internal)?;
    let parsed = client
        .check_url(url.trim())
        .map_err(|e| ApiError::invalid("url", e.to_string()))?;
    if parsed.as_str().len() > 2000 {
        return Err(ApiError::invalid("url", "must be at most 2000 characters"));
    }
    Ok(parsed.to_string())
}

fn check_events(events: &[String]) -> ApiResult<Vec<String>> {
    if events.is_empty() || events.len() > 100 {
        return Err(ApiError::invalid(
            "events",
            "list 1 to 100 topics, or `*` for everything",
        ));
    }
    for (i, e) in events.iter().enumerate() {
        if !engine::is_valid_pattern(e) {
            return Err(ApiError::invalid(
                format!("events[{i}]"),
                format!("`{e}` is not a topic or pattern such as `thing.*`"),
            ));
        }
    }
    let mut out = events.to_vec();
    out.sort();
    out.dedup();
    Ok(out)
}

fn view_json(e: &Endpoint) -> Value {
    json!({ "url": e.url, "description": e.description, "events": e.events, "enabled": e.enabled })
}

pub async fn list(state: &AppState, ctx: &Ctx) -> ApiResult<Vec<Endpoint>> {
    ctx.authorize(Permission::WebhookRead)?;
    let mut tx = begin(state, ctx).await?;
    let rows = core_db::webhooks::list(&mut tx).await?;
    tx.commit().await?;
    Ok(rows)
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<Endpoint> {
    ctx.authorize(Permission::WebhookRead)?;
    let mut tx = begin(state, ctx).await?;
    let e = core_db::webhooks::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(e)
}

/// Creates an endpoint; returns it with its signing secret, shown this once.
pub async fn create(
    state: &AppState,
    ctx: &Ctx,
    url: &str,
    description: Option<String>,
    events: &[String],
) -> ApiResult<(Endpoint, String)> {
    ctx.authorize(Permission::WebhookWrite)?;
    let url = check_url(state, url)?;
    let description = optional_text("description", description, 500)?;
    let events = check_events(events)?;
    let id = core_domain::ids::new_id();
    let secret = engine::new_secret();
    let mut tx = begin(state, ctx).await?;
    core_db::webhooks::insert(
        &mut tx,
        id,
        &url,
        description.as_deref(),
        &events,
        &seal(&state.config.keyring, id, &secret),
        ctx.user_id(),
    )
    .await?;
    let endpoint = core_db::webhooks::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "webhook.created",
            resource: "webhook",
            object_id: Some(id),
            before: None,
            after: Some(view_json(&endpoint)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok((endpoint, secret))
}

#[derive(Debug, Default)]
pub struct Patch {
    pub url: Option<String>,
    pub description: Option<Option<String>>,
    pub events: Option<Vec<String>>,
    pub enabled: Option<bool>,
}

pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    patch: Patch,
    if_match: Option<i64>,
) -> ApiResult<Endpoint> {
    ctx.authorize(Permission::WebhookWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = core_db::webhooks::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(before.version, if_match)?;
    let url = match &patch.url {
        Some(u) => check_url(state, u)?,
        None => before.url.clone(),
    };
    let description = optional_text(
        "description",
        patch.description.unwrap_or(before.description.clone()),
        500,
    )?;
    let events = match &patch.events {
        Some(e) => check_events(e)?,
        None => before.events.clone(),
    };
    let enabled = patch.enabled.unwrap_or(before.enabled);
    let values = core_db::webhooks::Values {
        url: &url,
        description: description.as_deref(),
        events: &events,
        enabled,
    };
    if !core_db::webhooks::update(&mut tx, id, values, if_match, ctx.user_id()).await? {
        return Err(ApiError::version_conflict());
    }
    let after = core_db::webhooks::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "webhook.updated",
            resource: "webhook",
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

pub async fn delete(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::WebhookWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = core_db::webhooks::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    core_db::webhooks::soft_delete(&mut tx, id, ctx.user_id()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "webhook.deleted",
            resource: "webhook",
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

/// Replaces the signing secret; the old one stops working at once.
pub async fn rotate_secret(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<String> {
    ctx.authorize(Permission::WebhookWrite)?;
    let secret = engine::new_secret();
    let mut tx = begin(state, ctx).await?;
    if !core_db::webhooks::set_secret(
        &mut tx,
        id,
        &seal(&state.config.keyring, id, &secret),
        ctx.user_id(),
    )
    .await?
    {
        return Err(ApiError::NotFound);
    }
    record(
        &mut tx,
        ctx,
        Change {
            topic: "webhook.secret_rotated",
            resource: "webhook",
            object_id: Some(id),
            before: None,
            after: None,
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(secret)
}

pub async fn deliveries(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    before: Option<Uuid>,
    limit: i64,
) -> ApiResult<Vec<Delivery>> {
    ctx.authorize(Permission::WebhookRead)?;
    let mut tx = begin(state, ctx).await?;
    core_db::webhooks::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let rows = core_db::webhooks::deliveries(&mut tx, id, before, limit.clamp(1, 200)).await?;
    tx.commit().await?;
    Ok(rows)
}

/// Queues the event for delivery to the endpoint again. Returns the job id.
pub async fn replay(state: &AppState, ctx: &Ctx, id: Uuid, event_id: Uuid) -> ApiResult<Uuid> {
    ctx.authorize(Permission::WebhookWrite)?;
    let mut tx = begin(state, ctx).await?;
    core_db::webhooks::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    core_db::outbox::get(&mut tx, event_id)
        .await?
        .ok_or_else(|| {
            ApiError::invalid("event_id", "there is no such event (it may have expired)")
        })?;
    let payload = json!({ "endpoint_id": id, "event_id": event_id, "replay": true });
    let job = core_db::jobs::enqueue(
        &mut tx,
        core_db::jobs::NewJob::new(
            Some(ctx.workspace_id),
            super::automation::JOB_WEBHOOK,
            &payload,
        ),
    )
    .await?
    .ok_or_else(|| ApiError::internal("replay job was not created"))?;
    tx.commit().await?;
    Ok(job)
}

/// Sends a `webhook.ping` event to just this endpoint.
pub async fn ping(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::WebhookWrite)?;
    let mut tx = begin(state, ctx).await?;
    core_db::webhooks::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: PING_TOPIC,
            resource: "webhook",
            object_id: Some(id),
            before: None,
            after: None,
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// Delivery (worker) ----------------------------------------------------------------------

/// The request a delivery makes.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub url: String,
    pub body: Vec<u8>,
    pub secret: String,
}

/// The JSON body receivers get: the event with its full data.
pub fn body(workspace: WorkspaceId, event: &core_db::outbox::Event) -> Value {
    json!({
        "id": event.id,
        "seq": event.seq,
        "topic": event.topic,
        "created_at": event.created_at,
        "workspace_id": workspace,
        "data": event.payload,
    })
}

/// Loads what a delivery needs; `None` when the endpoint was deleted or
/// turned off, or the event has expired (nothing to deliver).
pub async fn prepare_delivery(
    state: &AppState,
    workspace: WorkspaceId,
    endpoint_id: Uuid,
    event_id: Uuid,
) -> ApiResult<Option<Prepared>> {
    let mut tx = core_db::begin(&state.db, Some(workspace), None).await?;
    let Some(endpoint) = core_db::webhooks::get(&mut tx, endpoint_id)
        .await?
        .filter(|e| e.enabled)
    else {
        return Ok(None);
    };
    let Some(event) = core_db::outbox::get(&mut tx, event_id).await? else {
        return Ok(None);
    };
    let sealed = core_db::webhooks::secret(&mut tx, endpoint_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(Some(Prepared {
        url: endpoint.url,
        body: serde_json::to_vec(&body(workspace, &event)).map_err(ApiError::internal)?,
        secret: open(&state.config.keyring, endpoint_id, &sealed)?,
    }))
}

pub struct Outcome<'a> {
    pub succeeded: bool,
    pub status_code: Option<i32>,
    pub error: Option<&'a str>,
    pub response_excerpt: Option<&'a str>,
    pub duration_ms: i32,
}

pub async fn record_delivery(
    state: &AppState,
    workspace: WorkspaceId,
    endpoint_id: Uuid,
    event_id: Uuid,
    job_id: Uuid,
    attempt: i32,
    outcome: Outcome<'_>,
) -> ApiResult<()> {
    let mut tx = core_db::begin(&state.db, Some(workspace), None).await?;
    core_db::webhooks::record_delivery(
        &mut tx,
        core_db::webhooks::NewDelivery {
            endpoint_id,
            event_id,
            job_id: Some(job_id),
            attempt,
            succeeded: outcome.succeeded,
            status_code: outcome.status_code,
            error: outcome.error,
            response_excerpt: outcome.response_excerpt,
            duration_ms: outcome.duration_ms,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
