//! Signing in the desktop and mobile apps through the browser (RFC 8628
//! device authorization). Every call goes through a SECURITY DEFINER
//! function that touches one row named by an exact key.

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use core_domain::UserId;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

pub struct NewAppSignIn<'a> {
    pub id: Uuid,
    pub user_code: &'a str,
    pub poll_hash: &'a [u8],
    pub device_name: &'a str,
    pub ip: Option<IpAddr>,
    pub user_agent: Option<&'a str>,
    pub expires_at: DateTime<Utc>,
}

pub async fn start(conn: &mut PgConnection, new: NewAppSignIn<'_>) -> Result<()> {
    sqlx::query!(
        "SELECT app_sign_in_start($1, $2, $3, $4, $5, $6, $7)",
        new.id,
        new.user_code,
        new.poll_hash,
        new.device_name,
        new.ip as Option<IpAddr>,
        new.user_agent,
        new.expires_at
    )
    .execute(conn)
    .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct PendingAppSignIn {
    pub id: Uuid,
    pub device_name: String,
    pub ip: Option<IpAddr>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// A pending request by its code. Needs a user in context.
pub async fn find(conn: &mut PgConnection, user_code: &str) -> Result<Option<PendingAppSignIn>> {
    Ok(sqlx::query_as!(
        PendingAppSignIn,
        r#"SELECT id AS "id!", device_name AS "device_name!", ip AS "ip: IpAddr", user_agent,
                  created_at AS "created_at!", expires_at AS "expires_at!"
           FROM app_sign_in_find($1)"#,
        user_code
    )
    .fetch_optional(conn)
    .await?)
}

/// Approves or refuses as the user in context; false if nothing was pending.
pub async fn decide(conn: &mut PgConnection, user_code: &str, approve: bool) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT app_sign_in_decide($1, $2) AS "done!""#,
        user_code,
        approve
    )
    .fetch_one(conn)
    .await?)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Collected {
    Pending,
    Denied,
    Expired,
    Approved {
        user_id: UserId,
        device_name: String,
    },
}

/// What the polling app learns. `Approved` is returned once.
pub async fn collect(conn: &mut PgConnection, poll_hash: &[u8]) -> Result<Collected> {
    let row = sqlx::query!(
        r#"SELECT status AS "status!", user_id AS "user_id: UserId", device_name
           FROM app_sign_in_collect($1)"#,
        poll_hash
    )
    .fetch_one(conn)
    .await?;
    Ok(match (row.status.as_str(), row.user_id, row.device_name) {
        ("pending", ..) => Collected::Pending,
        ("denied", ..) => Collected::Denied,
        ("approved", Some(user_id), Some(device_name)) => Collected::Approved {
            user_id,
            device_name,
        },
        _ => Collected::Expired,
    })
}
