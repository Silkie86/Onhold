//! The event outbox. Every meaningful change appends a row in the same
//! transaction; the worker drains it from M2.

use core_domain::WorkspaceId;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

/// Appends an event. `workspace` must match the transaction's workspace
/// (`None` for user-level events such as a magic link request).
pub async fn emit(
    conn: &mut PgConnection,
    workspace: Option<WorkspaceId>,
    topic: &str,
    payload: &Value,
) -> Result<Uuid> {
    let id = core_domain::ids::new_id();
    sqlx::query!(
        "INSERT INTO outbox (id, workspace_id, topic, payload) VALUES ($1, $2, $3, $4)",
        id,
        workspace.map(|w| w.0),
        topic,
        payload
    )
    .execute(conn)
    .await?;
    Ok(id)
}
