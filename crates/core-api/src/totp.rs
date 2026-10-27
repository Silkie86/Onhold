//! TOTP (RFC 6238: SHA-1, 6 digits, 30-second steps, one step of skew) and
//! recovery codes.

use core_domain::UserId;
use core_domain::crypto::{Keyring, hash_token, random_bytes};
use data_encoding::BASE32_NOPAD;
use totp_rs::{Builder, Totp};

use crate::error::{ApiError, ApiResult};

const ISSUER: &str = "OnHold";
pub const RECOVERY_CODE_COUNT: usize = 10;

/// Binds an encrypted seed to its user, so it cannot be copied to another row.
fn aad(user: UserId) -> Vec<u8> {
    format!("users.totp_secret:{user}").into_bytes()
}

pub fn new_secret() -> [u8; 20] {
    random_bytes::<20>()
}

pub fn encrypt_secret(keyring: &Keyring, user: UserId, secret: &[u8]) -> Vec<u8> {
    keyring.encrypt(secret, &aad(user))
}

pub fn decrypt_secret(keyring: &Keyring, user: UserId, sealed: &[u8]) -> ApiResult<Vec<u8>> {
    keyring
        .decrypt(sealed, &aad(user))
        .map_err(ApiError::internal)
}

fn build(secret: &[u8], account: &str) -> ApiResult<Totp> {
    Builder::new()
        .with_secret(secret.to_vec())
        .with_account_name(account.replace(':', ""))
        .with_issuer(Some(ISSUER))
        .build()
        .map_err(ApiError::internal)
}

/// The `otpauth://` URL an authenticator app scans, and the secret for manual entry.
pub fn provisioning(secret: &[u8], account: &str) -> ApiResult<(String, String)> {
    let url = build(secret, account)?
        .to_url()
        .map_err(ApiError::internal)?;
    Ok((url, BASE32_NOPAD.encode(secret)))
}

/// The time step `code` is valid for, if any. Callers must reject a step
/// that was already used.
pub fn check(secret: &[u8], code: &str, now_unix: u64) -> ApiResult<Option<i64>> {
    let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(None);
    }
    let totp = build(secret, "check")?;
    Ok(totp
        .check(&code, now_unix)
        .and_then(|step| i64::try_from(step).ok()))
}

/// Ten single-use codes, `xxxx-xxxx-xxxx-xxxx` (80 bits each), with their stored hashes.
pub fn new_recovery_codes() -> (Vec<String>, Vec<Vec<u8>>) {
    (0..RECOVERY_CODE_COUNT)
        .map(|_| {
            let raw = BASE32_NOPAD
                .encode(&random_bytes::<10>())
                .to_ascii_lowercase();
            let code = raw
                .as_bytes()
                .chunks(4)
                .map(|c| String::from_utf8_lossy(c).into_owned())
                .collect::<Vec<_>>()
                .join("-");
            let hash = hash_recovery_code(&code);
            (code, hash)
        })
        .unzip()
}

/// Normalises (case, dashes, spaces) before hashing so users can type codes loosely.
pub fn hash_recovery_code(code: &str) -> Vec<u8> {
    let normalised: String = code
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    hash_token(&normalised)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_check_within_one_step() {
        let secret = new_secret();
        let totp = build(&secret, "a@b.test").expect("builds");
        let now = 1_800_000_000;
        let code = totp.generate(now).to_string();
        assert_eq!(
            check(&secret, &code, now).expect("checks"),
            Some((now / 30) as i64)
        );
        assert!(
            check(&secret, &code, now + 30).expect("checks").is_some(),
            "one step of skew"
        );
        assert!(check(&secret, &code, now + 90).expect("checks").is_none());
        assert!(check(&secret, "12345", now).expect("checks").is_none());
        assert!(check(&secret, "abcdef", now).expect("checks").is_none());
    }

    #[test]
    fn provisioning_url_names_the_issuer() {
        let (url, manual) = provisioning(&new_secret(), "a@b.test").expect("url");
        assert!(url.starts_with("otpauth://totp/OnHold:"));
        assert_eq!(manual.len(), 32);
    }

    #[test]
    fn recovery_codes_are_unique_and_forgiving() {
        let (codes, hashes) = new_recovery_codes();
        assert_eq!(codes.len(), RECOVERY_CODE_COUNT);
        let unique: std::collections::BTreeSet<_> = hashes.iter().collect();
        assert_eq!(unique.len(), RECOVERY_CODE_COUNT);
        assert_eq!(
            hash_recovery_code(&codes[0].to_uppercase().replace('-', " ")),
            hashes[0]
        );
    }

    #[test]
    fn sealed_secrets_are_bound_to_their_user() {
        let ring = Keyring::ephemeral();
        let (a, b) = (UserId::new(), UserId::new());
        let sealed = encrypt_secret(&ring, a, b"seed");
        assert_eq!(
            decrypt_secret(&ring, a, &sealed).expect("decrypts"),
            b"seed"
        );
        assert!(decrypt_secret(&ring, b, &sealed).is_err());
    }
}
