//! Outbound email.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use lettre::message::header::ContentType;
use lettre::message::{Attachment as Part, Mailbox, MultiPart, SinglePart};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

/// One message to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Email {
    pub to: String,
    /// Display name shown with the platform's sending address, e.g. the workspace name.
    pub from_name: Option<String>,
    pub reply_to: Option<String>,
    pub subject: String,
    pub html: Option<String>,
    pub text: String,
    /// Stable per logical message, so a retried send can be recognised downstream.
    pub message_id: String,
    /// Files sent with it, such as an invoice's PDF.
    pub attachments: Vec<Attachment>,
}

/// A file attached to an email.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub provider_message_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MailError {
    /// Worth retrying: the server was down, busy or slow.
    #[error("temporary mail failure: {0}")]
    Temporary(String),
    /// Retrying will not help: a rejected address, a bad message.
    #[error("mail rejected: {0}")]
    Permanent(String),
}

#[async_trait]
pub trait Mailer: Send + Sync {
    async fn send(&self, email: &Email) -> Result<Sent, MailError>;
}

/// Sends through an SMTP server (any provider: Postmark, SES, Mailgun, a
/// workspace's own server once M6 adds per-workspace senders).
pub struct SmtpMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl SmtpMailer {
    /// `url` is `smtp://user:pass@host:587?tls=required`, `smtps://…:465`, or
    /// `smtp://localhost:1025` for a local catcher such as Mailpit.
    pub fn from_url(url: &str, from: &str) -> Result<Self, MailError> {
        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
            .map_err(|e| MailError::Permanent(format!("SMTP_URL: {e}")))?
            .build();
        let from = from
            .parse::<Mailbox>()
            .map_err(|e| MailError::Permanent(format!("MAIL_FROM: {e}")))?;
        Ok(Self { transport, from })
    }

    fn build(&self, email: &Email) -> Result<Message, MailError> {
        let bad =
            |what: &str, e: &dyn std::fmt::Display| MailError::Permanent(format!("{what}: {e}"));
        let mut from = self.from.clone();
        if let Some(name) = &email.from_name {
            from.name = Some(name.clone());
        }
        let mut builder = Message::builder()
            .from(from)
            .to(email
                .to
                .parse::<Mailbox>()
                .map_err(|e| bad("recipient", &e))?)
            .subject(email.subject.clone())
            .message_id(Some(email.message_id.clone()));
        if let Some(reply_to) = &email.reply_to {
            builder = builder.reply_to(
                reply_to
                    .parse::<Mailbox>()
                    .map_err(|e| bad("reply-to", &e))?,
            );
        }
        let text = SinglePart::builder()
            .header(ContentType::TEXT_PLAIN)
            .body(email.text.clone());
        let body = match &email.html {
            Some(html) => MultiPart::alternative().singlepart(text).singlepart(
                SinglePart::builder()
                    .header(ContentType::TEXT_HTML)
                    .body(html.clone()),
            ),
            None if email.attachments.is_empty() => {
                return builder
                    .header(ContentType::TEXT_PLAIN)
                    .body(email.text.clone())
                    .map_err(|e| bad("message", &e));
            }
            None => MultiPart::mixed().singlepart(text),
        };
        if email.attachments.is_empty() {
            return builder.multipart(body).map_err(|e| bad("message", &e));
        }
        let mut mixed = MultiPart::mixed().multipart(body);
        for a in &email.attachments {
            let content_type =
                ContentType::parse(&a.content_type).map_err(|e| bad("attachment", &e))?;
            mixed =
                mixed.singlepart(Part::new(a.filename.clone()).body(a.bytes.clone(), content_type));
        }
        builder.multipart(mixed).map_err(|e| bad("message", &e))
    }
}

#[async_trait]
impl Mailer for SmtpMailer {
    async fn send(&self, email: &Email) -> Result<Sent, MailError> {
        let message = self.build(email)?;
        match self.transport.send(message).await {
            Ok(_) => Ok(Sent {
                provider_message_id: Some(email.message_id.clone()),
            }),
            Err(e) if e.is_permanent() => Err(MailError::Permanent(e.to_string())),
            Err(e) => Err(MailError::Temporary(e.to_string())),
        }
    }
}

/// Logs each message instead of sending it: local development without a
/// mail server. Never configure in production.
pub struct LogMailer;

#[async_trait]
impl Mailer for LogMailer {
    async fn send(&self, email: &Email) -> Result<Sent, MailError> {
        tracing::info!(to = %email.to, subject = %email.subject, "email not sent (no SMTP_URL); text follows\n{}", email.text);
        Ok(Sent {
            provider_message_id: None,
        })
    }
}

/// Keeps messages in memory, and can be told to fail, for tests.
#[derive(Default)]
pub struct MemoryMailer {
    sent: Mutex<Vec<Email>>,
    failures_left: Mutex<usize>,
    permanent: AtomicBool,
}

impl MemoryMailer {
    /// The next `n` sends fail with a temporary error.
    pub fn fail_next(&self, n: usize) {
        if let Ok(mut left) = self.failures_left.lock() {
            *left = n;
        }
    }

    /// Every send fails permanently from now on.
    pub fn reject_all(&self) {
        self.permanent.store(true, Ordering::SeqCst);
    }

    pub fn sent(&self) -> Vec<Email> {
        self.sent.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl Mailer for MemoryMailer {
    async fn send(&self, email: &Email) -> Result<Sent, MailError> {
        if self.permanent.load(Ordering::SeqCst) {
            return Err(MailError::Permanent("550 mailbox unavailable".into()));
        }
        let failing = self.failures_left.lock().is_ok_and(|mut left| {
            let fail = *left > 0;
            *left = left.saturating_sub(1);
            fail
        });
        if failing {
            return Err(MailError::Temporary("421 try again later".into()));
        }
        if let Ok(mut sent) = self.sent.lock() {
            sent.push(email.clone());
        }
        Ok(Sent {
            provider_message_id: Some(email.message_id.clone()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email() -> Email {
        Email {
            to: "Ana <ana@example.com>".into(),
            from_name: Some("Acme Agency".into()),
            reply_to: Some("office@acme.test".into()),
            subject: "Hello".into(),
            html: Some("<p>Hi</p>".into()),
            text: "Hi".into(),
            message_id: "<1@onhold.test>".into(),
            attachments: Vec::new(),
        }
    }

    // The SMTP transport's connection pool needs a Tokio runtime, even to drop.
    #[tokio::test]
    async fn builds_a_multipart_message() {
        let mailer = SmtpMailer::from_url("smtp://localhost:1025", "OnHold <noreply@onhold.test>")
            .expect("config");
        let raw =
            String::from_utf8(mailer.build(&email()).expect("builds").formatted()).expect("utf8");
        assert!(
            raw.contains("From: \"Acme Agency\" <noreply@onhold.test>"),
            "{raw}"
        );
        assert!(raw.contains("Reply-To: office@acme.test"), "{raw}");
        assert!(raw.contains("Message-ID: <1@onhold.test>"), "{raw}");
        assert!(raw.contains("multipart/alternative"), "{raw}");
        let mut with_pdf = email();
        with_pdf.attachments.push(Attachment {
            filename: "INV-2026-0001.pdf".into(),
            content_type: "application/pdf".into(),
            bytes: b"%PDF-1.7".to_vec(),
        });
        let raw =
            String::from_utf8(mailer.build(&with_pdf).expect("builds").formatted()).expect("utf8");
        assert!(raw.contains("multipart/mixed"), "{raw}");
        assert!(raw.contains("filename=\"INV-2026-0001.pdf\""), "{raw}");
        let mut bad = email();
        bad.to = "not an address".into();
        assert!(matches!(mailer.build(&bad), Err(MailError::Permanent(_))));
    }

    #[tokio::test]
    async fn bad_configuration_is_reported() {
        assert!(SmtpMailer::from_url("ftp://x", "a@b.c").is_err());
        assert!(SmtpMailer::from_url("smtp://localhost", "nobody").is_err());
    }

    #[tokio::test]
    async fn memory_mailer_fails_on_request() {
        let m = MemoryMailer::default();
        m.fail_next(2);
        assert!(matches!(
            m.send(&email()).await,
            Err(MailError::Temporary(_))
        ));
        assert!(matches!(
            m.send(&email()).await,
            Err(MailError::Temporary(_))
        ));
        assert!(m.send(&email()).await.is_ok());
        assert_eq!(m.sent().len(), 1);
        m.reject_all();
        assert!(matches!(
            m.send(&email()).await,
            Err(MailError::Permanent(_))
        ));
    }
}
