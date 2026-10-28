//! Provider implementations behind the core's integration traits: outbound
//! mail ([`mail::Mailer`]: SMTP, a log-only mailer for development, and an
//! in-memory one for tests), the guarded HTTP client webhooks are delivered
//! with ([`http::WebhookClient`]), file storage outside the database
//! ([`storage::BlobStore`]: a directory or an S3-compatible bucket), the
//! daily reference exchange rates ([`fx`]), Stripe Connect payments
//! ([`stripe`]), and people's own mailboxes ([`mailbox::MailProvider`]:
//! Gmail, Microsoft Graph, IMAP and SMTP), connected through [`oauth`].
//! Calendar, accounting and messaging providers follow.

pub mod fx;
pub mod gmail;
pub mod graph;
pub mod http;
pub mod imap;
pub mod mail;
pub mod mailbox;
pub mod oauth;
pub mod provider;
pub mod storage;
pub mod stripe;
