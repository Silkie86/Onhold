//! Clearing out what is only kept for a while: delivery and rule logs,
//! Stripe's payloads, and sign-ins and links that ended. Worker role.

use chrono::{DateTime, Utc};
use sqlx::PgConnection;

use crate::Result;

/// Deletes webhook deliveries made before `before`.
pub async fn delete_webhook_deliveries_before(
    conn: &mut PgConnection,
    before: DateTime<Utc>,
) -> Result<u64> {
    let done = sqlx::query!(
        "DELETE FROM webhook_deliveries WHERE created_at < $1",
        before
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected())
}

/// Deletes the runs where a rule did nothing (its conditions did not
/// match, automations were paused), logged before `before`.
pub async fn delete_skipped_rule_runs_before(
    conn: &mut PgConnection,
    before: DateTime<Utc>,
) -> Result<u64> {
    let done = sqlx::query!(
        "DELETE FROM rule_runs WHERE outcome = 'skipped' AND created_at < $1",
        before
    )
    .execute(conn)
    .await?;
    Ok(done.rows_affected())
}

/// Empties the payloads of Stripe events handled before `before`; that
/// they came, their type and outcome stay. Returns how many.
pub async fn forget_stripe_payloads_before(
    conn: &mut PgConnection,
    before: DateTime<Utc>,
) -> Result<i64> {
    Ok(
        sqlx::query_scalar!(r#"SELECT stripe_events_forget_before($1) AS "n!""#, before)
            .fetch_one(conn)
            .await?,
    )
}

/// Deletes sessions, sign-in links, invitations and calendar links that
/// ended before `before`. Returns how many.
pub async fn forget_sign_ins_before(conn: &mut PgConnection, before: DateTime<Utc>) -> Result<i64> {
    Ok(
        sqlx::query_scalar!(r#"SELECT sign_ins_forget_before($1) AS "n!""#, before)
            .fetch_one(conn)
            .await?,
    )
}
