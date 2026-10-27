//! Password hashing (argon2id, OWASP-recommended defaults) and policy.
//! Hashing is CPU-heavy, so it runs on the blocking pool.

use std::sync::LazyLock;

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};

use crate::error::{ApiError, ApiResult};

pub const MIN_LENGTH: usize = 12;
pub const MAX_LENGTH: usize = 256;

/// A hash of a random password, verified against when an account does not
/// exist so that "no such user" takes as long as "wrong password".
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| {
    #[allow(clippy::expect_used)]
    hash_blocking(&core_domain::crypto::new_token()).expect("hashing a random password")
});

/// Length bounds only, per NIST SP 800-63B: no composition rules. The
/// breached-password check arrives with the M8 security work.
pub fn check_policy(password: &str, email: &str) -> ApiResult<()> {
    let chars = password.chars().count();
    if chars < MIN_LENGTH {
        return Err(ApiError::BadRequest {
            code: "weak_password",
            message: format!("use at least {MIN_LENGTH} characters"),
            field: Some("password"),
        });
    }
    if chars > MAX_LENGTH {
        return Err(ApiError::BadRequest {
            code: "weak_password",
            message: format!("use at most {MAX_LENGTH} characters"),
            field: Some("password"),
        });
    }
    if password.eq_ignore_ascii_case(email) {
        return Err(ApiError::BadRequest {
            code: "weak_password",
            message: "don't use your email address as your password".into(),
            field: Some("password"),
        });
    }
    Ok(())
}

fn hash_blocking(password: &str) -> Result<String, String> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

fn verify_blocking(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(err) => {
            tracing::error!(error = %err, "stored password hash does not parse");
            false
        }
    }
}

pub async fn hash(password: &str) -> ApiResult<String> {
    let password = password.to_owned();
    tokio::task::spawn_blocking(move || hash_blocking(&password))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)
}

/// Verifies `password` against `hash`, or against a dummy hash when there is
/// none, so timing does not reveal whether the account exists.
pub async fn verify(password: &str, hash: Option<&str>) -> ApiResult<bool> {
    let password = password.to_owned();
    let (hash, real) = match hash {
        Some(h) => (h.to_owned(), true),
        None => (DUMMY_HASH.clone(), false),
    };
    let ok = tokio::task::spawn_blocking(move || verify_blocking(&password, &hash))
        .await
        .map_err(ApiError::internal)?;
    Ok(ok && real)
}

/// Builds the dummy hash ahead of the first sign-in.
pub fn warm_up() {
    LazyLock::force(&DUMMY_HASH);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hash_and_verify() {
        let h = hash("correct horse battery staple").await.expect("hashes");
        assert!(h.starts_with("$argon2id$"));
        assert!(
            verify("correct horse battery staple", Some(&h))
                .await
                .expect("verifies")
        );
        assert!(
            !verify("Correct horse battery staple", Some(&h))
                .await
                .expect("verifies")
        );
        assert!(!verify("anything", None).await.expect("dummy verify runs"));
    }

    #[test]
    fn policy() {
        assert!(check_policy("short", "a@b.test").is_err());
        assert!(check_policy(&"x".repeat(257), "a@b.test").is_err());
        assert!(check_policy("someone@example.test", "SOMEONE@example.test").is_err());
        assert!(check_policy("a perfectly fine passphrase", "a@b.test").is_ok());
    }
}
