//! Tokens, hashing and encryption at rest.
//!
//! - Bearer secrets (session tokens, invitation and magic-link tokens, API key
//!   secrets) are 256 random bits; only their SHA-256 is stored. They are
//!   high-entropy, so a fast hash is the right tool (passwords use argon2id).
//! - Secrets the system must read back (TOTP seeds, later OAuth tokens) are
//!   encrypted with AES-256-GCM under a keyring, so keys can rotate: new data
//!   uses the first key, old data stays readable while its key is listed.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use data_encoding::BASE64URL_NOPAD;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Fills a fixed-size array from the operating system's CSPRNG.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    // The OS RNG failing is unrecoverable; nothing secure can be issued without it.
    #[allow(clippy::expect_used)]
    getrandom::fill(&mut buf).expect("operating system random number generator unavailable");
    buf
}

/// A new 256-bit secret, URL-safe base64 without padding (43 characters).
pub fn new_token() -> String {
    BASE64URL_NOPAD.encode(&random_bytes::<32>())
}

/// The stored form of a bearer secret.
pub fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("invalid keyring: {0}")]
    InvalidKeyring(String),
    #[error("ciphertext was encrypted with key `{0}`, which is not in the keyring")]
    UnknownKey(String),
    #[error("ciphertext is malformed or was tampered with")]
    Corrupt,
}

const FORMAT_V1: u8 = 1;
const NONCE_LEN: usize = 12;

/// AES-256-GCM keys by id. The first key encrypts; every key can decrypt.
#[derive(Clone)]
pub struct Keyring {
    keys: Vec<(String, Aes256Gcm)>,
}

impl std::fmt::Debug for Keyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ids: Vec<&str> = self.keys.iter().map(|(id, _)| id.as_str()).collect();
        f.debug_struct("Keyring").field("key_ids", &ids).finish()
    }
}

impl Keyring {
    /// Parses `id:base64key[,id:base64key…]`, each key 32 bytes in standard
    /// or URL-safe base64. The first entry is the active key.
    pub fn parse(spec: &str) -> Result<Self, CryptoError> {
        let mut keys = Vec::new();
        for entry in spec.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let (id, b64) = entry.split_once(':').ok_or_else(|| {
                CryptoError::InvalidKeyring(format!(
                    "`{id}` is not `id:base64key`",
                    id = entry.split(':').next().unwrap_or_default()
                ))
            })?;
            if id.is_empty()
                || id.len() > 32
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Err(CryptoError::InvalidKeyring(format!(
                    "key id `{id}` must be 1 to 32 letters, digits, `_` or `-`"
                )));
            }
            let raw = data_encoding::BASE64
                .decode(b64.as_bytes())
                .or_else(|_| BASE64URL_NOPAD.decode(b64.trim_end_matches('=').as_bytes()))
                .map_err(|_| {
                    CryptoError::InvalidKeyring(format!("key `{id}` is not valid base64"))
                })?;
            let cipher = Aes256Gcm::new_from_slice(&raw).map_err(|_| {
                CryptoError::InvalidKeyring(format!(
                    "key `{id}` must be 32 bytes, got {}",
                    raw.len()
                ))
            })?;
            if keys.iter().any(|(existing, _)| existing == id) {
                return Err(CryptoError::InvalidKeyring(format!(
                    "key id `{id}` appears twice"
                )));
            }
            keys.push((id.to_owned(), cipher));
        }
        if keys.is_empty() {
            return Err(CryptoError::InvalidKeyring("no keys given".into()));
        }
        Ok(Self { keys })
    }

    /// A keyring with one random key, for tests and throwaway dev setups.
    pub fn ephemeral() -> Self {
        let key = random_bytes::<32>();
        #[allow(clippy::expect_used)]
        let cipher = Aes256Gcm::new_from_slice(&key).expect("32-byte key");
        Self {
            keys: vec![("ephemeral".into(), cipher)],
        }
    }

    /// Encrypts `plaintext`, binding it to `context` (for example the column
    /// and row it belongs in) so a ciphertext cannot be moved elsewhere.
    pub fn encrypt(&self, plaintext: &[u8], context: &[u8]) -> Vec<u8> {
        let (id, cipher) = &self.keys[0];
        let nonce = random_bytes::<NONCE_LEN>();
        #[allow(clippy::expect_used)]
        let sealed = cipher
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: plaintext,
                    aad: context,
                },
            )
            .expect("AES-GCM encryption of an in-memory buffer cannot fail");
        let mut out = Vec::with_capacity(2 + id.len() + NONCE_LEN + sealed.len());
        out.push(FORMAT_V1);
        // Key ids are at most 32 bytes, checked when the keys are loaded.
        #[allow(clippy::expect_used)]
        out.push(u8::try_from(id.len()).expect("key ids are at most 32 bytes"));
        out.extend_from_slice(id.as_bytes());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        out
    }

    pub fn decrypt(&self, data: &[u8], context: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let (&version, rest) = data.split_first().ok_or(CryptoError::Corrupt)?;
        if version != FORMAT_V1 {
            return Err(CryptoError::Corrupt);
        }
        let (&id_len, rest) = rest.split_first().ok_or(CryptoError::Corrupt)?;
        let id_len = usize::from(id_len);
        if rest.len() < id_len + NONCE_LEN {
            return Err(CryptoError::Corrupt);
        }
        let (id, rest) = rest.split_at(id_len);
        let (nonce, sealed) = rest.split_at(NONCE_LEN);
        let id = std::str::from_utf8(id).map_err(|_| CryptoError::Corrupt)?;
        let (_, cipher) = self
            .keys
            .iter()
            .find(|(k, _)| k == id)
            .ok_or_else(|| CryptoError::UnknownKey(id.to_owned()))?;
        let nonce = Nonce::try_from(nonce).map_err(|_| CryptoError::Corrupt)?;
        cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: sealed,
                    aad: context,
                },
            )
            .map_err(|_| CryptoError::Corrupt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_b64() -> String {
        data_encoding::BASE64.encode(&random_bytes::<32>())
    }

    #[test]
    fn tokens_are_unique_and_hash_stably() {
        let a = new_token();
        let b = new_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
        assert_eq!(hash_token(&a), hash_token(&a));
        assert_ne!(hash_token(&a), hash_token(&b));
    }

    #[test]
    fn encrypt_round_trips_and_binds_context() {
        let ring = Keyring::parse(&format!("k1:{}", key_b64())).expect("keyring");
        let sealed = ring.encrypt(b"seed", b"users.totp:42");
        assert_eq!(
            ring.decrypt(&sealed, b"users.totp:42").expect("decrypts"),
            b"seed"
        );
        assert!(matches!(
            ring.decrypt(&sealed, b"users.totp:43"),
            Err(CryptoError::Corrupt)
        ));
    }

    #[test]
    fn tampering_is_detected() {
        let ring = Keyring::ephemeral();
        let mut sealed = ring.encrypt(b"seed", b"ctx");
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert!(matches!(
            ring.decrypt(&sealed, b"ctx"),
            Err(CryptoError::Corrupt)
        ));
        assert!(matches!(
            ring.decrypt(&[], b"ctx"),
            Err(CryptoError::Corrupt)
        ));
    }

    #[test]
    fn rotation_keeps_old_ciphertext_readable() {
        let old = key_b64();
        let new = key_b64();
        let before = Keyring::parse(&format!("old:{old}")).expect("keyring");
        let sealed = before.encrypt(b"seed", b"ctx");
        let after = Keyring::parse(&format!("new:{new},old:{old}")).expect("keyring");
        assert_eq!(
            after
                .decrypt(&sealed, b"ctx")
                .expect("old key still decrypts"),
            b"seed"
        );
        let resealed = after.encrypt(b"seed", b"ctx");
        assert!(
            matches!(before.decrypt(&resealed, b"ctx"), Err(CryptoError::UnknownKey(id)) if id == "new")
        );
    }

    #[test]
    fn bad_keyrings_are_rejected() {
        for bad in [
            "",
            "k1",
            "k1:notbase64!!",
            "k1:AAAA",
            &format!("k1:{0},k1:{0}", key_b64()),
            &format!("bad id:{}", key_b64()),
        ] {
            assert!(Keyring::parse(bad).is_err(), "{bad}");
        }
    }
}
