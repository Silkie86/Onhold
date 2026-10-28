//! Gmail through the Gmail API. The first fetch lists recent messages and
//! notes the mailbox's history id; later fetches read the history since
//! then, so nothing is listed twice and nothing between fetches is missed.
//! Messages are read whole (`format=raw`) and sent whole.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use data_encoding::{BASE64URL, BASE64URL_NOPAD};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::mailbox::{Batch, FetchedMail, MailProvider, OutgoingMail, SentMail, build};
use crate::provider::{self, ProviderError, endpoint, segment};

pub const API_BASE: &str = "https://gmail.googleapis.com";

/// Labels whose messages stay out of the inbox.
const SKIPPED_LABELS: &[&str] = &[
    "SPAM",
    "TRASH",
    "DRAFT",
    "CATEGORY_PROMOTIONS",
    "CATEGORY_SOCIAL",
    "CATEGORY_FORUMS",
];
/// The search for the first fetch, before the date is added.
const FIRST_QUERY: &str =
    "-in:spam -in:trash -in:drafts -category:promotions -category:social -category:forums";

pub struct Gmail {
    http: reqwest::Client,
    base: Url,
    token: String,
}

impl Gmail {
    pub fn new(http: reqwest::Client, base: Url, access_token: String) -> Self {
        Self {
            http,
            base,
            token: access_token,
        }
    }

    fn url(&self, path: &str) -> Url {
        endpoint(&self.base, &format!("/gmail/v1/users/me{path}"))
    }

    /// The mailbox's address, and where its history stands now.
    pub async fn profile(&self) -> Result<Profile, ProviderError> {
        provider::send_json(self.http.get(self.url("/profile")).bearer_auth(&self.token)).await
    }

    async fn message(&self, id: &str) -> Result<Option<RawMessage>, ProviderError> {
        let url = self.url(&format!("/messages/{}?format=raw", segment(id)));
        // A whole message can be large: allow it longer than a plain call.
        let req = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .timeout(provider::BODY_TIMEOUT);
        match provider::send_json(req).await {
            Ok(m) => Ok(Some(m)),
            // Deleted between being listed and being read.
            Err(ProviderError::Refused { status: 404, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// A message as a list, history or thread names it: read later, and only if
/// the inbox does not hold it yet.
fn listed(m: Listed) -> FetchedMail {
    FetchedMail {
        sent: m.label_ids.iter().any(|l| l == "SENT"),
        external_id: m.id,
        thread_id: m.thread_id,
        raw: None,
    }
}

fn decode(raw: &str) -> Result<Vec<u8>, ProviderError> {
    let trimmed = raw.trim_end_matches('=');
    BASE64URL_NOPAD
        .decode(trimmed.as_bytes())
        .map_err(|e| ProviderError::Invalid(format!("message body: {e}")))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub email_address: String,
    pub history_id: String,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    raw: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MessageList {
    #[serde(default)]
    messages: Vec<Listed>,
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryList {
    #[serde(default)]
    history: Vec<History>,
    next_page_token: Option<String>,
    history_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct History {
    #[serde(default)]
    messages_added: Vec<Added>,
    #[serde(default)]
    labels_added: Vec<Labelled>,
    #[serde(default)]
    labels_removed: Vec<Labelled>,
}

#[derive(Debug, Deserialize)]
struct Added {
    message: Listed,
}

/// A message as a list, history or thread names it, with its labels at the
/// time (a list gives none).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Listed {
    id: String,
    thread_id: Option<String>,
    #[serde(default)]
    label_ids: Vec<String>,
}

/// A change to a message's labels: which message, and which labels.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Labelled {
    message: Listed,
    #[serde(default)]
    label_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ThreadMessages {
    #[serde(default)]
    messages: Vec<Listed>,
}

fn skipped(labels: &[String]) -> bool {
    labels.iter().any(|l| SKIPPED_LABELS.contains(&l.as_str()))
}

/// Where the mailbox has been read to.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct Cursor {
    /// The history id everything up to is read.
    history: String,
    /// Part way through a list (the first fetch) or a history read.
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<String>,
    /// The first fetch's list, before history takes over.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    listing: bool,
    /// The search the first fetch is paging through.
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,
}

#[async_trait]
impl MailProvider for Gmail {
    async fn fetch(
        &self,
        cursor: Option<&str>,
        since: DateTime<Utc>,
        max: usize,
    ) -> Result<Batch, ProviderError> {
        let cursor: Option<Cursor> = match cursor {
            Some(c) => Some(serde_json::from_str(c).map_err(|_| ProviderError::CursorExpired)?),
            None => None,
        };
        let max = max.clamp(1, 100);
        let c = match cursor {
            Some(c) => c,
            None => {
                // Note where history stands before listing, so whatever
                // arrives meanwhile comes with the first history read.
                let profile = self.profile().await?;
                Cursor {
                    history: profile.history_id,
                    page: None,
                    listing: true,
                    query: Some(format!("{FIRST_QUERY} after:{}", since.timestamp())),
                }
            }
        };
        if c.listing {
            let mut path = format!("/messages?maxResults={max}");
            if let Some(q) = &c.query {
                path.push_str(&format!("&q={}", segment(q)));
            }
            if let Some(p) = &c.page {
                path.push_str(&format!("&pageToken={}", segment(p)));
            }
            let list: MessageList =
                provider::send_json(self.http.get(self.url(&path)).bearer_auth(&self.token))
                    .await?;
            // Gmail lists newest first; keep arrival order.
            let messages = list.messages.into_iter().rev().map(listed).collect();
            let more = list.next_page_token.is_some();
            let next = Cursor {
                history: c.history,
                listing: more,
                query: if more { c.query } else { None },
                page: list.next_page_token,
            };
            return Ok(Batch {
                messages,
                cursor: to_string(&next)?,
                more,
            });
        }

        // New messages, and ones moved into the inbox or out of spam or
        // promotions (a label change, not a new message).
        let mut path = format!(
            "/history?startHistoryId={}&historyTypes=messageAdded&historyTypes=labelAdded\
             &historyTypes=labelRemoved&maxResults={max}",
            segment(&c.history)
        );
        if let Some(p) = &c.page {
            path.push_str(&format!("&pageToken={}", segment(p)));
        }
        let list: HistoryList = match provider::send_json(
            self.http.get(self.url(&path)).bearer_auth(&self.token),
        )
        .await
        {
            // Gmail keeps about a week of history; older is a 404.
            Err(ProviderError::Refused { status: 404, .. }) => {
                return Err(ProviderError::CursorExpired);
            }
            other => other?,
        };
        let mut messages: Vec<FetchedMail> = Vec::new();
        let mut add = |m: Listed| {
            if !messages.iter().any(|f| f.external_id == m.id) {
                messages.push(listed(m));
            }
        };
        for h in list.history {
            for a in h.messages_added {
                // Spam, drafts and promotions are not even downloaded.
                if !skipped(&a.message.label_ids) {
                    add(a.message);
                }
            }
            for l in h.labels_added {
                if l.label_ids.iter().any(|x| x == "INBOX") && !skipped(&l.message.label_ids) {
                    add(l.message);
                }
            }
            for l in h.labels_removed {
                if skipped(&l.label_ids) && !skipped(&l.message.label_ids) {
                    add(l.message);
                }
            }
        }
        let more = list.next_page_token.is_some();
        let next = if more {
            Cursor {
                history: c.history,
                page: list.next_page_token,
                ..Cursor::default()
            }
        } else {
            Cursor {
                history: list.history_id.unwrap_or(c.history),
                ..Cursor::default()
            }
        };
        Ok(Batch {
            messages,
            cursor: to_string(&next)?,
            more,
        })
    }

    async fn raw(&self, external_id: &str) -> Result<Option<Vec<u8>>, ProviderError> {
        match self.message(external_id).await? {
            Some(m) => decode(&m.raw).map(Some),
            None => Ok(None),
        }
    }

    async fn conversation(&self, thread_id: &str) -> Result<Vec<FetchedMail>, ProviderError> {
        let url = self.url(&format!("/threads/{}?format=minimal", segment(thread_id)));
        let thread: ThreadMessages =
            match provider::send_json(self.http.get(url).bearer_auth(&self.token)).await {
                Ok(t) => t,
                Err(ProviderError::Refused { status: 404, .. }) => return Ok(Vec::new()),
                Err(e) => return Err(e),
            };
        Ok(thread
            .messages
            .into_iter()
            .filter(|m| !skipped(&m.label_ids))
            .map(listed)
            .collect())
    }

    async fn send(&self, mail: &OutgoingMail) -> Result<SentMail, ProviderError> {
        let raw = build(mail)?.formatted();
        let body = serde_json::json!({ "raw": BASE64URL.encode(&raw) });
        let post = |body: &serde_json::Value| {
            provider::send_json::<Sent>(
                self.http
                    .post(self.url("/messages/send"))
                    .bearer_auth(&self.token)
                    .timeout(provider::BODY_TIMEOUT)
                    .json(body),
            )
        };
        let sent = match &mail.thread_id {
            Some(t) => {
                let mut threaded = body.clone();
                threaded["threadId"] = serde_json::Value::String(t.clone());
                match post(&threaded).await {
                    // The conversation was deleted in Gmail: send it on its
                    // own; its headers still thread it for the recipient.
                    Err(ProviderError::Refused { status: 404, .. }) => post(&body).await?,
                    other => other?,
                }
            }
            None => post(&body).await?,
        };
        Ok(SentMail {
            external_id: Some(sent.id),
            thread_id: sent.thread_id,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Sent {
    id: String,
    thread_id: Option<String>,
}

fn to_string(c: &Cursor) -> Result<String, ProviderError> {
    serde_json::to_string(c).map_err(|e| ProviderError::Invalid(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_bodies_decode_with_or_without_padding() {
        let encoded = BASE64URL.encode(b"Subject: hi\r\n\r\n?>");
        assert_eq!(decode(&encoded).expect("padded"), b"Subject: hi\r\n\r\n?>");
        assert_eq!(
            decode(encoded.trim_end_matches('=')).expect("unpadded"),
            b"Subject: hi\r\n\r\n?>"
        );
        assert!(decode("not base64!").is_err());
    }

    #[test]
    fn cursors_round_trip() {
        let c = Cursor {
            history: "123".into(),
            ..Cursor::default()
        };
        let s = to_string(&c).expect("serialises");
        assert_eq!(s, r#"{"history":"123"}"#);
        assert_eq!(serde_json::from_str::<Cursor>(&s).expect("parses"), c);
    }
}
