//! The service layer: every operation authorizes once, does its work in one
//! transaction, and records the change in the audit log and the event outbox
//! before committing. Handlers only translate HTTP to and from these calls.

pub mod admin;
pub mod auth;
pub mod tenancy;

use core_db::Tx;
use core_domain::Ctx;
use serde_json::{Value, json};
use uuid::Uuid;

/// A change to record: one audit row and one outbox event, same transaction.
pub struct Change<'a> {
    /// Event topic and audit action, e.g. `member.role_changed`.
    pub topic: &'a str,
    pub resource: &'a str,
    pub object_id: Option<Uuid>,
    pub before: Option<Value>,
    pub after: Option<Value>,
    /// Extra event fields consumers need but the audit log must not keep
    /// (for example an invitation link). Merged into the event payload.
    pub event_extra: Option<Value>,
}

pub async fn record(tx: &mut Tx, ctx: &Ctx, change: Change<'_>) -> core_db::Result<()> {
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
        "actor": {
            "user_id": ctx.user_id(),
            "api_key_id": ctx.api_key_id(),
            "impersonator_id": ctx.impersonator(),
        },
        "before": change.before,
        "after": change.after,
    });
    if let (Some(extra), Some(obj)) = (change.event_extra, payload.as_object_mut())
        && let Value::Object(extra) = extra
    {
        obj.extend(extra);
    }
    core_db::outbox::emit(tx, Some(ctx.workspace_id), change.topic, &payload).await?;
    Ok(())
}
