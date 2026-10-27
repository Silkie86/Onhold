//! Provider implementations behind the core's integration traits: outbound
//! mail ([`mail::Mailer`]: SMTP, a log-only mailer for development, and an
//! in-memory one for tests), the guarded HTTP client webhooks are delivered
//! with ([`http::WebhookClient`]), file storage outside the database
//! ([`storage::BlobStore`]: a directory or an S3-compatible bucket) and the
//! daily reference exchange rates ([`fx`]). Payment, calendar, accounting
//! and messaging providers follow.

pub mod fx;
pub mod http;
pub mod mail;
pub mod storage;
