//! Settings for the whole platform, such as the price list.

use serde_json::Value;
use sqlx::PgConnection;

use crate::Result;

pub async fn get(conn: &mut PgConnection, key: &str) -> Result<Option<Value>> {
    Ok(
        sqlx::query_scalar!("SELECT value FROM platform_settings WHERE key = $1", key)
            .fetch_optional(conn)
            .await?,
    )
}

/// Raises unless the user in context is a superadmin.
pub async fn admin_set(conn: &mut PgConnection, key: &str, value: &Value) -> Result<()> {
    sqlx::query!("SELECT admin_set_platform_setting($1, $2)", key, value)
        .execute(conn)
        .await?;
    Ok(())
}
