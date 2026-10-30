//! Provider implementations behind the core's integration traits: outbound
//! mail ([`mail::Mailer`]: SMTP, a log-only mailer for development, and an
//! in-memory one for tests), the guarded HTTP client webhooks are delivered
//! with ([`http::WebhookClient`]), file storage outside the database
//! ([`storage::BlobStore`]: a directory or an S3-compatible bucket), the
//! daily reference exchange rates ([`fx`]), Stripe Connect payments
//! ([`stripe`]), and people's own mailboxes ([`mailbox::MailProvider`]:
//! Gmail, Microsoft Graph, IMAP and SMTP), connected through [`oauth`],
//! and workspaces' own sender domains ([`domains`]: Resend), set up in their
//! DNS through Domain Connect ([`dns`]).
//! When people are busy comes from their own calendars ([`calendars`]:
//! Google, Microsoft or an iCal link), and bookings go into members' own
//! calendars ([`calendar_events`]). A workspace's books are kept in Xero or
//! QuickBooks ([`accounting`]), and text messages go through the
//! workspace's own Twilio and WhatsApp numbers ([`messaging`]).

pub mod accounting;
pub mod calendar_events;
pub mod calendars;
pub mod dns;
pub mod domains;
pub mod fx;
pub mod gmail;
pub mod graph;
pub mod http;
pub mod imap;
pub mod mail;
pub mod mailbox;
pub mod messaging;
pub mod oauth;
pub mod provider;
pub mod storage;
pub mod stripe;
