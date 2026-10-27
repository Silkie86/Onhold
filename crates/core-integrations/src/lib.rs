//! Provider implementations behind the core's integration traits. M2 adds
//! outbound mail ([`mail::Mailer`]: SMTP, a log-only mailer for development,
//! and an in-memory one for tests) and the guarded HTTP client webhooks are
//! delivered with ([`http::WebhookClient`]). Calendar, payment, accounting
//! and messaging providers arrive in M5 and M6.

pub mod http;
pub mod mail;
