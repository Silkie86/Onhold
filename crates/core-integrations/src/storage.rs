//! Where file bytes are kept when not in the database: a directory, or an
//! S3-compatible bucket (Amazon S3, Cloudflare R2, Backblaze B2, MinIO).
//!
//! The S3 client is small on purpose: three calls (put, get, delete),
//! path-style addresses and AWS Signature Version 4, which every
//! S3-compatible service accepts. Keys are `<workspace id>/<file id>`, so
//! nothing about a file's contents or name is in its address.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("the file is not in storage")]
    NotFound,
    /// Worth retrying: the service was down, slow or busy.
    #[error("storage is unavailable: {0}")]
    Unavailable(String),
    #[error("storage failed: {0}")]
    Failed(String),
}

/// Somewhere outside the database that holds files' bytes.
#[async_trait]
pub trait BlobStore: Send + Sync {
    /// `directory` or `s3`, as recorded on each file.
    fn kind(&self) -> &'static str;
    async fn put(&self, key: &str, bytes: &[u8], content_type: &str) -> Result<(), StorageError>;
    async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError>;
    /// Deleting something already gone succeeds.
    async fn delete(&self, key: &str) -> Result<(), StorageError>;
}

/// Keys are made by OnHold, but are checked anyway so none can leave the
/// store's root: hex, digits and dashes in `/`-separated parts.
fn check_key(key: &str) -> Result<(), StorageError> {
    let ok = !key.is_empty()
        && key.len() <= 200
        && key.split('/').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        });
    if ok {
        Ok(())
    } else {
        Err(StorageError::Failed(format!("`{key}` is not a storage key")))
    }
}

// Directory ------------------------------------------------------------------------

/// Files under a local directory (a mounted volume in production).
pub struct DirectoryStore {
    root: PathBuf,
}

impl DirectoryStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path(&self, key: &str) -> Result<PathBuf, StorageError> {
        check_key(key)?;
        Ok(key.split('/').fold(self.root.clone(), |p, part| p.join(part)))
    }
}

fn io_error(e: std::io::Error) -> StorageError {
    match e.kind() {
        std::io::ErrorKind::NotFound => StorageError::NotFound,
        _ => StorageError::Failed(e.to_string()),
    }
}

#[async_trait]
impl BlobStore for DirectoryStore {
    fn kind(&self) -> &'static str {
        "directory"
    }

    async fn put(&self, key: &str, bytes: &[u8], _content_type: &str) -> Result<(), StorageError> {
        let path = self.path(key)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(io_error)?;
        }
        // Written beside the target and renamed, so a reader never sees half a file.
        let partial = path.with_extension("partial");
        tokio::fs::write(&partial, bytes).await.map_err(io_error)?;
        tokio::fs::rename(&partial, &path).await.map_err(io_error)
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        tokio::fs::read(self.path(key)?).await.map_err(io_error)
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        match tokio::fs::remove_file(self.path(key)?).await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other.map_err(io_error),
        }
    }
}

// S3 -----------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct S3Config {
    /// `https://<account>.r2.cloudflarestorage.com`, `https://s3.eu-west-2.amazonaws.com`…
    pub endpoint: url::Url,
    pub bucket: String,
    /// `auto` for R2; the bucket's region for S3.
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

pub struct S3Store {
    config: S3Config,
    http: reqwest::Client,
}

impl S3Store {
    pub fn new(config: S3Config) -> Result<Self, StorageError> {
        if config.bucket.is_empty()
            || !config
                .bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        {
            return Err(StorageError::Failed(
                "S3_BUCKET is not a bucket name".into(),
            ));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| StorageError::Failed(e.to_string()))?;
        Ok(Self { config, http })
    }

    fn url(&self, key: &str) -> Result<url::Url, StorageError> {
        check_key(key)?;
        let mut url = self.config.endpoint.clone();
        let base = url.path().trim_end_matches('/').to_owned();
        url.set_path(&format!("{base}/{}/{key}", self.config.bucket));
        Ok(url)
    }

    async fn send(
        &self,
        method: reqwest::Method,
        key: &str,
        body: Option<(&[u8], &str)>,
    ) -> Result<reqwest::Response, StorageError> {
        let url = self.url(key)?;
        let payload = body.map_or(&[][..], |(b, _)| b);
        let signed = sign(
            &Request {
                method: method.as_str(),
                url: &url,
                headers: &[],
                payload_sha256: &hex(&Sha256::digest(payload)),
            },
            &self.config,
            Utc::now(),
        );
        let mut req = self.http.request(method, url);
        for (name, value) in &signed {
            req = req.header(name.as_str(), value.as_str());
        }
        if let Some((bytes, content_type)) = body {
            req = req
                .header("content-type", content_type)
                .body(bytes.to_vec());
        }
        req.send()
            .await
            .map_err(|e| StorageError::Unavailable(e.to_string()))
    }
}

async fn failure(res: reqwest::Response) -> StorageError {
    let status = res.status();
    let text: String = res
        .text()
        .await
        .unwrap_or_default()
        .chars()
        .take(300)
        .collect();
    if status.is_server_error() || status.as_u16() == 429 {
        StorageError::Unavailable(format!("{status}: {text}"))
    } else if status.as_u16() == 404 {
        StorageError::NotFound
    } else {
        StorageError::Failed(format!("{status}: {text}"))
    }
}

#[async_trait]
impl BlobStore for S3Store {
    fn kind(&self) -> &'static str {
        "s3"
    }

    async fn put(&self, key: &str, bytes: &[u8], content_type: &str) -> Result<(), StorageError> {
        let res = self
            .send(reqwest::Method::PUT, key, Some((bytes, content_type)))
            .await?;
        if res.status().is_success() {
            Ok(())
        } else {
            Err(failure(res).await)
        }
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        let res = self.send(reqwest::Method::GET, key, None).await?;
        if !res.status().is_success() {
            return Err(failure(res).await);
        }
        res.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| StorageError::Unavailable(e.to_string()))
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        let res = self.send(reqwest::Method::DELETE, key, None).await?;
        if res.status().is_success() || res.status().as_u16() == 404 {
            Ok(())
        } else {
            Err(failure(res).await)
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("any key length");
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// RFC 3986 encoding of a path, `/` kept, as SigV4 wants it.
fn uri_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

struct Request<'a> {
    method: &'a str,
    url: &'a url::Url,
    /// Extra headers to sign, lower-case names.
    headers: &'a [(&'a str, &'a str)],
    payload_sha256: &'a str,
}

/// The headers that sign a request with AWS Signature Version 4: `host`,
/// `x-amz-date`, `x-amz-content-sha256` and `authorization`.
fn sign(req: &Request<'_>, config: &S3Config, now: DateTime<Utc>) -> Vec<(String, String)> {
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();
    let host = match req.url.port() {
        Some(port) => format!("{}:{port}", req.url.host_str().unwrap_or_default()),
        None => req.url.host_str().unwrap_or_default().to_owned(),
    };
    let mut headers: Vec<(String, String)> = vec![
        ("host".into(), host),
        ("x-amz-content-sha256".into(), req.payload_sha256.to_owned()),
        ("x-amz-date".into(), amz_date.clone()),
    ];
    headers.extend(
        req.headers
            .iter()
            .map(|(n, v)| (n.to_ascii_lowercase(), v.trim().to_owned())),
    );
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(n, v)| format!("{n}:{v}\n")).collect();
    let signed_headers = headers
        .iter()
        .map(|(n, _)| n.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical = format!(
        "{}\n{}\n{}\n{canonical_headers}\n{signed_headers}\n{}",
        req.method,
        uri_encode_path(req.url.path()),
        req.url.query().unwrap_or(""),
        req.payload_sha256
    );
    let scope = format!("{date}/{}/s3/aws4_request", config.region);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex(&Sha256::digest(canonical.as_bytes()))
    );
    let key = [&date, config.region.as_str(), "s3", "aws4_request"]
        .iter()
        .fold(
            format!("AWS4{}", config.secret_access_key).into_bytes(),
            |k, part| hmac(&k, part),
        );
    let signature = hex(&hmac(&key, &to_sign));
    let mut out: Vec<(String, String)> = headers
        .into_iter()
        .filter(|(n, _)| n != "host")
        .collect();
    out.push((
        "authorization".into(),
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            config.access_key_id
        ),
    ));
    out
}

/// Whether a directory can be written to, for the startup check.
pub fn writable(dir: &Path) -> bool {
    std::fs::create_dir_all(dir).is_ok() && {
        let probe = dir.join(format!(".onhold-probe-{}", uuid::Uuid::now_v7()));
        let ok = std::fs::write(&probe, b"").is_ok();
        let _ = std::fs::remove_file(&probe);
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_like_the_aws_example() {
        // The "GET Object" example in AWS's Signature Version 4 documentation.
        let config = S3Config {
            endpoint: "https://examplebucket.s3.amazonaws.com".parse().expect("url"),
            bucket: "examplebucket".into(),
            region: "us-east-1".into(),
            access_key_id: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
        };
        let url: url::Url = "https://examplebucket.s3.amazonaws.com/test.txt"
            .parse()
            .expect("url");
        let empty = hex(&Sha256::digest(b""));
        let now = DateTime::parse_from_rfc3339("2013-05-24T00:00:00Z")
            .expect("date")
            .with_timezone(&Utc);
        let headers = sign(
            &Request {
                method: "GET",
                url: &url,
                headers: &[("range", "bytes=0-9")],
                payload_sha256: &empty,
            },
            &config,
            now,
        );
        let auth = headers
            .iter()
            .find(|(n, _)| n == "authorization")
            .map(|(_, v)| v.as_str())
            .expect("signed");
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn keys_cannot_leave_the_root() {
        for bad in ["", "../etc/passwd", "a//b", "a/../b", "a b", "/abs"] {
            assert!(check_key(bad).is_err(), "{bad}");
        }
        assert!(check_key("0192f0c4-aaaa/0192f0c4-bbbb").is_ok());
        let store = S3Store::new(S3Config {
            endpoint: "https://r2.example.test/base/".parse().expect("url"),
            bucket: "onhold-files".into(),
            region: "auto".into(),
            access_key_id: "id".into(),
            secret_access_key: "secret".into(),
        })
        .expect("store");
        assert_eq!(
            store.url("ws/file").expect("url").as_str(),
            "https://r2.example.test/base/onhold-files/ws/file"
        );
    }

    #[tokio::test]
    async fn directories_store_and_forget() {
        let root = std::env::temp_dir().join(format!("onhold-store-{}", uuid::Uuid::now_v7()));
        let store = DirectoryStore::new(&root);
        store
            .put("ws/one", b"hello", "text/plain")
            .await
            .expect("put");
        assert_eq!(store.get("ws/one").await.expect("get"), b"hello");
        store.delete("ws/one").await.expect("delete");
        assert!(matches!(
            store.get("ws/one").await,
            Err(StorageError::NotFound)
        ));
        store.delete("ws/one").await.expect("deleting twice is fine");
        assert!(writable(&root));
        let _ = std::fs::remove_dir_all(&root);
    }
}
