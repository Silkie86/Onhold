//! Users, recovery codes, magic links and OIDC identities. Rows are visible
//! to the user themselves and, for `users`, to fellow members of the
//! workspace in context.

use chrono::{DateTime, Utc};
use core_domain::UserId;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct User {
    pub id: UserId,
    pub email: String,
    pub name: String,
    pub has_password: bool,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub is_superadmin: bool,
    pub totp_enabled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

pub async fn get(conn: &mut PgConnection, id: UserId) -> Result<Option<User>> {
    Ok(sqlx::query_as!(
        User,
        r#"SELECT id AS "id: UserId", email::text AS "email!", name,
                  password_hash IS NOT NULL AS "has_password!", email_verified_at, is_superadmin,
                  totp_enabled_at, created_at
           FROM users WHERE id = $1 AND deleted_at IS NULL"#,
        id.0
    )
    .fetch_optional(conn)
    .await?)
}

pub struct NewUser<'a> {
    pub id: UserId,
    pub email: &'a str,
    pub name: &'a str,
    pub password_hash: Option<&'a str>,
    pub email_verified: bool,
}

/// Inserts a user. The transaction's user must be `new.id` (row-level security).
pub async fn insert(conn: &mut PgConnection, new: NewUser<'_>) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO users (id, email, name, password_hash, password_changed_at, email_verified_at)
           VALUES ($1, $2::text::citext, $3, $4,
                   CASE WHEN $4::text IS NULL THEN NULL ELSE now() END,
                   CASE WHEN $5 THEN now() ELSE NULL END)"#,
        new.id.0,
        new.email,
        new.name,
        new.password_hash,
        new.email_verified
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn password_hash(conn: &mut PgConnection, id: UserId) -> Result<Option<String>> {
    Ok(
        sqlx::query_scalar!("SELECT password_hash FROM users WHERE id = $1", id.0)
            .fetch_optional(conn)
            .await?
            .flatten(),
    )
}

pub async fn set_password(conn: &mut PgConnection, id: UserId, hash: &str) -> Result<()> {
    sqlx::query!(
        "UPDATE users SET password_hash = $2, password_changed_at = now() WHERE id = $1",
        id.0,
        hash
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn mark_email_verified(conn: &mut PgConnection, id: UserId) -> Result<()> {
    sqlx::query!(
        "UPDATE users SET email_verified_at = coalesce(email_verified_at, now()) WHERE id = $1",
        id.0
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Records a failed sign-in; locks the account for `lock_for` once `max_failures` is reached.
pub async fn record_login_failure(
    conn: &mut PgConnection,
    id: UserId,
    max_failures: i32,
    lock_for_secs: f64,
) -> Result<()> {
    sqlx::query!(
        r#"UPDATE users
           SET failed_login_count = failed_login_count + 1,
               locked_until = CASE WHEN failed_login_count + 1 >= $2
                                   THEN now() + make_interval(secs => $3) ELSE locked_until END
           WHERE id = $1"#,
        id.0,
        max_failures,
        lock_for_secs
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// True while too many failed sign-ins (passwords or second-factor codes) lock the account.
/// Holds the user's row until the transaction ends, so one user's guesses are judged one at a
/// time and none slips past the lockout.
pub async fn is_locked(conn: &mut PgConnection, id: UserId) -> Result<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT coalesce(locked_until > now(), false) AS "locked!" FROM users WHERE id = $1 FOR UPDATE"#,
        id.0
    )
    .fetch_optional(conn)
    .await?
    .unwrap_or(false))
}

pub async fn reset_login_failures(conn: &mut PgConnection, id: UserId) -> Result<()> {
    sqlx::query!(
        "UPDATE users SET failed_login_count = 0, locked_until = NULL WHERE id = $1 AND (failed_login_count <> 0 OR locked_until IS NOT NULL)",
        id.0
    )
    .execute(conn)
    .await?;
    Ok(())
}

// TOTP ------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TotpState {
    pub secret_enc: Option<Vec<u8>>,
    pub pending_secret_enc: Option<Vec<u8>>,
    pub enabled_at: Option<DateTime<Utc>>,
    pub last_step: Option<i64>,
}

pub async fn totp_state(conn: &mut PgConnection, id: UserId) -> Result<Option<TotpState>> {
    Ok(sqlx::query_as!(
        TotpState,
        r#"SELECT totp_secret_enc AS secret_enc, totp_pending_secret_enc AS pending_secret_enc,
                  totp_enabled_at AS enabled_at, totp_last_step AS last_step
           FROM users WHERE id = $1"#,
        id.0
    )
    .fetch_optional(conn)
    .await?)
}

pub async fn set_totp_pending(
    conn: &mut PgConnection,
    id: UserId,
    secret_enc: &[u8],
) -> Result<()> {
    sqlx::query!(
        "UPDATE users SET totp_pending_secret_enc = $2 WHERE id = $1",
        id.0,
        secret_enc
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn enable_totp(conn: &mut PgConnection, id: UserId, step: i64) -> Result<()> {
    sqlx::query!(
        r#"UPDATE users SET totp_secret_enc = totp_pending_secret_enc, totp_pending_secret_enc = NULL,
                            totp_enabled_at = now(), totp_last_step = $2
           WHERE id = $1 AND totp_pending_secret_enc IS NOT NULL"#,
        id.0,
        step
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn disable_totp(conn: &mut PgConnection, id: UserId) -> Result<()> {
    sqlx::query!(
        r#"UPDATE users SET totp_secret_enc = NULL, totp_pending_secret_enc = NULL,
                            totp_enabled_at = NULL, totp_last_step = NULL
           WHERE id = $1"#,
        id.0
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "UPDATE user_recovery_codes SET used_at = now() WHERE user_id = $1 AND used_at IS NULL",
        id.0
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Records the time step a code was accepted for. Returns false if the step
/// (or a later one) was already used, which makes the code a replay.
pub async fn advance_totp_step(conn: &mut PgConnection, id: UserId, step: i64) -> Result<bool> {
    let updated = sqlx::query!(
        r#"UPDATE users SET totp_last_step = $2
           WHERE id = $1 AND (totp_last_step IS NULL OR totp_last_step < $2)"#,
        id.0,
        step
    )
    .execute(conn)
    .await?;
    Ok(updated.rows_affected() == 1)
}

pub async fn replace_recovery_codes(
    conn: &mut PgConnection,
    id: UserId,
    hashes: &[Vec<u8>],
) -> Result<()> {
    sqlx::query!(
        "UPDATE user_recovery_codes SET used_at = now() WHERE user_id = $1 AND used_at IS NULL",
        id.0
    )
    .execute(&mut *conn)
    .await?;
    for hash in hashes {
        sqlx::query!(
            "INSERT INTO user_recovery_codes (id, user_id, code_hash) VALUES ($1, $2, $3)",
            core_domain::ids::new_id(),
            id.0,
            hash
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Consumes a recovery code by hash. Returns false if none matched.
pub async fn use_recovery_code(conn: &mut PgConnection, id: UserId, hash: &[u8]) -> Result<bool> {
    let used = sqlx::query!(
        r#"UPDATE user_recovery_codes SET used_at = now()
           WHERE id = (SELECT id FROM user_recovery_codes
                       WHERE user_id = $1 AND code_hash = $2 AND used_at IS NULL
                       LIMIT 1 FOR UPDATE)"#,
        id.0,
        hash
    )
    .execute(conn)
    .await?;
    Ok(used.rows_affected() == 1)
}

// Magic links ---------------------------------------------------------------------

pub async fn insert_magic_link(
    conn: &mut PgConnection,
    user: UserId,
    token_hash: &[u8],
    expires_at: DateTime<Utc>,
) -> Result<Uuid> {
    let id = core_domain::ids::new_id();
    sqlx::query!(
        "INSERT INTO magic_links (id, user_id, token_hash, expires_at) VALUES ($1, $2, $3, $4)",
        id,
        user.0,
        token_hash,
        expires_at
    )
    .execute(conn)
    .await?;
    Ok(id)
}

/// Marks a magic link used. Returns false if it was already used (a race between two clicks).
pub async fn use_magic_link(conn: &mut PgConnection, id: Uuid) -> Result<bool> {
    let used = sqlx::query!(
        "UPDATE magic_links SET used_at = now() WHERE id = $1 AND used_at IS NULL",
        id
    )
    .execute(conn)
    .await?;
    Ok(used.rows_affected() == 1)
}

// OIDC identities --------------------------------------------------------------------

pub async fn link_oidc_identity(
    conn: &mut PgConnection,
    user: UserId,
    provider: &str,
    subject: &str,
    email: Option<&str>,
) -> Result<()> {
    sqlx::query!(
        r#"INSERT INTO oidc_identities (id, user_id, provider, subject, email, last_used_at)
           VALUES ($1, $2, $3, $4, $5::text::citext, now())"#,
        core_domain::ids::new_id(),
        user.0,
        provider,
        subject,
        email
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn touch_oidc_identity(
    conn: &mut PgConnection,
    user: UserId,
    provider: &str,
    subject: &str,
) -> Result<()> {
    sqlx::query!(
        "UPDATE oidc_identities SET last_used_at = now() WHERE user_id = $1 AND provider = $2 AND subject = $3",
        user.0,
        provider,
        subject
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Grants or revokes platform operator rights. Only the owner pool can do
/// this: the app role has no UPDATE grant on `is_superadmin`. Returns false
/// when no such user exists.
pub async fn set_superadmin(owner: &sqlx::PgPool, email: &str, on: bool) -> Result<bool> {
    let done = sqlx::query!(
        "UPDATE users SET is_superadmin = $2 WHERE email = $1::text::citext AND deleted_at IS NULL",
        email,
        on
    )
    .execute(owner)
    .await?;
    Ok(done.rows_affected() == 1)
}
