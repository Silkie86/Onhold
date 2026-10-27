//! Stored responses for retried writes (`Idempotency-Key`). Only reachable
//! through the `idem_*` SECURITY DEFINER functions.

use sqlx::PgPool;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Stored {
    pub request_hash: Vec<u8>,
    /// `None` while the original request is still running.
    pub status: Option<i16>,
    pub content_type: Option<String>,
    pub body: Option<Vec<u8>>,
}

/// Claims `key` for this caller. `None` means the claim is ours and the
/// request should run; `Some` is the earlier attempt.
pub async fn claim(
    pool: &PgPool,
    scope: &[u8],
    key: &str,
    request_hash: &[u8],
) -> Result<Option<Stored>> {
    Ok(sqlx::query_as!(
        Stored,
        r#"SELECT request_hash AS "request_hash!", status, content_type, body FROM idem_claim($1, $2, $3)"#,
        scope,
        key,
        request_hash
    )
    .fetch_optional(pool)
    .await?)
}

pub async fn complete(
    pool: &PgPool,
    scope: &[u8],
    key: &str,
    status: i16,
    content_type: Option<&str>,
    body: &[u8],
) -> Result<()> {
    sqlx::query!(
        "SELECT idem_complete($1, $2, $3, $4, $5)",
        scope,
        key,
        status,
        content_type,
        body
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn release(pool: &PgPool, scope: &[u8], key: &str) -> Result<()> {
    sqlx::query!("SELECT idem_release($1, $2)", scope, key)
        .execute(pool)
        .await?;
    Ok(())
}
