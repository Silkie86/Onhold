//! People's own mailboxes: reading what arrives and sending replies as the
//! person. Each provider (Gmail, Outlook through Microsoft Graph, any IMAP
//! and SMTP server) implements [`MailProvider`]; the inbox module only ever
//! sees whole RFC 5322 messages, parsed here into [`ParsedMail`].

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use lettre::message::header::{ContentType, HeaderName, HeaderValue};
use lettre::message::{Attachment as Part, Mailbox, MultiPart, SinglePart};
use mail_parser::MessageParser;

use crate::mail::Attachment;
use crate::provider::ProviderError;

/// The most messages one fetch returns; the rest come on the next call.
pub const FETCH_BATCH: usize = 25;

/// One message as the provider holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedMail {
    /// The provider's id for it (Gmail id, Graph id, IMAP UID).
    pub external_id: String,
    /// The provider's conversation, when it has one.
    pub thread_id: Option<String>,
    /// The whole message, headers and MIME body, when the provider sends it
    /// with the listing (IMAP). Otherwise it is read with
    /// [`MailProvider::raw`], and only if the inbox does not hold it yet.
    pub raw: Option<Vec<u8>>,
    /// Sent from this mailbox (Gmail's SENT label, Outlook's Sent Items).
    pub sent: bool,
}

/// What one fetch found, and where the next one starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub messages: Vec<FetchedMail>,
    /// Opaque to the caller; store it and pass it back.
    pub cursor: String,
    /// More is waiting: fetch again straight away.
    pub more: bool,
}

/// A message to send from a person's mailbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingMail {
    pub from: String,
    pub from_name: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub text: String,
    pub html: Option<String>,
    /// With angle brackets, e.g. `<id@host>`.
    pub message_id: String,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    /// The provider's conversation to add it to, when replying.
    pub thread_id: Option<String>,
    pub attachments: Vec<Attachment>,
}

/// What the provider said about a sent message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SentMail {
    pub external_id: Option<String>,
    pub thread_id: Option<String>,
}

#[async_trait]
pub trait MailProvider: Send + Sync {
    /// Messages since `cursor`, or (with no cursor) since `since`.
    async fn fetch(
        &self,
        cursor: Option<&str>,
        since: DateTime<Utc>,
        max: usize,
    ) -> Result<Batch, ProviderError>;

    async fn send(&self, mail: &OutgoingMail) -> Result<SentMail, ProviderError>;

    /// One message, whole: one listed without its body, or read again to
    /// open an attachment OnHold left in the mailbox. `None` when it is no
    /// longer there.
    async fn raw(&self, external_id: &str) -> Result<Option<Vec<u8>>, ProviderError>;

    /// The other messages of a provider conversation, for when the inbox
    /// starts a thread part way through one (a first read goes newest
    /// first, so an answer can come before what it answers).
    async fn conversation(&self, _thread_id: &str) -> Result<Vec<FetchedMail>, ProviderError> {
        Ok(Vec::new())
    }
}

/// Builds the RFC 5322 message for `mail`.
pub fn build(mail: &OutgoingMail) -> Result<lettre::Message, ProviderError> {
    let bad = |what: &str, e: &dyn std::fmt::Display| ProviderError::Refused {
        status: 0,
        message: format!("{what}: {e}"),
    };
    let mut from: Mailbox = mail.from.parse().map_err(|e| bad("sender", &e))?;
    from.name = mail.from_name.clone();
    let mut builder = lettre::Message::builder()
        .from(from)
        .subject(mail.subject.clone())
        .message_id(Some(mail.message_id.clone()));
    if mail.to.is_empty() {
        return Err(bad("recipient", &"there is none"));
    }
    for to in &mail.to {
        builder = builder.to(to.parse().map_err(|e| bad("recipient", &e))?);
    }
    for cc in &mail.cc {
        builder = builder.cc(cc.parse().map_err(|e| bad("copy", &e))?);
    }
    if let Some(parent) = &mail.in_reply_to {
        builder = builder.in_reply_to(parent.clone());
    }
    if !mail.references.is_empty() {
        builder = builder.references(mail.references.join(" "));
    }
    builder = builder.header(AutoSubmitted::no());
    let text = SinglePart::builder()
        .header(ContentType::TEXT_PLAIN)
        .body(mail.text.clone());
    let body = match &mail.html {
        Some(html) => MultiPart::alternative().singlepart(text).singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_HTML)
                .body(html.clone()),
        ),
        None => MultiPart::mixed().singlepart(text),
    };
    let mut mixed = MultiPart::mixed().multipart(body);
    for a in &mail.attachments {
        let ct = ContentType::parse(&a.content_type).map_err(|e| bad("attachment", &e))?;
        mixed = mixed.singlepart(Part::new(a.filename.clone()).body(a.bytes.clone(), ct));
    }
    builder.multipart(mixed).map_err(|e| bad("message", &e))
}

/// `Auto-Submitted: no`: a person wrote this, so auto-responders may answer.
#[derive(Clone)]
struct AutoSubmitted(String);

impl AutoSubmitted {
    fn no() -> Self {
        Self("no".into())
    }
}

impl lettre::message::header::Header for AutoSubmitted {
    fn name() -> HeaderName {
        HeaderName::new_from_ascii_str("Auto-Submitted")
    }

    fn parse(s: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(s.to_owned()))
    }

    fn display(&self) -> HeaderValue {
        HeaderValue::new(Self::name(), self.0.clone())
    }
}

/// A sender or recipient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    /// Lower-cased.
    pub email: String,
    pub name: Option<String>,
}

/// A file that came with a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAttachment {
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
    /// Shown inside the HTML (`cid:`), rather than listed.
    pub inline: bool,
}

/// What OnHold keeps of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMail {
    /// Without angle brackets.
    pub message_id: Option<String>,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
    pub from: Option<Address>,
    pub reply_to: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: Option<String>,
    pub date: Option<DateTime<Utc>>,
    pub text: Option<String>,
    /// As sent; it must be sanitised before anyone sees it.
    pub html: Option<String>,
    pub attachments: Vec<ParsedAttachment>,
    /// An auto-reply, bounce or other machine-sent message, which must not
    /// start follow-ups or be answered automatically.
    pub automatic: bool,
    /// Sent to a mailing list.
    pub bulk: bool,
}

fn addresses(list: Option<mail_parser::AddressList<'_>>) -> Vec<Address> {
    list.map(|l| {
        l.mailboxes()
            .filter_map(|m| {
                let email = m.address()?.trim().to_ascii_lowercase();
                (email.contains('@') && email.len() <= 320).then(|| Address {
                    email,
                    name: m
                        .name()
                        .map(|n| clean(n.trim()).chars().take(200).collect::<String>())
                        .filter(|n| !n.is_empty()),
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

fn id_list(list: Option<mail_parser::TextList<'_>>) -> Vec<String> {
    list.map(|l| {
        l.iter()
            .map(|s| s.trim().trim_matches(['<', '>']).to_owned())
            .filter(|s| !s.is_empty() && s.len() <= 998)
            .take(100)
            .collect()
    })
    .unwrap_or_default()
}

/// Text as Postgres can keep it: without NUL characters, which some
/// messages carry in encoded headers or bodies.
fn clean(s: &str) -> String {
    s.replace('\0', "")
}

/// Parses a whole message. `None` when there is nothing recognisable as
/// one. Every attachment is listed, in order, whatever its size.
pub fn parse(raw: &[u8]) -> Option<ParsedMail> {
    let msg = MessageParser::default().parse(raw)?;
    let header = |name: &str| {
        msg.headers()
            .get(name)
            .and_then(|h| h.value().as_text().map(|t| t.trim().to_ascii_lowercase()))
    };
    let auto_submitted = header("auto-submitted").is_some_and(|v| v != "no");
    let precedence = header("precedence")
        .is_some_and(|v| matches!(v.as_str(), "bulk" | "junk" | "list" | "auto_reply"));
    let content_type = msg.content_type();
    let report = content_type.as_ref().is_some_and(|c| {
        c.ctype().eq_ignore_ascii_case("multipart")
            && c.subtype()
                .is_some_and(|s| s.eq_ignore_ascii_case("report"))
    });
    let from = addresses(msg.from()).into_iter().next();
    let daemon = from.as_ref().is_some_and(|f| {
        let local = f.email.split('@').next().unwrap_or_default();
        matches!(local, "mailer-daemon" | "postmaster")
    });
    let automatic = auto_submitted
        || precedence
        || report
        || daemon
        || msg.headers().contains("x-autoreply")
        || msg.headers().contains("x-autorespond");
    let bulk = msg.list_id().is_some() || msg.headers().contains("list-unsubscribe");

    let mut attachments = Vec::new();
    for part in msg.attachments() {
        let bytes = part.decoded().into_owned();
        let content_type = part
            .content_type()
            .map(|c| match c.subtype() {
                Some(sub) => format!("{}/{}", c.ctype(), sub),
                None => c.ctype().to_owned(),
            })
            .unwrap_or_else(|| "application/octet-stream".into())
            .to_ascii_lowercase();
        let filename = part
            .attachment_name()
            .map(clean)
            .unwrap_or_else(|| "attachment".into());
        attachments.push(ParsedAttachment {
            filename,
            content_type,
            bytes,
            inline: part.content_id().is_some() && !part.is_attachment(),
        });
    }

    Some(ParsedMail {
        message_id: msg
            .message_id()
            .map(|s| s.trim().trim_matches(['<', '>']).to_owned())
            .filter(|s| !s.is_empty() && s.len() <= 998),
        in_reply_to: id_list(msg.in_reply_to()),
        references: id_list(msg.references()),
        from,
        reply_to: addresses(msg.reply_to()).into_iter().next(),
        to: addresses(msg.to()),
        cc: addresses(msg.cc()),
        subject: msg
            .subject()
            .map(|s| clean(s.trim()).chars().take(1000).collect()),
        date: msg
            .date()
            .and_then(|d| Utc.timestamp_opt(d.to_timestamp(), 0).single()),
        text: msg.body_text(0).map(|t| clean(&t)),
        html: msg
            .html_body()
            .next()
            .and_then(|_| msg.body_html(0).map(|h| clean(&h))),
        attachments,
        automatic,
        bulk,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_postgres_cannot_keep_is_cleaned() {
        let long = "x".repeat(1500);
        let raw = format!(
            "From: Bea <bea@grand.test>\r\nSubject: {long}\r\nContent-Type: text/plain\r\n\
             Content-Transfer-Encoding: quoted-printable\r\n\r\nHello=00 there\r\n"
        );
        let m = parse(raw.as_bytes()).expect("a message");
        assert_eq!(m.subject.expect("a subject").chars().count(), 1000);
        assert_eq!(m.text.expect("a body").trim(), "Hello there");
    }

    const RAW: &[u8] = b"From: Bea Booker <Bea@Grand.test>\r\n\
To: Ana Agent <ana@agency.test>, ops@agency.test\r\n\
Cc: =?UTF-8?Q?Z=C3=B6e?= <zoe@grand.test>\r\n\
Subject: Booking enquiry: summer ball\r\n\
Date: Tue, 27 Oct 2026 09:30:00 +0000\r\n\
Message-ID: <abc123@grand.test>\r\n\
In-Reply-To: <q1@onhold.test>\r\n\
References: <q0@onhold.test> <q1@onhold.test>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"b1\"\r\n\
\r\n\
--b1\r\n\
Content-Type: multipart/alternative; boundary=\"b2\"\r\n\
\r\n\
--b2\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Hello, are the Owls free on 12 June?\r\n\
--b2\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>Hello, are the <b>Owls</b> free on 12 June?</p><script>alert(1)</script>\r\n\
--b2--\r\n\
--b1\r\n\
Content-Type: application/pdf; name=\"rider.pdf\"\r\n\
Content-Disposition: attachment; filename=\"rider.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjcK\r\n\
--b1--\r\n";

    #[test]
    fn parses_what_the_inbox_keeps() {
        let m = parse(RAW).expect("a message");
        assert_eq!(m.message_id.as_deref(), Some("abc123@grand.test"));
        assert_eq!(m.in_reply_to, ["q1@onhold.test"]);
        assert_eq!(m.references, ["q0@onhold.test", "q1@onhold.test"]);
        let from = m.from.expect("a sender");
        assert_eq!(from.email, "bea@grand.test");
        assert_eq!(from.name.as_deref(), Some("Bea Booker"));
        assert_eq!(
            m.to.iter().map(|a| a.email.as_str()).collect::<Vec<_>>(),
            ["ana@agency.test", "ops@agency.test"]
        );
        assert_eq!(m.cc[0].name.as_deref(), Some("Zöe"));
        assert_eq!(m.subject.as_deref(), Some("Booking enquiry: summer ball"));
        assert_eq!(
            m.date.map(|d| d.to_rfc3339()),
            Some("2026-10-27T09:30:00+00:00".into())
        );
        assert!(m.text.as_deref().is_some_and(|t| t.contains("12 June")));
        assert!(m.html.as_deref().is_some_and(|h| h.contains("<b>Owls</b>")));
        assert_eq!(m.attachments.len(), 1);
        assert_eq!(m.attachments[0].filename, "rider.pdf");
        assert_eq!(m.attachments[0].content_type, "application/pdf");
        assert_eq!(m.attachments[0].bytes, b"%PDF-1.7\n");
        assert!(!m.automatic && !m.bulk);
    }

    #[test]
    fn machine_mail_is_flagged() {
        let auto = b"From: Bea <bea@grand.test>\r\nAuto-Submitted: auto-replied\r\nSubject: Out of office\r\n\r\nAway\r\n";
        assert!(parse(auto).expect("parses").automatic);
        let bounce =
            b"From: Mail Delivery <MAILER-DAEMON@mx.test>\r\nSubject: Undelivered\r\n\r\nx\r\n";
        assert!(parse(bounce).expect("parses").automatic);
        let list = b"From: News <news@list.test>\r\nList-Id: <news.list.test>\r\nSubject: Weekly\r\n\r\nx\r\n";
        let l = parse(list).expect("parses");
        assert!(l.bulk && !l.automatic);
        assert!(parse(b"").is_none_or(|m| m.from.is_none()));
    }

    #[test]
    fn builds_replies_that_thread() {
        let mail = OutgoingMail {
            from: "ana@agency.test".into(),
            from_name: Some("Ana Agent".into()),
            to: vec!["bea@grand.test".into()],
            cc: vec!["zoe@grand.test".into()],
            subject: "Re: Booking enquiry".into(),
            text: "Yes, they are.".into(),
            html: Some("<p>Yes, they are.</p>".into()),
            message_id: "<r1@onhold.test>".into(),
            in_reply_to: Some("<abc123@grand.test>".into()),
            references: vec!["<q1@onhold.test>".into(), "<abc123@grand.test>".into()],
            thread_id: None,
            attachments: vec![Attachment {
                filename: "contract.pdf".into(),
                content_type: "application/pdf".into(),
                bytes: b"%PDF-1.7".to_vec(),
            }],
        };
        let raw = String::from_utf8(build(&mail).expect("builds").formatted()).expect("utf8");
        assert!(raw.contains("In-Reply-To: <abc123@grand.test>"), "{raw}");
        assert!(
            raw.contains("References: <q1@onhold.test> <abc123@grand.test>"),
            "{raw}"
        );
        assert!(raw.contains("Auto-Submitted: no"), "{raw}");
        assert!(raw.contains("Cc: zoe@grand.test"), "{raw}");
        let back = parse(raw.as_bytes()).expect("parses back");
        assert_eq!(back.in_reply_to, ["abc123@grand.test"]);
        assert_eq!(back.attachments[0].filename, "contract.pdf");

        let mut none = mail;
        none.to.clear();
        assert!(build(&none).is_err());
    }
}
