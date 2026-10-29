//! The service layer: every operation authorizes once, does its work in one
//! transaction, and records the change in the audit log and the event outbox
//! before committing. Handlers only translate HTTP to and from these calls.

pub mod accounting;
pub mod admin;
pub mod app_sign_in;
pub mod auth;
pub mod automation;
pub mod availability;
pub mod bookings;
pub mod calendar_feeds;
pub mod calendar_push;
pub mod contacts;
pub mod context;
pub mod crm;
pub mod documents;
pub mod enquiries;
pub mod events;
pub mod fields;
pub mod files;
pub mod finance;
pub mod follow_ups;
pub mod imports;
pub mod inbox;
pub mod integrations;
pub mod invoices;
pub mod jobs;
pub mod links;
pub mod mail;
pub mod messaging;
pub mod organisations;
pub mod payments;
pub mod pdf_imports;
pub mod pipelines;
pub mod pitch_follow_ups;
pub mod pitch_sending;
pub mod pitches;
pub mod pitching;
pub mod portal;
pub mod projects;
pub mod quotes;
pub mod records;
pub mod reports;
pub mod rules;
pub mod search;
pub mod sender_domains;
pub mod settlements;
pub mod setup;
pub mod signatures;
pub mod stripe;
pub mod talent;
pub mod talent_calendars;
pub mod tasks;
pub mod templates;
pub mod tenancy;
pub mod things;
pub mod travel;
pub mod views;
pub mod webhooks;

use core_db::Tx;
use core_domain::Ctx;
use core_domain::crypto::Keyring;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};

/// A change to record: one audit row and one outbox event, same transaction.
pub struct Change<'a> {
    /// Event topic and audit action, e.g. `member.role_changed`.
    pub topic: &'a str,
    pub resource: &'a str,
    pub object_id: Option<Uuid>,
    pub before: Option<Value>,
    pub after: Option<Value>,
    /// Extra event fields consumers need that the audit log need not keep.
    /// Merged into the event payload, which webhooks and the event stream
    /// expose, so never put secrets here (see [`record_with_secret`]).
    pub event_extra: Option<Value>,
}

pub async fn record(tx: &mut Tx, ctx: &Ctx, change: Change<'_>) -> core_db::Result<()> {
    record_inner(tx, ctx, change, None).await.map(|_| ())
}

/// Like [`record`], with data only the event's one consumer may read (an
/// invitation link for the mailer). It is encrypted into the outbox row,
/// moved to the job that sends the mail, and never published.
pub async fn record_with_secret(
    tx: &mut Tx,
    ctx: &Ctx,
    change: Change<'_>,
    keyring: &Keyring,
    secret: &Value,
) -> core_db::Result<Uuid> {
    record_inner(tx, ctx, change, Some((keyring, secret))).await
}

fn secret_aad(event_id: Uuid) -> Vec<u8> {
    [b"outbox-secret:".as_slice(), event_id.as_bytes()].concat()
}

/// Encrypts an event's secret, bound to that event.
pub fn seal_event_secret(keyring: &Keyring, event_id: Uuid, secret: &Value) -> Vec<u8> {
    keyring.encrypt(secret.to_string().as_bytes(), &secret_aad(event_id))
}

pub fn open_event_secret(keyring: &Keyring, event_id: Uuid, sealed: &[u8]) -> ApiResult<Value> {
    let plain = keyring
        .decrypt(sealed, &secret_aad(event_id))
        .map_err(ApiError::internal)?;
    serde_json::from_slice(&plain).map_err(ApiError::internal)
}

/// The actor block every event payload carries.
pub fn actor_json(ctx: &Ctx) -> Value {
    json!({
        "user_id": ctx.user_id(),
        "api_key_id": ctx.api_key_id(),
        "impersonator_id": ctx.impersonator(),
        "rule_id": ctx.rule_id(),
        "rule_depth": ctx.rule_depth(),
    })
}

async fn record_inner(
    tx: &mut Tx,
    ctx: &Ctx,
    change: Change<'_>,
    secret: Option<(&Keyring, &Value)>,
) -> core_db::Result<Uuid> {
    core_db::audit::record(
        tx,
        ctx,
        core_db::audit::Entry {
            action: change.topic,
            resource: change.resource,
            object_id: change.object_id,
            before: change.before.clone(),
            after: change.after.clone(),
        },
    )
    .await?;

    let mut payload = json!({
        "object_id": change.object_id,
        "resource": change.resource,
        "actor": actor_json(ctx),
        "before": change.before,
        "after": change.after,
    });
    if let (Some(extra), Some(obj)) = (change.event_extra, payload.as_object_mut())
        && let Value::Object(extra) = extra
    {
        obj.extend(extra);
    }
    let id = core_domain::ids::new_id();
    let sealed = secret.map(|(keyring, value)| seal_event_secret(keyring, id, value));
    core_db::outbox::emit_with_secret(
        tx,
        id,
        Some(ctx.workspace_id),
        change.topic,
        &payload,
        sealed.as_deref(),
    )
    .await
}

/// A transaction in the context's workspace, as the context's user.
pub(crate) async fn begin(state: &crate::AppState, ctx: &Ctx) -> ApiResult<Tx> {
    Ok(core_db::begin(&state.db, Some(ctx.workspace_id), ctx.user_id()).await?)
}

/// Fails with 412 when the client's `If-Match` version is stale.
pub(crate) fn check_version(current: i64, expected: Option<i64>) -> ApiResult<()> {
    match expected {
        Some(v) if v != current => Err(ApiError::version_conflict()),
        _ => Ok(()),
    }
}

/// Trims optional text; blank becomes `None`. Errors past `max` characters.
pub(crate) fn optional_text(
    field: &str,
    value: Option<String>,
    max: usize,
) -> ApiResult<Option<String>> {
    match value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()) {
        Some(v) if v.chars().count() > max => Err(ApiError::invalid(
            field,
            format!("must be at most {max} characters"),
        )),
        v => Ok(v),
    }
}

pub(crate) fn required_text(field: &str, value: &str, max: usize) -> ApiResult<String> {
    let v = value.trim();
    if v.is_empty() || v.chars().count() > max {
        return Err(ApiError::invalid(
            field,
            format!("must be 1 to {max} characters"),
        ));
    }
    Ok(v.to_owned())
}
