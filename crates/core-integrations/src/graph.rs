//! Outlook and Microsoft 365 mail through Microsoft Graph. Each fetch
//! follows Graph's delta links for the Inbox and Sent Items, so only
//! changes since the last fetch come back; messages are read whole (MIME)
//! and sent whole. A shared mailbox is read and sent from as itself.

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use data_encoding::BASE64;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::mailbox::{Batch, FetchedMail, MailProvider, OutgoingMail, SentMail, build};
use crate::provider::{self, ProviderError, endpoint, segment};

pub const API_BASE: &str = "https://graph.microsoft.com";

/// The folders read, by their well-known names.
const FOLDERS: [&str; 2] = ["inbox", "sentitems"];

/// Attachments a message sent through Graph may carry in all. Graph takes
/// at most 4 MB in one request and the message goes base64-encoded twice;
/// larger files would need an upload session.
pub const MAX_SEND_ATTACHMENTS: usize = 2 * 1024 * 1024;

/// Ids that stay the same when a message moves to another folder, so a
/// message can be read again (to open an attachment) after it is filed.
const IMMUTABLE_IDS: &str = "IdType=\"ImmutableId\"";

pub struct Graph {
    http: reqwest::Client,
    base: Url,
    token: String,
    /// `me`, or `users/<address>` for a shared mailbox.
    user: String,
}

impl Graph {
    /// `shared` names a shared mailbox to use instead of the person's own.
    pub fn new(
        http: reqwest::Client,
        base: Url,
        access_token: String,
        shared: Option<&str>,
    ) -> Self {
        Self {
            http,
            base,
            token: access_token,
            user: match shared {
                Some(m) => format!("users/{}", segment(m)),
                None => "me".into(),
            },
        }
    }

    fn url(&self, path: &str) -> Url {
        endpoint(&self.base, &format!("/v1.0/{}{path}", self.user))
    }

    /// The mailbox's address.
    pub async fn address(&self) -> Result<String, ProviderError> {
        let me: Me = provider::send_json(
            self.http
                .get(self.url("?$select=mail,userPrincipalName"))
                .bearer_auth(&self.token),
        )
        .await?;
        me.mail
            .or(me.user_principal_name)
            .filter(|a| a.contains('@'))
            .ok_or_else(|| ProviderError::Invalid("the account has no mail address".into()))
    }

    /// Whether the mailbox can be read with this grant (a shared mailbox
    /// the person was given access to).
    pub async fn check_access(&self) -> Result<(), ProviderError> {
        provider::send_raw(
            self.http
                .get(self.url("/mailFolders/inbox?$select=id"))
                .bearer_auth(&self.token),
        )
        .await
        .map(|_| ())
    }

    /// A link Graph gave us must stay on Graph: it carries our token.
    fn own_link(&self, link: &str) -> Result<Url, ProviderError> {
        let url = Url::parse(link).map_err(|e| ProviderError::Invalid(e.to_string()))?;
        if url.origin() != self.base.origin() {
            return Err(ProviderError::Invalid("a link to another site".into()));
        }
        Ok(url)
    }

    async fn mime(&self, id: &str) -> Result<Option<Vec<u8>>, ProviderError> {
        let url = self.url(&format!("/messages/{}/$value", segment(id)));
        let req = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .header("prefer", IMMUTABLE_IDS)
            .timeout(provider::BODY_TIMEOUT);
        match provider::send_raw(req).await {
            Ok((_, body)) => Ok(Some(body)),
            Err(ProviderError::Refused { status: 404, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Me {
    mail: Option<String>,
    user_principal_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeltaPage {
    #[serde(default)]
    value: Vec<DeltaItem>,
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
    #[serde(rename = "@odata.deltaLink")]
    delta_link: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeltaItem {
    id: String,
    conversation_id: Option<String>,
    #[serde(rename = "@removed")]
    removed: Option<serde_json::Value>,
    is_draft: Option<bool>,
}

/// Where each folder has been read to: a next link part way through, a
/// delta link once caught up.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct Cursor {
    #[serde(skip_serializing_if = "Option::is_none")]
    inbox: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sentitems: Option<String>,
}

#[async_trait]
impl MailProvider for Graph {
    async fn fetch(
        &self,
        cursor: Option<&str>,
        since: DateTime<Utc>,
        max: usize,
    ) -> Result<Batch, ProviderError> {
        let mut c: Cursor = match cursor {
            Some(c) => serde_json::from_str(c).map_err(|_| ProviderError::CursorExpired)?,
            None => Cursor::default(),
        };
        let max = max.clamp(1, 100);
        let mut messages = Vec::new();
        let mut more = false;
        for folder in FOLDERS {
            if messages.len() >= max {
                more = true;
                break;
            }
            let link = match folder {
                "inbox" => c.inbox.clone(),
                _ => c.sentitems.clone(),
            };
            let url = match link {
                Some(l) => self.own_link(&l)?,
                None => {
                    let filter = format!(
                        "receivedDateTime ge {}",
                        since.to_rfc3339_opts(SecondsFormat::Secs, true)
                    );
                    self.url(&format!(
                        "/mailFolders/{folder}/messages/delta?$select=id,conversationId,isDraft&$filter={}",
                        segment(&filter)
                    ))
                }
            };
            let page: DeltaPage =
                provider::send_json(self.http.get(url).bearer_auth(&self.token).header(
                    "prefer",
                    format!("odata.maxpagesize={max}, {IMMUTABLE_IDS}"),
                ))
                .await?;
            for item in page.value {
                if item.removed.is_some() || item.is_draft == Some(true) {
                    continue;
                }
                // A change to a message already read (read state, flags)
                // lists it again: its body is read only if it is new.
                messages.push(FetchedMail {
                    external_id: item.id,
                    thread_id: item.conversation_id,
                    raw: None,
                    sent: folder == "sentitems",
                });
            }
            let next = match (page.next_link, page.delta_link) {
                (Some(n), _) => {
                    more = true;
                    Some(n)
                }
                (None, Some(d)) => Some(d),
                (None, None) => {
                    return Err(ProviderError::Invalid("a delta page with no link".into()));
                }
            };
            match folder {
                "inbox" => c.inbox = next,
                _ => c.sentitems = next,
            }
        }
        Ok(Batch {
            messages,
            cursor: serde_json::to_string(&c).map_err(|e| ProviderError::Invalid(e.to_string()))?,
            more,
        })
    }

    async fn raw(&self, external_id: &str) -> Result<Option<Vec<u8>>, ProviderError> {
        self.mime(external_id).await
    }

    async fn send(&self, mail: &OutgoingMail) -> Result<SentMail, ProviderError> {
        // Graph takes a whole MIME message, base64-encoded, as text/plain.
        let raw = build(mail)?.formatted();
        provider::send_raw(
            self.http
                .post(self.url("/sendMail"))
                .bearer_auth(&self.token)
                .timeout(provider::BODY_TIMEOUT)
                .header("content-type", "text/plain")
                .body(BASE64.encode(&raw)),
        )
        .await?;
        // Graph answers 202 with nothing: the copy in Sent Items arrives
        // with the next fetch, matched by its Message-ID.
        Ok(SentMail::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_must_stay_on_graph() {
        let g = Graph::new(
            reqwest::Client::new(),
            Url::parse(API_BASE).expect("static URL"),
            "t".into(),
            None,
        );
        assert!(
            g.own_link(
                "https://graph.microsoft.com/v1.0/me/mailFolders/inbox/messages/delta?$deltatoken=x"
            )
            .is_ok()
        );
        assert!(g.own_link("https://evil.test/steal").is_err());
        assert_eq!(
            g.url("/sendMail").as_str(),
            "https://graph.microsoft.com/v1.0/me/sendMail"
        );
        let shared = Graph::new(
            reqwest::Client::new(),
            Url::parse(API_BASE).expect("static URL"),
            "t".into(),
            Some("bookings@agency.test"),
        );
        assert_eq!(
            shared.url("/sendMail").as_str(),
            "https://graph.microsoft.com/v1.0/users/bookings%40agency.test/sendMail"
        );
    }
}
