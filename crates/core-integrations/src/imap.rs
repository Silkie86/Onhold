//! Any mailbox on its own domain: read over IMAP, sent over SMTP, with the
//! person's username and password (often an app password).
//!
//! The servers are chosen by a tenant, so, as with webhooks, a host must
//! resolve to public addresses and the connection goes to exactly the
//! address that was checked (TLS is still verified against the host name).
//! Only the usual mail ports are allowed.

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::TryStreamExt;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::http::is_public;
use crate::mailbox::{Batch, FetchedMail, MailProvider, OutgoingMail, SentMail, build};
use crate::provider::ProviderError;

pub const IMAP_PORTS: &[u16] = &[143, 993];
pub const SMTP_PORTS: &[u16] = &[25, 465, 587, 2525];
const TIMEOUT: Duration = Duration::from_secs(60);
/// A fetch starts reading no more messages after this long, so it ends
/// (with what it has) well inside [`FETCH_TIMEOUT`].
const READ_BUDGET: Duration = Duration::from_secs(20);
/// The most one fetch may take, under the worker's two-minute job limit.
const FETCH_TIMEOUT: Duration = Duration::from_secs(100);

/// How a connection is protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Security {
    /// TLS from the first byte (IMAP 993, SMTP 465).
    Tls,
    /// Plain, then upgraded with STARTTLS before anything is sent (143, 587).
    Starttls,
    /// No encryption: tests and local development only.
    None,
}

/// One server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub security: Security,
}

/// A mailbox's servers and sign-in. The password is kept apart, encrypted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub imap: Server,
    pub smtp: Server,
    pub username: String,
}

pub struct Imap {
    settings: Settings,
    password: String,
    /// Let servers be on private addresses and unencrypted. Development
    /// and tests only, like webhooks' setting of the same name.
    allow_private: bool,
}

impl Imap {
    pub fn new(settings: Settings, password: String, allow_private: bool) -> Self {
        Self {
            settings,
            password,
            allow_private,
        }
    }

    /// Checks settings before they are saved.
    pub fn check_settings(settings: &Settings, allow_private: bool) -> Result<(), String> {
        for (what, s, ports) in [
            ("IMAP", &settings.imap, IMAP_PORTS),
            ("SMTP", &settings.smtp, SMTP_PORTS),
        ] {
            let host = s.host.trim();
            if host.is_empty()
                || host.len() > 253
                || !host
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
            {
                return Err(format!("the {what} server is not a host name"));
            }
            if !allow_private {
                if !ports.contains(&s.port) {
                    return Err(format!(
                        "the {what} port must be one of {}",
                        ports
                            .iter()
                            .map(u16::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                if s.security == Security::None {
                    return Err(format!("the {what} connection must be encrypted"));
                }
            }
        }
        if settings.username.trim().is_empty() || settings.username.len() > 320 {
            return Err("the username is missing".into());
        }
        Ok(())
    }

    /// Signs in to the IMAP server and greets the SMTP one, so a mistake
    /// shows when the mailbox is added rather than at the first poll.
    pub async fn verify(&self) -> Result<(), ProviderError> {
        let mut session = self.session().await?;
        session.select("INBOX").await.map_err(imap_error)?;
        let _ = session.logout().await;
        let transport = self.smtp().await?;
        match transport.test_connection().await {
            Ok(true) => Ok(()),
            Ok(false) => Err(ProviderError::Unavailable(
                "the SMTP server did not answer".into(),
            )),
            Err(e) => Err(smtp_error(&e)),
        }
    }

    async fn address(&self, server: &Server) -> Result<SocketAddr, ProviderError> {
        let host = server
            .host
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']');
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, server.port))
            .await
            .map_err(|e| ProviderError::Unavailable(format!("{host}: {e}")))?
            .collect();
        addrs
            .into_iter()
            .find(|a| self.allow_private || is_public(a.ip()))
            .ok_or_else(|| ProviderError::Refused {
                status: 0,
                message: format!("{host} does not resolve to a public address"),
            })
    }

    async fn session(&self) -> Result<async_imap::Session<Conn>, ProviderError> {
        let server = &self.settings.imap;
        let addr = self.address(server).await?;
        let tcp = tokio::time::timeout(TIMEOUT, TcpStream::connect(addr))
            .await
            .map_err(|_| ProviderError::Unavailable("connecting timed out".into()))?
            .map_err(|e| ProviderError::Unavailable(e.to_string()))?;
        let client = match server.security {
            Security::Tls => {
                let mut c =
                    async_imap::Client::new(Conn::Tls(Box::new(tls(&server.host, tcp).await?)));
                greeting(&mut c).await?;
                c
            }
            Security::None if self.allow_private => {
                let mut c = async_imap::Client::new(Conn::Plain(tcp));
                greeting(&mut c).await?;
                c
            }
            Security::None => {
                return Err(ProviderError::Refused {
                    status: 0,
                    message: "the connection must be encrypted".into(),
                });
            }
            Security::Starttls => {
                let mut c = async_imap::Client::new(Conn::Plain(tcp));
                greeting(&mut c).await?;
                c.run_command_and_check_ok("STARTTLS", None)
                    .await
                    .map_err(imap_error)?;
                let Conn::Plain(tcp) = c.into_inner() else {
                    return Err(ProviderError::Invalid("not a plain connection".into()));
                };
                // No greeting after STARTTLS.
                async_imap::Client::new(Conn::Tls(Box::new(tls(&server.host, tcp).await?)))
            }
        };
        client
            .login(&self.settings.username, &self.password)
            .await
            .map_err(|(e, _)| match e {
                async_imap::error::Error::No(text) => login_refused(&text),
                other => imap_error(other),
            })
    }

    async fn smtp(&self) -> Result<AsyncSmtpTransport<Tokio1Executor>, ProviderError> {
        let server = &self.settings.smtp;
        let addr = self.address(server).await?;
        let host = server.host.trim().to_owned();
        let tls =
            || TlsParameters::new(host.clone()).map_err(|e| ProviderError::Invalid(e.to_string()));
        let security = match server.security {
            Security::Tls => Tls::Wrapper(tls()?),
            Security::Starttls => Tls::Required(tls()?),
            Security::None if self.allow_private => Tls::None,
            Security::None => {
                return Err(ProviderError::Refused {
                    status: 0,
                    message: "the connection must be encrypted".into(),
                });
            }
        };
        // Connect to the address checked above, not to whatever the name
        // resolves to by then.
        Ok(
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(addr.ip().to_string())
                .port(addr.port())
                .tls(security)
                .credentials(Credentials::new(
                    self.settings.username.clone(),
                    self.password.clone(),
                ))
                .timeout(Some(TIMEOUT))
                .build(),
        )
    }
}

/// A refused login: the password is wrong (connect again), or the server
/// says it cannot sign anyone in just now (try again later, RFC 5530).
fn login_refused(text: &str) -> ProviderError {
    let lower = text.to_ascii_lowercase();
    let temporary = [
        "[unavailable]",
        "[limit]",
        "[inuse]",
        "[serverbug]",
        "temporar",
        "try again",
    ]
    .iter()
    .any(|m| lower.contains(m));
    if temporary {
        ProviderError::Unavailable(format!(
            "the mail server could not sign in just now: {text}"
        ))
    } else {
        ProviderError::Revoked("the mail server refused the username or password".into())
    }
}

async fn greeting(c: &mut async_imap::Client<Conn>) -> Result<(), ProviderError> {
    match tokio::time::timeout(TIMEOUT, c.read_response()).await {
        Ok(Ok(Some(_))) => Ok(()),
        Ok(Ok(None)) => Err(ProviderError::Unavailable(
            "the server closed the connection".into(),
        )),
        Ok(Err(e)) => Err(ProviderError::Unavailable(e.to_string())),
        Err(_) => Err(ProviderError::Unavailable(
            "the server did not greet us".into(),
        )),
    }
}

async fn tls(host: &str, tcp: TcpStream) -> Result<TlsStream<TcpStream>, ProviderError> {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| ProviderError::Invalid(e.to_string()))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(
        host.trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned(),
    )
    .map_err(|e| ProviderError::Invalid(e.to_string()))?;
    tokio::time::timeout(
        TIMEOUT,
        TlsConnector::from(Arc::new(config)).connect(name, tcp),
    )
    .await
    .map_err(|_| ProviderError::Unavailable("TLS timed out".into()))?
    .map_err(|e| ProviderError::Unavailable(format!("TLS: {e}")))
}

fn imap_error(e: async_imap::error::Error) -> ProviderError {
    use async_imap::error::Error as E;
    match e {
        E::Io(e) => ProviderError::Unavailable(e.to_string()),
        E::ConnectionLost => ProviderError::Unavailable("connection lost".into()),
        E::No(m) | E::Bad(m) => ProviderError::Refused {
            status: 0,
            message: m.chars().take(300).collect(),
        },
        other => ProviderError::Invalid(other.to_string()),
    }
}

fn smtp_error(e: &lettre::transport::smtp::Error) -> ProviderError {
    let message: String = e.to_string().chars().take(300).collect();
    if e.is_permanent() {
        // 535: authentication failed.
        if message.contains("535") {
            return ProviderError::Revoked(
                "the mail server refused the username or password".into(),
            );
        }
        ProviderError::Refused { status: 0, message }
    } else {
        ProviderError::Unavailable(message)
    }
}

/// Where INBOX has been read to. A new `uidvalidity` means the server
/// renumbered the folder, so reading starts again from a date.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Cursor {
    uidvalidity: u32,
    last_uid: u32,
}

#[async_trait]
impl MailProvider for Imap {
    async fn fetch(
        &self,
        cursor: Option<&str>,
        since: DateTime<Utc>,
        max: usize,
    ) -> Result<Batch, ProviderError> {
        let cursor: Option<Cursor> = cursor.and_then(|c| serde_json::from_str(c).ok());
        let work = async {
            let mut session = self.session().await?;
            let mailbox = session.select("INBOX").await.map_err(imap_error)?;
            let validity = mailbox.uid_validity.unwrap_or(0);
            let after = match &cursor {
                Some(c) if c.uidvalidity == validity => Some(c.last_uid),
                _ => None,
            };
            let query = match after {
                Some(last) => format!("UID {}:*", last.saturating_add(1)),
                None => format!("SINCE {}", since.format("%d-%b-%Y")),
            };
            let mut uids: Vec<u32> = session
                .uid_search(&query)
                .await
                .map_err(imap_error)?
                .into_iter()
                // `n:*` always matches the highest UID, even below n.
                .filter(|u| after.is_none_or(|last| *u > last))
                .collect();
            uids.sort_unstable();
            let mut more = uids.len() > max;
            uids.truncate(max.max(1));
            // One message at a time, each with its own time: a slow server
            // or a large message still leaves the ones before it read, and
            // the next fetch carries on from there.
            let mut messages = Vec::with_capacity(uids.len());
            let mut read_to = after;
            let started = tokio::time::Instant::now();
            for uid in &uids {
                if started.elapsed() > READ_BUDGET && !messages.is_empty() {
                    more = true;
                    break;
                }
                let fetched = tokio::time::timeout(crate::provider::BODY_TIMEOUT, async {
                    session
                        .uid_fetch(uid.to_string(), "(UID BODY.PEEK[])")
                        .await
                        .map_err(imap_error)?
                        .try_collect::<Vec<async_imap::types::Fetch>>()
                        .await
                        .map_err(imap_error)
                })
                .await;
                let fetches = match fetched {
                    Ok(f) => f?,
                    Err(_) if !messages.is_empty() => {
                        more = true;
                        break;
                    }
                    Err(_) => {
                        return Err(ProviderError::Unavailable(
                            "the mail server was too slow".into(),
                        ));
                    }
                };
                if let Some(body) = fetches
                    .iter()
                    .find(|f| f.uid == Some(*uid))
                    .and_then(|f| f.body())
                {
                    messages.push(FetchedMail {
                        external_id: format!("{validity}:{uid}"),
                        thread_id: None,
                        raw: Some(body.to_vec()),
                        sent: false,
                    });
                }
                read_to = Some(*uid);
            }
            let _ = session.logout().await;
            let last_uid = read_to.unwrap_or(0);
            let next = Cursor {
                uidvalidity: validity,
                last_uid,
            };
            Ok::<_, ProviderError>(Batch {
                messages,
                cursor: serde_json::to_string(&next)
                    .map_err(|e| ProviderError::Invalid(e.to_string()))?,
                more,
            })
        };
        tokio::time::timeout(FETCH_TIMEOUT, work)
            .await
            .map_err(|_| ProviderError::Unavailable("the mail server was too slow".into()))?
    }

    async fn raw(&self, external_id: &str) -> Result<Option<Vec<u8>>, ProviderError> {
        let Some((validity, uid)) = external_id
            .split_once(':')
            .and_then(|(v, u)| Some((v.parse::<u32>().ok()?, u.parse::<u32>().ok()?)))
        else {
            return Ok(None);
        };
        let work = async {
            let mut session = self.session().await?;
            let mailbox = session.select("INBOX").await.map_err(imap_error)?;
            // Renumbered since: the UID may now be another message.
            if mailbox.uid_validity.unwrap_or(0) != validity {
                let _ = session.logout().await;
                return Ok(None);
            }
            let fetches: Vec<async_imap::types::Fetch> = session
                .uid_fetch(uid.to_string(), "(UID BODY.PEEK[])")
                .await
                .map_err(imap_error)?
                .try_collect()
                .await
                .map_err(imap_error)?;
            let _ = session.logout().await;
            Ok::<_, ProviderError>(
                fetches
                    .iter()
                    .find(|f| f.uid == Some(uid))
                    .and_then(|f| f.body())
                    .map(<[u8]>::to_vec),
            )
        };
        tokio::time::timeout(TIMEOUT * 3, work)
            .await
            .map_err(|_| ProviderError::Unavailable("the mail server was too slow".into()))?
    }

    async fn send(&self, mail: &OutgoingMail) -> Result<SentMail, ProviderError> {
        let message = build(mail)?;
        let transport = self.smtp().await?;
        transport.send(message).await.map_err(|e| smtp_error(&e))?;
        Ok(SentMail::default())
    }
}

/// A connection, encrypted or (in tests) not.
#[derive(Debug)]
enum Conn {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl AsyncRead for Conn {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Conn {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_flush(cx),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            imap: Server {
                host: "imap.agency.test".into(),
                port: 993,
                security: Security::Tls,
            },
            smtp: Server {
                host: "smtp.agency.test".into(),
                port: 587,
                security: Security::Starttls,
            },
            username: "ana@agency.test".into(),
        }
    }

    #[test]
    fn settings_are_checked() {
        assert!(Imap::check_settings(&settings(), false).is_ok());
        let mut s = settings();
        s.imap.port = 6379;
        assert!(Imap::check_settings(&s, false).is_err());
        assert!(
            Imap::check_settings(&s, true).is_ok(),
            "any port in development"
        );
        let mut s = settings();
        s.smtp.security = Security::None;
        assert!(Imap::check_settings(&s, false).is_err());
        let mut s = settings();
        s.imap.host = "imap.test/../x".into();
        assert!(Imap::check_settings(&s, false).is_err());
        let mut s = settings();
        s.username = " ".into();
        assert!(Imap::check_settings(&s, false).is_err());
    }

    #[tokio::test]
    async fn private_servers_are_refused() {
        let mut s = settings();
        s.imap.host = "127.0.0.1".into();
        let imap = Imap::new(s, "pw".into(), false);
        assert!(matches!(
            imap.address(&imap.settings.imap).await,
            Err(ProviderError::Refused { .. })
        ));
    }
}
