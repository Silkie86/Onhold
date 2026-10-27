//! Per-workspace counters: quote numbers now, invoice numbers later.

use sqlx::PgConnection;

use crate::Result;

/// The next value of the counter `name`, starting at 1. Taken inside the
/// caller's transaction, so a rolled-back create leaves no gap and two
/// creates never share a number (the row stays locked until commit).
pub async fn next(conn: &mut PgConnection, name: &str) -> Result<i64> {
    Ok(sqlx::query_scalar!(
        r#"INSERT INTO workspace_counters (workspace_id, name, value) VALUES (app_workspace_id(), $1, 1)
           ON CONFLICT (workspace_id, name) DO UPDATE SET value = workspace_counters.value + 1
           RETURNING value"#,
        name
    )
    .fetch_one(conn)
    .await?)
}
