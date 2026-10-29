//! The unified inbox. The worker reads connected mailboxes into threads
//! ([`ingest`]), linking each to the contact it is with and keeping a
//! conversation's replies together; members read, link, assign, snooze
//! and resolve threads, and reply from the mailbox the thread lives in
//! (sent by the worker, [`send`]). With no mailbox connected, a message
//! goes through OnHold's own sender with the member's address to reply to.
//!
//! A thread in a mailbox its owner has not shared is seen only by them.
//! Events about threads name the thread and what it is linked to, never
//! its addresses or text, since webhooks carry events outside OnHold.

use ammonia::Builder;
use chrono::{DateTime, Duration, Utc};
use core_db::inbox::{Links, NewMessage, NewThread, Thread};
use core_db::integrations::Integration;
use core_domain::{Ctx, Permission, UserId, WorkspaceId};
use core_integrations::mail::{Attachment, Email, MailError, Mailer};
use core_integrations::mailbox::{self, FetchedMail, OutgoingMail};
use core_integrations::provider::ProviderError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::crm::Check;
use super::integrations::viewer;
use super::{Change, begin, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

pub const ENTITY: &str = "thread";
/// Sends one message written in OnHold (`message_id`).
pub const JOB_SEND: &str = "inbox.send";
/// Wakes a snoozed thread (`thread_id`, `at`).
pub const JOB_WAKE: &str = "inbox.wake";

const STATUSES: &[&str] = &["open", "snoozed", "resolved"];
/// The most recipients one message may have.
const MAX_RECIPIENTS: usize = 50;
/// How long the body of a message written here may be.
const MAX_BODY: usize = 100_000;

// Views ------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ThreadView {
    pub id: Uuid,
    /// `email`, `sms` or `whatsapp`.
    pub channel: String,
    /// The mailbox it lives in; none for one sent through OnHold's own sender.
    pub integration_id: Option<Uuid>,
    /// The workspace's SMS or WhatsApp number it is on.
    pub messaging_channel_id: Option<Uuid>,
    pub subject: Option<String>,
    pub contact_id: Option<Uuid>,
    pub organisation_id: Option<Uuid>,
    pub enquiry_id: Option<Uuid>,
    pub booking_id: Option<Uuid>,
    /// `open`, `snoozed` or `resolved`.
    pub status: String,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub assigned_to: Option<Uuid>,
    pub unread: bool,
    pub message_count: i32,
    /// The other people in it.
    pub participants: Vec<String>,
    /// The start of the latest message.
    pub preview: Option<String>,
    pub last_message_at: DateTime<Utc>,
    pub last_inbound_at: Option<DateTime<Utc>>,
    pub last_outbound_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

impl From<Thread> for ThreadView {
    fn from(t: Thread) -> Self {
        Self {
            id: t.id,
            channel: t.channel,
            integration_id: t.integration_id,
            messaging_channel_id: t.messaging_channel_id,
            subject: t.subject,
            contact_id: t.contact_id,
            organisation_id: t.organisation_id,
            enquiry_id: t.enquiry_id,
            booking_id: t.booking_id,
            status: t.status,
            snoozed_until: t.snoozed_until,
            assigned_to: t.assigned_to,
            unread: t.unread,
            message_count: t.message_count,
            participants: t.participants,
            preview: t.preview,
            last_message_at: t.last_message_at,
            last_inbound_at: t.last_inbound_at,
            last_outbound_at: t.last_outbound_at,
            created_at: t.created_at,
            version: t.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ThreadPage {
    pub items: Vec<ThreadView>,
    pub total: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MessageFileView {
    pub id: Uuid,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MessageView {
    pub id: Uuid,
    /// `inbound` or `outbound`.
    pub direction: String,
    pub from_address: Option<String>,
    pub from_name: Option<String>,
    /// Where the sender asked answers to go, when not to `from_address`.
    pub reply_to: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: Option<String>,
    pub text: String,
    /// Sanitised: no scripts, styles, forms or remote images. Show it in a
    /// sandboxed frame all the same.
    pub html: Option<String>,
    pub sent_at: DateTime<Utc>,
    /// `received`, or for messages written here `queued`, `sent` or `failed`.
    pub status: String,
    pub error: Option<String>,
    /// An auto-reply, bounce or other machine-sent message.
    pub automatic: bool,
    /// Sent to a mailing list.
    pub bulk: bool,
    /// Who wrote it in OnHold.
    pub author_id: Option<Uuid>,
    pub template_key: Option<String>,
    pub files: Vec<MessageFileView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ThreadDetail {
    pub thread: ThreadView,
    /// Oldest first.
    pub messages: Vec<MessageView>,
    /// The mailbox replies go out from; none when it was disconnected or
    /// never was, and replies go through OnHold's own sender.
    pub mailbox: Option<String>,
}

pub(crate) fn event_json(t: &Thread) -> Value {
    json!({
        "status": t.status,
        "snoozed_until": t.snoozed_until,
        "assigned_to": t.assigned_to,
        "unread": t.unread,
        "contact_id": t.contact_id,
        "organisation_id": t.organisation_id,
        "enquiry_id": t.enquiry_id,
        "booking_id": t.booking_id,
        "integration_id": t.integration_id,
    })
}

pub(crate) async fn load(tx: &mut core_db::Tx, ctx: &Ctx, id: Uuid) -> ApiResult<Thread> {
    core_db::inbox::get(tx, id, viewer(ctx))
        .await?
        .ok_or(ApiError::NotFound)
}

pub(crate) async fn detail(
    state: &AppState,
    tx: &mut core_db::Tx,
    t: Thread,
) -> ApiResult<ThreadDetail> {
    let messages = core_db::inbox::messages(tx, t.id).await?;
    let files = core_db::inbox::files(tx, t.id).await?;
    let mailbox = match t.integration_id {
        Some(i) => core_db::integrations::get(tx, i)
            .await?
            .filter(|i| i.disconnected_at.is_none())
            .map(|i| i.mailbox().to_owned()),
        None => None,
    };
    let _ = state;
    let messages = messages
        .into_iter()
        .map(|m| MessageView {
            files: files
                .iter()
                .filter(|f| f.message_id == m.id)
                .map(|f| MessageFileView {
                    id: f.id,
                    filename: f.filename.clone(),
                    content_type: f.content_type.clone(),
                    size_bytes: f.size_bytes,
                })
                .collect(),
            id: m.id,
            direction: m.direction,
            from_address: m.from_address,
            from_name: m.from_name,
            reply_to: m.reply_to,
            to: m.to_addresses,
            cc: m.cc_addresses,
            subject: m.subject,
            text: m.body_text,
            html: m.body_html,
            sent_at: m.sent_at,
            status: m.status,
            error: m.error,
            automatic: m.automatic,
            bulk: m.bulk,
            author_id: m.author_id,
            template_key: m.template_key,
        })
        .collect();
    Ok(ThreadDetail {
        thread: t.into(),
        messages,
        mailbox,
    })
}

// Reading ------------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
pub struct ThreadQuery {
    /// `open`, `snoozed` or `resolved`.
    pub status: Option<String>,
    /// One mailbox's threads.
    pub integration_id: Option<Uuid>,
    /// `email`, `sms` or `whatsapp`.
    pub channel: Option<String>,
    pub contact_id: Option<Uuid>,
    pub organisation_id: Option<Uuid>,
    pub enquiry_id: Option<Uuid>,
    pub booking_id: Option<Uuid>,
    pub assigned_to: Option<Uuid>,
    /// Only threads linked to no contact, enquiry or booking.
    #[serde(default)]
    pub unlinked: bool,
    #[serde(default)]
    pub unread: bool,
    /// Matched against the subject and the people in it.
    pub q: Option<String>,
    pub offset: Option<i64>,
    /// 1 to 200, default 50.
    pub limit: Option<i64>,
}

pub async fn list(state: &AppState, ctx: &Ctx, q: ThreadQuery) -> ApiResult<ThreadPage> {
    ctx.authorize(Permission::InboxRead)?;
    if let Some(s) = q.status.as_deref()
        && !STATUSES.contains(&s)
    {
        return Err(ApiError::invalid("status", "use open, snoozed or resolved"));
    }
    if let Some(c) = q.channel.as_deref()
        && !["email", "sms", "whatsapp", "portal"].contains(&c)
    {
        return Err(ApiError::invalid(
            "channel",
            "use email, sms, whatsapp or portal",
        ));
    }
    let search = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let mut tx = begin(state, ctx).await?;
    let (rows, total) = core_db::inbox::list(
        &mut tx,
        core_db::inbox::Filter {
            viewer: viewer(ctx),
            status: q.status.as_deref(),
            integration_id: q.integration_id,
            channel: q.channel.as_deref(),
            contact_id: q.contact_id,
            organisation_id: q.organisation_id,
            enquiry_id: q.enquiry_id,
            booking_id: q.booking_id,
            assigned_to: q.assigned_to,
            unlinked: q.unlinked,
            unread: q.unread,
            q: search,
            offset: q.offset.unwrap_or(0).max(0),
            limit: q.limit.unwrap_or(50).clamp(1, 200),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(ThreadPage {
        items: rows.into_iter().map(ThreadView::from).collect(),
        total,
    })
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<ThreadDetail> {
    ctx.authorize(Permission::InboxRead)?;
    let mut tx = begin(state, ctx).await?;
    let t = load(&mut tx, ctx, id).await?;
    let d = detail(state, &mut tx, t).await?;
    tx.commit().await?;
    Ok(d)
}

/// A file that came with, or was sent with, a message in the thread. One
/// that came from a mailbox is read from there again: OnHold keeps only
/// its name and size.
pub async fn download(
    state: &AppState,
    ctx: &Ctx,
    thread_id: Uuid,
    id: Uuid,
) -> ApiResult<super::files::Download> {
    ctx.authorize(Permission::InboxRead)?;
    let mut tx = begin(state, ctx).await?;
    let thread = load(&mut tx, ctx, thread_id).await?;
    let found = core_db::inbox::file(&mut tx, thread_id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if let Some(file_id) = found.file.file_id {
        let file = core_db::files::get(&mut tx, file_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        let bytes = super::files::bytes(state, &mut tx, &file).await?;
        tx.commit().await?;
        return Ok(super::files::Download {
            filename: file.filename,
            content_type: file.content_type,
            bytes,
        });
    }
    tx.commit().await?;
    // Only through the mailbox the thread belongs to, which is the one the
    // reader was allowed to see.
    let (Some(integration_id), Some(external_id), Some(part)) =
        (found.integration_id, found.external_id, found.file.part)
    else {
        return Err(ApiError::NotFound);
    };
    if thread.integration_id != Some(integration_id) {
        return Err(ApiError::NotFound);
    }
    let raw = super::integrations::fetch_raw(state, ctx, integration_id, &external_id).await?;
    let attachment = mailbox::parse(&raw)
        .and_then(|m| m.attachments.into_iter().nth(part as usize))
        .ok_or_else(|| {
            ApiError::conflict(
                "message_gone",
                "the message in the mailbox no longer has this file",
            )
        })?;
    Ok(super::files::Download {
        filename: found.file.filename,
        content_type: found.file.content_type,
        bytes: attachment.bytes,
    })
}

// Changing a thread --------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct ThreadPatch {
    /// `open`, `snoozed` (with `snoozed_until`) or `resolved`.
    pub status: Option<String>,
    /// When a snoozed thread comes back; setting it snoozes the thread.
    pub snoozed_until: Option<DateTime<Utc>>,
    /// `null` unassigns.
    #[serde(default, deserialize_with = "crate::routes::nullable")]
    #[schema(value_type = Option<Uuid>)]
    pub assigned_to: Option<Option<Uuid>>,
    pub unread: Option<bool>,
    /// `null` unlinks; likewise for the other records. Linking a contact
    /// also links their organisation, unless one is given.
    #[serde(default, deserialize_with = "crate::routes::nullable")]
    #[schema(value_type = Option<Uuid>)]
    pub contact_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "crate::routes::nullable")]
    #[schema(value_type = Option<Uuid>)]
    pub organisation_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "crate::routes::nullable")]
    #[schema(value_type = Option<Uuid>)]
    pub enquiry_id: Option<Option<Uuid>>,
    #[serde(default, deserialize_with = "crate::routes::nullable")]
    #[schema(value_type = Option<Uuid>)]
    pub booking_id: Option<Option<Uuid>>,
}

impl ThreadPatch {
    fn links(&self) -> bool {
        self.contact_id.is_some()
            || self.organisation_id.is_some()
            || self.enquiry_id.is_some()
            || self.booking_id.is_some()
    }
}

pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    p: ThreadPatch,
    if_match: Option<i64>,
) -> ApiResult<ThreadDetail> {
    // Marking a thread read is reading it; everything else is a change.
    let only_read = p.unread.is_some()
        && p.status.is_none()
        && p.snoozed_until.is_none()
        && p.assigned_to.is_none()
        && !p.links();
    ctx.authorize(if only_read {
        Permission::InboxRead
    } else {
        Permission::InboxWrite
    })?;
    let mut tx = begin(state, ctx).await?;
    let before = load(&mut tx, ctx, id).await?;
    let version = core_db::inbox::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    super::check_version(version, if_match)?;

    let mut check = Check::default();
    // Status and snooze.
    let mut status = p.status.clone().unwrap_or_else(|| before.status.clone());
    let mut snoozed_until = before.snoozed_until;
    if let Some(s) = p.status.as_deref()
        && !STATUSES.contains(&s)
    {
        check.issue("status", "use open, snoozed or resolved");
    }
    if let Some(until) = p.snoozed_until {
        if until <= Utc::now() {
            check.issue("snoozed_until", "must be in the future");
        }
        if p.status.as_deref().is_some_and(|s| s != "snoozed") {
            check.issue(
                "snoozed_until",
                "only a snoozed thread has a time to come back",
            );
        }
        status = "snoozed".into();
        snoozed_until = Some(until);
    } else if status == "snoozed" && p.status.is_some() {
        check.issue("snoozed_until", "say when the thread should come back");
    }
    if status != "snoozed" {
        snoozed_until = None;
    }
    let assigned_to = match p.assigned_to {
        Some(Some(u)) => {
            if core_db::members::get(&mut tx, UserId(u)).await?.is_none() {
                check.issue("assigned_to", "is not a member of this workspace");
            }
            Some(u)
        }
        Some(None) => None,
        None => before.assigned_to,
    };
    // Links.
    let mut links = Links {
        contact_id: p.contact_id.unwrap_or(before.contact_id),
        organisation_id: p.organisation_id.unwrap_or(before.organisation_id),
        enquiry_id: p.enquiry_id.unwrap_or(before.enquiry_id),
        booking_id: p.booking_id.unwrap_or(before.booking_id),
    };
    if p.links() {
        check_links(&mut tx, ctx, &mut check, &p, &mut links).await?;
    }
    // Unlinking the contact also unlinks the organisation that came with
    // it, unless an organisation is given.
    if p.contact_id == Some(None) && p.organisation_id.is_none() {
        links.organisation_id = None;
    }
    check.finish()?;

    core_db::inbox::set_state(
        &mut tx,
        id,
        &core_db::inbox::State {
            status,
            snoozed_until,
            assigned_to,
            unread: p.unread.unwrap_or(before.unread),
        },
    )
    .await?;
    if p.links() {
        core_db::inbox::set_links(&mut tx, id, &links).await?;
    }
    let after = load(&mut tx, ctx, id).await?;
    if let (Some(until), true) = (
        after.snoozed_until,
        after.snoozed_until != before.snoozed_until,
    ) {
        queue_wake(&mut tx, ctx.workspace_id, id, until).await?;
    }
    // Reading a thread is not worth an audit entry or an event.
    if !only_read {
        let topic = if before.status != after.status {
            match after.status.as_str() {
                "resolved" => "thread.resolved",
                "snoozed" => "thread.snoozed",
                _ => "thread.reopened",
            }
        } else if before.assigned_to != after.assigned_to {
            "thread.assigned"
        } else {
            "thread.updated"
        };
        record(
            &mut tx,
            ctx,
            Change {
                topic,
                resource: ENTITY,
                object_id: Some(id),
                before: Some(event_json(&before)),
                after: Some(event_json(&after)),
                event_extra: None,
            },
        )
        .await?;
    }
    let d = detail(state, &mut tx, after).await?;
    tx.commit().await?;
    Ok(d)
}

/// Checks the records a thread is linked to exist and may be seen, and
/// fills in the organisation from the contact.
async fn check_links(
    tx: &mut core_db::Tx,
    ctx: &Ctx,
    check: &mut Check,
    p: &ThreadPatch,
    links: &mut Links,
) -> ApiResult<()> {
    if let Some(Some(c)) = p.contact_id {
        ctx.authorize(Permission::ContactRead)?;
        match super::contacts::load(tx, c).await? {
            Some(contact) => {
                if p.organisation_id.is_none() {
                    links.organisation_id = contact.organisation_id;
                }
            }
            None => check.issue("contact_id", "no such contact"),
        }
    }
    if let Some(Some(o)) = p.organisation_id {
        ctx.authorize(Permission::OrganisationRead)?;
        if super::organisations::load(tx, o).await?.is_none() {
            check.issue("organisation_id", "no such organisation");
        }
    }
    if let Some(Some(e)) = p.enquiry_id {
        ctx.authorize(Permission::EnquiryRead)?;
        if core_db::enquiries::get(tx, e).await?.is_none() {
            check.issue("enquiry_id", "no such enquiry");
        }
    }
    if let Some(Some(b)) = p.booking_id {
        ctx.authorize(Permission::BookingRead)?;
        if core_db::bookings::get(tx, b).await?.is_none() {
            check.issue("booking_id", "no such booking");
        }
    }
    Ok(())
}

async fn queue_wake(
    tx: &mut core_db::Tx,
    ws: WorkspaceId,
    id: Uuid,
    at: DateTime<Utc>,
) -> ApiResult<()> {
    let payload = json!({ "thread_id": id, "at": at });
    let key = format!("{id}@{}", at.to_rfc3339());
    core_db::jobs::enqueue(
        tx,
        core_db::jobs::NewJob {
            run_at: Some(at),
            ..core_db::jobs::NewJob::new(Some(ws), JOB_WAKE, &payload).dedupe(&key)
        },
    )
    .await?;
    Ok(())
}

/// Brings a snoozed thread back when its time comes (the worker's
/// `inbox.wake`). A thread snoozed again meanwhile is left alone.
pub async fn wake(
    state: &AppState,
    ws: WorkspaceId,
    id: Uuid,
    at: DateTime<Utc>,
) -> ApiResult<bool> {
    let ctx = Ctx::system(ws);
    let mut tx = begin(state, &ctx).await?;
    let woke = core_db::inbox::wake(&mut tx, id, at).await?;
    if woke {
        record(
            &mut tx,
            &ctx,
            Change {
                topic: "thread.woken",
                resource: ENTITY,
                object_id: Some(id),
                before: Some(json!({ "status": "snoozed", "snoozed_until": at })),
                after: Some(json!({ "status": "open" })),
                event_extra: None,
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(woke)
}

// Writing ------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct Reply {
    /// The message, as plain text. Give this or `template_key`.
    pub body: Option<String>,
    /// An email template to write it from, filled in with the thread's
    /// contact, enquiry and booking.
    pub template_key: Option<String>,
    /// Default: whoever wrote the latest message from outside.
    pub to: Option<Vec<String>>,
    #[serde(default)]
    pub cc: Vec<String>,
    /// Files uploaded with `POST /files` to send with it.
    #[serde(default)]
    pub attachments: Vec<Uuid>,
    /// Add your signature (or the workspace's) under it. Default: yes.
    #[serde(default = "yes")]
    pub signature: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct Compose {
    /// The mailbox to send from; default the sender's own, if connected.
    pub integration_id: Option<Uuid>,
    /// Send through OnHold, in the workspace's name, rather than from a
    /// mailbox (`integration_id` must then be left out).
    #[serde(default)]
    pub from_onhold: bool,
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    /// Required unless the template gives one.
    pub subject: Option<String>,
    pub body: Option<String>,
    pub template_key: Option<String>,
    pub contact_id: Option<Uuid>,
    pub enquiry_id: Option<Uuid>,
    pub booking_id: Option<Uuid>,
    #[serde(default)]
    pub attachments: Vec<Uuid>,
    /// Add your signature (or the workspace's) under it. Default: yes.
    #[serde(default = "yes")]
    pub signature: bool,
}

fn clean_addresses(field: &str, list: &[String], check: &mut Check) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in list {
        let a = a.trim().to_ascii_lowercase();
        if a.is_empty() {
            continue;
        }
        if !core_engines::fields::is_email(&a) || a.len() > 320 {
            check.issue(field, format!("`{a}` is not an email address"));
        } else if !out.contains(&a) {
            out.push(a);
        }
    }
    if out.len() > MAX_RECIPIENTS {
        check.issue(field, format!("at most {MAX_RECIPIENTS} addresses"));
    }
    out
}

/// Plain text as simple HTML.
fn text_to_html(text: &str) -> String {
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    format!(
        "<div style=\"white-space:pre-wrap\">{}</div>",
        escaped.replace("\r\n", "\n").replace('\n', "<br>")
    )
}

/// What a message written here says, from its body or a template.
struct Written {
    subject: Option<String>,
    text: String,
    html: String,
    template_key: Option<String>,
}

/// What a template written in the inbox is filled in with: the workspace,
/// the writer, and what the thread is linked to (as far as they may see).
pub(crate) async fn template_context(
    state: &AppState,
    tx: &mut core_db::Tx,
    ctx: &Ctx,
    about: &Links,
) -> ApiResult<(Value, core_engines::templates::Options)> {
    let (workspace, options) = super::context::workspace(tx, state).await?;
    let mut tree = serde_json::Map::new();
    tree.insert("workspace".into(), workspace);
    for (entity, id, permission) in [
        ("contact", about.contact_id, Permission::ContactRead),
        (
            "organisation",
            about.organisation_id,
            Permission::OrganisationRead,
        ),
        ("enquiry", about.enquiry_id, Permission::EnquiryRead),
        ("booking", about.booking_id, Permission::BookingRead),
    ] {
        if let Some(id) = id
            && ctx.permissions.contains(&permission)
            && let Some(obj) = super::context::load_object(tx, state, entity, id).await?
        {
            tree.insert(entity.into(), obj);
        }
    }
    if let Some(user) = ctx.user_id()
        && let Some(u) = core_db::users::get(tx, user).await?
    {
        tree.insert("user".into(), json!({ "name": u.name, "email": u.email }));
    }
    Ok((Value::Object(tree), options))
}

async fn write_body(
    state: &AppState,
    tx: &mut core_db::Tx,
    ctx: &Ctx,
    body: Option<&str>,
    template_key: Option<&str>,
    about: &Links,
) -> ApiResult<Written> {
    match (body.map(str::trim).filter(|b| !b.is_empty()), template_key) {
        (Some(_), Some(_)) => Err(ApiError::invalid(
            "body",
            "give the message or a template, not both",
        )),
        (None, None) => Err(ApiError::invalid(
            "body",
            "write a message or choose a template",
        )),
        (Some(b), None) => {
            if b.chars().count() > MAX_BODY {
                return Err(ApiError::invalid(
                    "body",
                    format!("must be at most {MAX_BODY} characters"),
                ));
            }
            Ok(Written {
                subject: None,
                text: b.to_owned(),
                html: text_to_html(b),
                template_key: None,
            })
        }
        (None, Some(key)) => {
            let (tree, options) = template_context(state, tx, ctx, about).await?;
            let template = core_db::templates::find(tx, key, &options.locale)
                .await?
                .filter(|t| t.kind == "email")
                .ok_or_else(|| ApiError::invalid("template_key", "no such email template"))?;
            let rendered = super::templates::render_in(
                tx,
                core_engines::templates::Kind::Email,
                template.subject.as_deref(),
                &template.body,
                &tree,
                &options,
            )
            .await?;
            Ok(Written {
                subject: rendered.subject.filter(|s| !s.trim().is_empty()),
                text: rendered.text,
                html: rendered.body,
                template_key: Some(key.to_owned()),
            })
        }
    }
}

/// The mailbox a member sends from: their own or a shared one, connected
/// and not waiting to be connected again.
async fn sending_mailbox(
    tx: &mut core_db::Tx,
    ctx: &Ctx,
    id: Uuid,
) -> ApiResult<Option<Integration>> {
    let Some(i) = core_db::integrations::get(tx, id).await? else {
        return Ok(None);
    };
    // A mailbox that needs connecting again cannot send either.
    if i.disconnected_at.is_some() || i.status == "reconnect" {
        return Ok(None);
    }
    if !(i.shared || viewer(ctx).is_none_or(|v| v == i.owner_id)) {
        return Err(ApiError::NotFound);
    }
    Ok(Some(i))
}

/// Files uploaded to send with a message.
async fn check_attachments(
    tx: &mut core_db::Tx,
    ctx: &Ctx,
    ids: &[Uuid],
    max_bytes: usize,
) -> ApiResult<Vec<Uuid>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    ctx.authorize(Permission::DocumentRead)?;
    let mut total: i64 = 0;
    let mut out = Vec::new();
    for id in ids {
        let f = core_db::files::get(tx, *id)
            .await?
            .ok_or_else(|| ApiError::invalid("attachments", format!("no such file {id}")))?;
        total += f.size_bytes;
        if !out.contains(id) {
            out.push(*id);
        }
    }
    if total > max_bytes as i64 {
        return Err(ApiError::invalid(
            "attachments",
            format!(
                "together the files may be at most {} MB",
                max_bytes / (1024 * 1024)
            ),
        ));
    }
    Ok(out)
}

fn new_message_id(state: &AppState) -> String {
    let host = state
        .config
        .public_base_url
        .host_str()
        .unwrap_or("onhold.invalid");
    format!("{}@{host}", core_domain::ids::new_id())
}

/// Attachments sent with mail may be 20 MB together, what most providers take.
const MAX_SEND_BYTES: usize = 20 * 1024 * 1024;

/// What a message may carry, by the mailbox it goes from.
fn send_limit(mailbox: Option<&Integration>) -> usize {
    match mailbox.map(|m| m.provider.as_str()) {
        Some("microsoft") => core_integrations::graph::MAX_SEND_ATTACHMENTS,
        _ => MAX_SEND_BYTES,
    }
}

/// Writes a reply in a thread; the worker sends it.
pub async fn reply(state: &AppState, ctx: &Ctx, id: Uuid, r: Reply) -> ApiResult<MessageView> {
    ctx.authorize(Permission::InboxWrite)?;
    let mut tx = begin(state, ctx).await?;
    let thread = load(&mut tx, ctx, id).await?;
    core_db::inbox::lock(&mut tx, id).await?;
    if let Some(channel) = thread.messaging_channel_id {
        let view = super::messaging::reply(state, &mut tx, ctx, &thread, channel, r).await?;
        tx.commit().await?;
        return Ok(view);
    }
    if thread.channel == "portal" {
        let message = super::portal::reply_in(state, &mut tx, ctx, id, r).await?;
        let t = load(&mut tx, ctx, id).await?;
        let view = detail(state, &mut tx, t)
            .await?
            .messages
            .into_iter()
            .find(|m| m.id == message)
            .ok_or(ApiError::NotFound)?;
        tx.commit().await?;
        return Ok(view);
    }
    let messages = core_db::inbox::messages(&mut tx, id).await?;
    let mailbox = match thread.integration_id {
        Some(i) => sending_mailbox(&mut tx, ctx, i).await?,
        None => None,
    };
    // A mailbox that needs connecting again cannot send, but answers still
    // go to it, to be read in once it is connected.
    let answer_to = match (thread.integration_id, &mailbox) {
        (Some(i), None) => core_db::integrations::get(&mut tx, i)
            .await?
            .filter(|i| i.disconnected_at.is_none())
            .map(|i| i.mailbox().to_owned()),
        _ => None,
    };
    let own: Vec<String> = mailbox.iter().map(|m| m.mailbox().to_owned()).collect();
    let mut check = Check::default();
    // Answer whoever wrote last from outside; else whoever we last wrote to.
    let default_to: Vec<String> = messages
        .iter()
        .rev()
        .find(|m| m.direction == "inbound" && !m.automatic)
        .and_then(|m| m.reply_to.clone().or_else(|| m.from_address.clone()))
        .map(|a| vec![a])
        .or_else(|| {
            messages
                .iter()
                .rev()
                .find(|m| m.direction == "outbound")
                .map(|m| m.to_addresses.clone())
        })
        .unwrap_or_default();
    let to = clean_addresses("to", r.to.as_deref().unwrap_or(&default_to), &mut check);
    let to: Vec<String> = to.into_iter().filter(|a| !own.contains(a)).collect();
    if to.is_empty() {
        check.issue("to", "say who to send it to");
    }
    let cc = clean_addresses("cc", &r.cc, &mut check);
    if mailbox.is_none() && !cc.is_empty() {
        check.issue("cc", "copies need a connected mailbox to send from");
    }
    if mailbox.is_none() && to.len() > 1 {
        check.issue(
            "to",
            "without a connected mailbox, a message goes to one person",
        );
    }
    check.finish()?;
    let attachments =
        check_attachments(&mut tx, ctx, &r.attachments, send_limit(mailbox.as_ref())).await?;
    let links = Links {
        contact_id: thread.contact_id,
        organisation_id: thread.organisation_id,
        enquiry_id: thread.enquiry_id,
        booking_id: thread.booking_id,
    };
    let mut written = write_body(
        state,
        &mut tx,
        ctx,
        r.body.as_deref(),
        r.template_key.as_deref(),
        &links,
    )
    .await?;
    if r.signature
        && let Some(sig) = core_db::signatures::for_sender(&mut tx, ctx.user_id()).await?
    {
        super::signatures::append(&mut written.text, &mut written.html, &sig);
    }
    let base = thread.subject.clone().unwrap_or_default();
    let subject = written.subject.clone().unwrap_or_else(|| {
        if base.to_ascii_lowercase().starts_with("re:") {
            base.clone()
        } else {
            format!("Re: {base}").trim().to_owned()
        }
    });
    let parent = messages.iter().rev().find(|m| m.message_id.is_some());
    let in_reply_to = parent.and_then(|m| m.message_id.clone());
    let mut references: Vec<String> = parent.map(|m| m.references.clone()).unwrap_or_default();
    if let Some(p) = &in_reply_to {
        references.push(p.clone());
    }
    let skip = references.len().saturating_sub(20);
    let references: Vec<String> = references.into_iter().skip(skip).collect();
    let view = queue_outbound(
        state,
        &mut tx,
        ctx,
        &thread,
        mailbox.as_ref(),
        Outbound {
            to,
            cc,
            subject,
            written,
            in_reply_to,
            references,
            attachments,
            answer_to,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(view)
}

/// Starts a new thread with a message; the worker sends it.
pub async fn compose(state: &AppState, ctx: &Ctx, c: Compose) -> ApiResult<ThreadDetail> {
    ctx.authorize(Permission::InboxWrite)?;
    let mut tx = begin(state, ctx).await?;
    let (id, _) = compose_in(state, &mut tx, ctx, c).await?;
    let t = load(&mut tx, ctx, id).await?;
    let d = detail(state, &mut tx, t).await?;
    tx.commit().await?;
    Ok(d)
}

/// The mailbox a member's mail goes from when they pick none: their own,
/// if they connected exactly one that works.
pub(crate) async fn own_mailbox(tx: &mut core_db::Tx, ctx: &Ctx) -> ApiResult<Option<Integration>> {
    let own: Vec<Integration> = core_db::integrations::list(tx, viewer(ctx))
        .await?
        .into_iter()
        .filter(|i| {
            Some(i.owner_id) == ctx.user_id()
                && i.status != "reconnect"
                && i.purposes.iter().any(|p| p == "mail")
        })
        .collect();
    Ok((own.len() == 1).then(|| own.into_iter().next()).flatten())
}

/// Starts a thread with a message to send, in the caller's transaction;
/// returns the thread and the message. The caller checks `InboxWrite`.
pub(crate) async fn compose_in(
    state: &AppState,
    tx: &mut core_db::Tx,
    ctx: &Ctx,
    c: Compose,
) -> ApiResult<(Uuid, MessageView)> {
    let mailbox = match (c.integration_id, c.from_onhold) {
        (Some(_), true) => {
            return Err(ApiError::invalid(
                "from_onhold",
                "choose a mailbox or OnHold, not both",
            ));
        }
        (Some(id), false) => Some(sending_mailbox(tx, ctx, id).await?.ok_or_else(|| {
            ApiError::invalid(
                "integration_id",
                "that mailbox is not connected, or needs connecting again",
            )
        })?),
        (None, true) => None,
        (None, false) => own_mailbox(tx, ctx).await?,
    };
    let mut check = Check::default();
    let to = clean_addresses("to", &c.to, &mut check);
    if to.is_empty() {
        check.issue("to", "say who to send it to");
    }
    let cc = clean_addresses("cc", &c.cc, &mut check);
    if mailbox.is_none() && !cc.is_empty() {
        check.issue("cc", "copies need a connected mailbox to send from");
    }
    if mailbox.is_none() && to.len() > 1 {
        check.issue(
            "to",
            "without a connected mailbox, a message goes to one person",
        );
    }
    let patch = ThreadPatch {
        contact_id: c.contact_id.map(Some),
        enquiry_id: c.enquiry_id.map(Some),
        booking_id: c.booking_id.map(Some),
        ..ThreadPatch::default()
    };
    let mut links = Links {
        contact_id: c.contact_id,
        organisation_id: None,
        enquiry_id: c.enquiry_id,
        booking_id: c.booking_id,
    };
    check_links(tx, ctx, &mut check, &patch, &mut links).await?;
    if links.contact_id.is_none()
        && let Some((contact, org)) = core_db::inbox::contact_for(tx, &to).await?
    {
        links.contact_id = Some(contact);
        links.organisation_id = org;
    }
    check.finish()?;
    let attachments =
        check_attachments(tx, ctx, &c.attachments, send_limit(mailbox.as_ref())).await?;
    let mut written = write_body(
        state,
        tx,
        ctx,
        c.body.as_deref(),
        c.template_key.as_deref(),
        &links,
    )
    .await?;
    if c.signature
        && let Some(sig) = core_db::signatures::for_sender(tx, ctx.user_id()).await?
    {
        super::signatures::append(&mut written.text, &mut written.html, &sig);
    }
    let subject = c
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or(written.subject.clone())
        .ok_or_else(|| ApiError::invalid("subject", "is required"))?;
    if subject.chars().count() > 1000 {
        return Err(ApiError::invalid(
            "subject",
            "must be at most 1000 characters",
        ));
    }
    let id = core_domain::ids::new_id();
    core_db::inbox::insert(
        tx,
        NewThread {
            id,
            integration_id: mailbox.as_ref().map(|m| m.id),
            external_id: None,
            subject: Some(&subject),
            contact_id: links.contact_id,
            organisation_id: links.organisation_id,
            enquiry_id: links.enquiry_id,
            booking_id: links.booking_id,
            assigned_to: ctx.user_id().map(|u| u.0),
            unread: false,
            at: Utc::now(),
        },
    )
    .await?;
    let thread = core_db::inbox::get(tx, id, None)
        .await?
        .ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "thread.created",
            resource: ENTITY,
            object_id: Some(id),
            before: None,
            after: Some(event_json(&thread)),
            event_extra: None,
        },
    )
    .await?;
    let message = queue_outbound(
        state,
        tx,
        ctx,
        &thread,
        mailbox.as_ref(),
        Outbound {
            to,
            cc,
            subject,
            written,
            in_reply_to: None,
            references: Vec::new(),
            attachments,
            answer_to: None,
        },
    )
    .await?;
    Ok((id, message))
}

struct Outbound {
    to: Vec<String>,
    cc: Vec<String>,
    subject: String,
    written: Written,
    in_reply_to: Option<String>,
    references: Vec<String>,
    attachments: Vec<Uuid>,
    /// Where answers go when there is no mailbox to send from.
    answer_to: Option<String>,
}

async fn queue_outbound(
    state: &AppState,
    tx: &mut core_db::Tx,
    ctx: &Ctx,
    thread: &Thread,
    mailbox: Option<&Integration>,
    o: Outbound,
) -> ApiResult<MessageView> {
    let author = match ctx.user_id() {
        Some(u) => core_db::users::get(tx, u).await?,
        None => None,
    };
    let from_address = match (mailbox, o.answer_to, &author) {
        (Some(m), _, _) => Some(m.mailbox().to_owned()),
        (None, Some(a), _) => Some(a),
        (None, None, Some(a)) => Some(a.email.to_string()),
        (None, None, None) => None,
    };
    let from_name = mailbox
        .and_then(|m| m.display_name.clone())
        .or_else(|| author.as_ref().map(|a| a.name.clone()));
    let id = core_domain::ids::new_id();
    let message_id = new_message_id(state);
    core_db::inbox::insert_message(
        tx,
        NewMessage {
            id,
            thread_id: thread.id,
            direction: "outbound",
            integration_id: mailbox.map(|m| m.id),
            external_id: None,
            message_id: Some(&message_id),
            in_reply_to: o.in_reply_to.as_deref(),
            references: &o.references,
            from_address: from_address.as_deref(),
            from_name: from_name.as_deref(),
            reply_to: None,
            to_addresses: &o.to,
            cc_addresses: &o.cc,
            subject: Some(&o.subject),
            body_text: &o.written.text,
            body_html: Some(&o.written.html),
            sent_at: Utc::now(),
            status: "queued",
            automatic: false,
            bulk: false,
            author_id: ctx.user_id(),
            template_key: o.written.template_key.as_deref(),
        },
    )
    .await?;
    for f in &o.attachments {
        core_db::inbox::attach_file(tx, id, *f).await?;
        core_db::files::attach(tx, *f).await?;
    }
    core_db::inbox::refresh(tx, thread.id, false).await?;
    let payload = json!({ "message_id": id });
    core_db::jobs::enqueue(
        tx,
        core_db::jobs::NewJob::new(Some(ctx.workspace_id), JOB_SEND, &payload)
            .dedupe(&id.to_string()),
    )
    .await?;
    record(
        tx,
        ctx,
        Change {
            topic: "thread.replied",
            resource: ENTITY,
            object_id: Some(thread.id),
            before: None,
            after: Some(json!({
                "message_id": id,
                "template_key": o.written.template_key,
                "contact_id": thread.contact_id,
                "enquiry_id": thread.enquiry_id,
                "booking_id": thread.booking_id,
            })),
            event_extra: None,
        },
    )
    .await?;
    let t = load(tx, ctx, thread.id).await?;
    let d = detail(state, tx, t).await?;
    d.messages
        .into_iter()
        .find(|m| m.id == id)
        .ok_or(ApiError::NotFound)
}

/// What sending did, for the job's log.
#[derive(Debug)]
pub enum SendOutcome {
    Sent { via: String },
    AlreadyDone,
    Failed(String),
}

impl std::fmt::Display for SendOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sent { via } => write!(f, "sent through {via}"),
            Self::AlreadyDone => f.write_str("already sent or given up on"),
            Self::Failed(why) => write!(f, "not sent: {why}"),
        }
    }
}

/// Sends a message written here (the worker's `inbox.send`): from the
/// thread's mailbox, or through OnHold's own `mailer` when there is none.
/// A failure worth retrying returns an error until `last_attempt`, when the
/// message is marked failed instead.
pub async fn send(
    state: &AppState,
    ws: WorkspaceId,
    message_id: Uuid,
    mailer: &dyn Mailer,
    last_attempt: bool,
) -> ApiResult<SendOutcome> {
    let ctx = Ctx::system(ws);
    let mut tx = begin(state, &ctx).await?;
    let Some(m) = core_db::inbox::lock_message(&mut tx, message_id).await? else {
        return Ok(SendOutcome::AlreadyDone);
    };
    if m.status == "sending" {
        // An earlier attempt handed it over and stopped before recording
        // what happened. Sending again could send it twice; if it did go,
        // reading the mailbox's copy back marks it sent.
        tx.commit().await?;
        return finish(
            state,
            &ctx,
            &m,
            Err(
                "sending was interrupted and it may not have gone out; check the Sent folder \
                 before sending it again"
                    .into(),
            ),
        )
        .await;
    }
    if m.status != "queued" {
        return Ok(SendOutcome::AlreadyDone);
    }
    let thread = core_db::inbox::get(&mut tx, m.thread_id, None)
        .await?
        .ok_or(ApiError::NotFound)?;
    if let Some(channel) = thread.messaging_channel_id {
        return super::messaging::send(state, tx, &ctx, &m, &thread, channel, last_attempt).await;
    }
    let mailbox = match thread.integration_id {
        Some(i) => core_db::integrations::get(&mut tx, i)
            .await?
            .filter(|i| i.disconnected_at.is_none() && i.status != "reconnect"),
        None => None,
    };
    let mut attachments = Vec::new();
    for f in core_db::inbox::files(&mut tx, thread.id)
        .await?
        .into_iter()
        .filter(|f| f.message_id == m.id)
    {
        let file = match f.file_id {
            Some(id) => core_db::files::get(&mut tx, id).await?,
            None => None,
        }
        .ok_or(ApiError::NotFound)?;
        attachments.push(Attachment {
            filename: file.filename.clone(),
            content_type: file.content_type.clone(),
            bytes: super::files::bytes(state, &mut tx, &file).await?,
        });
    }
    let (workspace, _) = super::context::workspace(&mut tx, state).await?;
    let own_address = core_db::sender_domains::sending_address(&mut tx).await?;
    core_db::inbox::start_sending(&mut tx, m.id).await?;
    tx.commit().await?;

    let wrap = |id: &str| format!("<{id}>");
    let html = super::mail::layout(m.body_html.as_deref().unwrap_or_default(), "");
    // Sent: (message id, provider thread id, sent through); failed: (permanent, why).
    type Sent = (Option<String>, Option<String>, String);
    let result: Result<Sent, (bool, String)> = match &mailbox {
        Some(i) => {
            let out = OutgoingMail {
                from: i.mailbox().to_owned(),
                from_name: m.from_name.clone(),
                to: m.to_addresses.clone(),
                cc: m.cc_addresses.clone(),
                subject: m.subject.clone().unwrap_or_default(),
                text: m.body_text.clone(),
                html: Some(html),
                message_id: wrap(m.message_id.as_deref().unwrap_or_default()),
                in_reply_to: m.in_reply_to.as_deref().map(wrap),
                references: m.references.iter().map(|r| wrap(r)).collect(),
                thread_id: thread.external_id.clone(),
                attachments,
            };
            match super::integrations::send_from(state, ws, i, &out).await {
                Ok(sent) => Ok((sent.external_id, sent.thread_id, i.mailbox().to_owned())),
                Err(e @ ProviderError::Revoked(_)) => {
                    let msg = format!("the mailbox needs to be connected again ({e})");
                    super::integrations::mark_revoked(state, ws, i, e).await?;
                    Err((true, msg))
                }
                Err(e) => Err((!e.is_temporary(), e.to_string())),
            }
        }
        None => {
            // OnHold's own sender, in the workspace's name, answering to
            // the member who wrote it.
            let name = workspace
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("OnHold")
                .to_owned();
            // It sends to one person, without copies: a message written
            // for a mailbox that stopped working fails rather than going
            // to only some of the people it names.
            let to = match m.to_addresses.as_slice() {
                [to] if m.cc_addresses.is_empty() => to,
                [] => {
                    return finish(state, &ctx, &m, Err("there is no one to send it to".into()))
                        .await;
                }
                _ => {
                    return finish(
                        state,
                        &ctx,
                        &m,
                        Err(
                            "its mailbox is no longer connected, and without one a message \
                             goes to one person only"
                                .into(),
                        ),
                    )
                    .await;
                }
            };
            let email = Email {
                to: to.clone(),
                from_name: Some(match &m.from_name {
                    Some(n) => format!("{n} at {name}"),
                    None => name.clone(),
                }),
                from_address: own_address.clone(),
                reply_to: m.from_address.clone(),
                subject: m.subject.clone().unwrap_or_default(),
                html: Some(super::mail::layout(
                    m.body_html.as_deref().unwrap_or_default(),
                    &format!("Sent by {name} via OnHold."),
                )),
                text: m.body_text.clone(),
                message_id: wrap(m.message_id.as_deref().unwrap_or_default()),
                attachments,
            };
            match mailer.send(&email).await {
                Ok(_) => Ok((
                    None,
                    None,
                    own_address.unwrap_or_else(|| "OnHold's own sender".into()),
                )),
                Err(e @ MailError::Permanent(_)) => Err((true, e.to_string())),
                Err(e) => Err((false, e.to_string())),
            }
        }
    };
    match result {
        Ok((external, provider_thread, via)) => {
            // It has gone: a failure from here on is logged, never retried,
            // since a retry would send it again.
            let record_sent = async {
                let mut tx = begin(state, &ctx).await?;
                core_db::inbox::set_outcome(&mut tx, m.id, "sent", external.as_deref(), None)
                    .await?;
                if let (Some(pt), Some(i)) = (&provider_thread, thread.integration_id) {
                    core_db::inbox::adopt_external(&mut tx, thread.id, i, pt).await?;
                }
                core_db::inbox::refresh(&mut tx, thread.id, false).await?;
                record(
                    &mut tx,
                    &ctx,
                    Change {
                        topic: "thread.message_sent",
                        resource: ENTITY,
                        object_id: Some(thread.id),
                        before: None,
                        after: Some(json!({ "message_id": m.id })),
                        event_extra: None,
                    },
                )
                .await?;
                tx.commit().await?;
                Ok::<_, ApiError>(())
            };
            if let Err(e) = record_sent.await {
                tracing::error!(message_id = %m.id, error = %e, "a message was sent but could not be recorded as sent");
            }
            Ok(SendOutcome::Sent { via })
        }
        Err((permanent, why)) if permanent || last_attempt => {
            finish(state, &ctx, &m, Err(why)).await
        }
        Err((_, why)) => {
            // Not handed over: back in the queue for the next attempt.
            let mut tx = begin(state, &ctx).await?;
            core_db::inbox::set_outcome(&mut tx, m.id, "queued", None, None).await?;
            tx.commit().await?;
            Err(ApiError::unavailable("mail_unavailable", why))
        }
    }
}

pub(crate) async fn finish(
    state: &AppState,
    ctx: &Ctx,
    m: &core_db::inbox::Message,
    result: Result<(), String>,
) -> ApiResult<SendOutcome> {
    let Err(why) = result else {
        return Ok(SendOutcome::AlreadyDone);
    };
    let mut tx = begin(state, ctx).await?;
    core_db::inbox::set_outcome(&mut tx, m.id, "failed", None, Some(&why)).await?;
    core_db::inbox::refresh(&mut tx, m.thread_id, false).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "thread.message_failed",
            resource: ENTITY,
            object_id: Some(m.thread_id),
            before: None,
            // Why stays on the message: a provider's words can name people.
            after: Some(json!({ "message_id": m.id })),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(SendOutcome::Failed(why))
}

/// Marks a message failed when its job gave up without finishing (it timed
/// out on its last attempt, or its worker stopped).
pub async fn abandon(
    state: &AppState,
    ws: WorkspaceId,
    message_id: Uuid,
    error: &str,
) -> ApiResult<()> {
    let ctx = Ctx::system(ws);
    let mut tx = begin(state, &ctx).await?;
    let m = core_db::inbox::lock_message(&mut tx, message_id).await?;
    tx.commit().await?;
    if let Some(m) = m.filter(|m| m.status == "queued" || m.status == "sending") {
        finish(state, &ctx, &m, Err(format!("sending stopped: {error}"))).await?;
    }
    Ok(())
}

// Reading mail in ----------------------------------------------------------------------

/// Removes everything from a message's HTML that could run, track or
/// reach out: scripts, styles, forms, frames and remote images. Links are
/// kept and open in a new tab without a referrer.
pub fn sanitise(html: &str) -> String {
    Builder::default()
        .url_schemes(["http", "https", "mailto", "tel"].into())
        .attribute_filter(|element, attribute, value| {
            // A remote image tells the sender when, where and that the
            // message was read; only images inside the message are kept.
            if element == "img" && attribute == "src" && !value.starts_with("data:image/") {
                return None;
            }
            Some(value.into())
        })
        .add_url_schemes(["data"])
        .link_rel(Some("noopener noreferrer nofollow"))
        .set_tag_attribute_value("a", "target", "_blank")
        .clean(html)
        .to_string()
}

fn html_to_text(html: &str) -> String {
    html2text::from_read(html.as_bytes(), 10_000).unwrap_or_default()
}

/// The most of a message read from a mailbox that is kept: its text (in
/// characters) and its cleaned HTML (in bytes; longer HTML is dropped).
const MAX_STORED_TEXT: usize = 100_000;
const MAX_STORED_HTML: usize = 256 * 1024;

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn strip_subject(s: &str) -> &str {
    let mut s = s.trim();
    loop {
        let lower = s.to_ascii_lowercase();
        let cut = ["re:", "fw:", "fwd:", "aw:", "sv:"]
            .iter()
            .find(|p| lower.starts_with(*p))
            .map(|p| p.len());
        match cut {
            Some(n) => s = s[n..].trim_start(),
            None => return s,
        }
    }
}

/// What reading one message did.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Ingested {
    /// It was new and kept.
    pub added: bool,
    /// It started a thread.
    pub new_thread: bool,
}

/// Whether this mailbox's message is in the inbox already, so its body
/// need not be read.
pub(crate) async fn holds(
    state: &AppState,
    ws: WorkspaceId,
    integration_id: Uuid,
    external_id: &str,
) -> ApiResult<bool> {
    let ctx = Ctx::system(ws);
    let mut tx = begin(state, &ctx).await?;
    let known = core_db::inbox::known(&mut tx, integration_id, external_id, None).await?;
    tx.commit().await?;
    Ok(known.is_some())
}

/// Reads one message into the inbox.
pub(crate) async fn ingest(
    state: &AppState,
    ws: WorkspaceId,
    i: &Integration,
    m: &FetchedMail,
    raw: &[u8],
) -> ApiResult<Ingested> {
    let Some(parsed) = mailbox::parse(raw) else {
        tracing::warn!(integration_id = %i.id, external_id = %m.external_id, "a message could not be read");
        return Ok(Ingested::default());
    };
    let ctx = Ctx::system(ws);
    let mut tx = begin(state, &ctx).await?;
    // Nothing more is read in once the mailbox is disconnected; a
    // disconnect waits for a message being read in to finish.
    if !core_db::integrations::hold_live(&mut tx, i.id).await? {
        return Ok(Ingested::default());
    }
    if let Some(existing) =
        core_db::inbox::known(&mut tx, i.id, &m.external_id, parsed.message_id.as_deref()).await?
    {
        // A message sent from here, now read back from the mailbox.
        core_db::inbox::set_external(&mut tx, existing, &m.external_id).await?;
        tx.commit().await?;
        return Ok(Ingested::default());
    }
    let own = i.mailbox();
    let from = parsed.from.as_ref().map(|f| f.email.as_str());
    let outbound = m.sent || from == Some(own);
    let person = !(parsed.automatic || parsed.bulk);
    // The other side: the sender of what came in, the recipients of what went out.
    let others: Vec<String> = if outbound {
        parsed
            .to
            .iter()
            .chain(parsed.cc.iter())
            .map(|a| a.email.clone())
            .filter(|a| a != own)
            .collect()
    } else {
        parsed
            .reply_to
            .iter()
            .chain(parsed.from.iter())
            .map(|a| a.email.clone())
            .filter(|a| a != own)
            .collect()
    };

    // The thread: the provider's conversation, else the one the message
    // answers, else a new one.
    let mut thread_id = match &m.thread_id {
        Some(t) => core_db::inbox::by_external(&mut tx, i.id, t).await?,
        None => None,
    };
    if thread_id.is_none() {
        let mut refs = parsed.in_reply_to.clone();
        refs.extend(parsed.references.iter().rev().cloned());
        if !refs.is_empty()
            && let Some(t) = core_db::inbox::by_message_ids(&mut tx, i.id, &refs).await?
        {
            core_db::inbox::adopt(&mut tx, t, i.id, m.thread_id.as_deref()).await?;
            thread_id = Some(t);
        }
    }
    let contact = if others.is_empty() {
        None
    } else {
        core_db::inbox::contact_for(&mut tx, &others).await?
    };
    // Only what the business needs is kept: mail from people, what belongs
    // to a conversation already here, and mail to or from a contact.
    // Mailing lists, auto-replies and the mailbox's other sent mail stay in
    // the mailbox only.
    let keep = thread_id.is_some() || contact.is_some() || (!outbound && person);
    if !keep {
        tx.rollback().await?;
        return Ok(Ingested::default());
    }
    let subject = parsed
        .subject
        .as_deref()
        .map(strip_subject)
        .filter(|s| !s.is_empty());
    let sent_at = parsed
        .date
        .filter(|d| *d <= Utc::now() + Duration::hours(1))
        .unwrap_or_else(Utc::now);
    let created = thread_id.is_none();
    let thread_id = match thread_id {
        Some(t) => {
            // Link a thread that has no contact yet, now that one matches.
            if let Some((c, org)) = contact
                && let Some(t0) = core_db::inbox::get(&mut tx, t, None).await?
                && t0.contact_id.is_none()
            {
                core_db::inbox::set_links(
                    &mut tx,
                    t,
                    &Links {
                        contact_id: Some(c),
                        organisation_id: t0.organisation_id.or(org),
                        enquiry_id: t0.enquiry_id,
                        booking_id: t0.booking_id,
                    },
                )
                .await?;
            }
            t
        }
        None => {
            let id = core_domain::ids::new_id();
            core_db::inbox::insert(
                &mut tx,
                NewThread {
                    id,
                    integration_id: Some(i.id),
                    external_id: m.thread_id.as_deref(),
                    subject,
                    contact_id: contact.map(|c| c.0),
                    organisation_id: contact.and_then(|c| c.1),
                    enquiry_id: None,
                    booking_id: None,
                    assigned_to: None,
                    unread: !outbound && person,
                    at: sent_at,
                },
            )
            .await?;
            id
        }
    };

    // A very long message keeps its text, cut short, and loses its
    // formatting rather than taking up room.
    let html = parsed
        .html
        .as_deref()
        .map(sanitise)
        .filter(|h| h.len() <= MAX_STORED_HTML);
    let text = parsed
        .text
        .clone()
        .or_else(|| parsed.html.as_deref().map(html_to_text))
        .unwrap_or_default();
    let text = truncate(&text, MAX_STORED_TEXT);
    let message_id = core_domain::ids::new_id();
    let addresses = |list: &[mailbox::Address]| -> Vec<String> {
        list.iter().take(100).map(|a| a.email.clone()).collect()
    };
    let inserted = core_db::inbox::insert_message(
        &mut tx,
        NewMessage {
            id: message_id,
            thread_id,
            direction: if outbound { "outbound" } else { "inbound" },
            integration_id: Some(i.id),
            external_id: Some(&m.external_id),
            message_id: parsed.message_id.as_deref(),
            in_reply_to: parsed.in_reply_to.first().map(String::as_str),
            references: &parsed
                .references
                .iter()
                .take(100)
                .cloned()
                .collect::<Vec<_>>(),
            from_address: from,
            from_name: parsed.from.as_ref().and_then(|f| f.name.as_deref()),
            reply_to: parsed
                .reply_to
                .as_ref()
                .map(|a| a.email.as_str())
                .filter(|r| Some(*r) != from),
            to_addresses: &addresses(&parsed.to),
            cc_addresses: &addresses(&parsed.cc),
            subject: parsed.subject.as_deref(),
            body_text: text,
            body_html: html.as_deref(),
            sent_at,
            status: if outbound { "sent" } else { "received" },
            automatic: parsed.automatic,
            bulk: parsed.bulk,
            author_id: None,
            template_key: None,
        },
    )
    .await?;
    if !inserted {
        // Read in by a read running at the same time.
        tx.rollback().await?;
        return Ok(Ingested::default());
    }
    // Attachments stay in the mailbox; shown inside the message (`cid:`)
    // ones are not shown at all.
    for (part, a) in parsed.attachments.iter().enumerate() {
        if a.inline || a.bytes.is_empty() {
            continue;
        }
        let filename = super::files::clean_filename(&a.filename);
        let content_type = super::files::sniff(&a.bytes, Some(&a.content_type), &filename);
        core_db::inbox::attach_part(
            &mut tx,
            message_id,
            part as i32,
            &filename,
            truncate(&content_type, 255),
            a.bytes.len() as i64,
        )
        .await?;
    }
    core_db::inbox::refresh(&mut tx, thread_id, !outbound && person).await?;
    let thread = core_db::inbox::get(&mut tx, thread_id, None)
        .await?
        .ok_or(ApiError::NotFound)?;
    if !outbound {
        record(
            &mut tx,
            &ctx,
            Change {
                topic: "message.received",
                resource: ENTITY,
                object_id: Some(thread_id),
                before: None,
                after: Some(json!({
                    "message_id": message_id,
                    "new_thread": created,
                    "automatic": parsed.automatic,
                    "bulk": parsed.bulk,
                    "integration_id": i.id,
                    "contact_id": thread.contact_id,
                    "organisation_id": thread.organisation_id,
                    "enquiry_id": thread.enquiry_id,
                    "booking_id": thread.booking_id,
                })),
                event_extra: None,
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Ingested {
        added: true,
        new_thread: created,
    })
}

// Turning a thread into an enquiry ------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct ThreadEnquiry {
    /// Default: the thread's subject.
    pub title: Option<String>,
    /// The contact's name when one is made for the sender; default the name
    /// in their email, else their address.
    pub contact_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ThreadEnquiryMade {
    pub enquiry_id: Uuid,
    pub contact_id: Uuid,
    /// A contact was made for the sender.
    pub contact_created: bool,
    pub thread: ThreadView,
}

/// Makes an enquiry from a thread, with a contact for its sender if they
/// are not one yet, and links the thread to both.
pub async fn make_enquiry(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    req: ThreadEnquiry,
) -> ApiResult<ThreadEnquiryMade> {
    ctx.authorize(Permission::InboxWrite)?;
    ctx.authorize(Permission::EnquiryWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = load(&mut tx, ctx, id).await?;
    core_db::inbox::lock(&mut tx, id).await?;
    if before.channel == "portal" {
        return Err(ApiError::conflict(
            "portal_thread",
            "this is a conversation with your own talent, not a client",
        ));
    }
    if let Some(e) = before.enquiry_id {
        return Err(ApiError::conflict(
            "thread_has_enquiry",
            format!("this thread is already linked to enquiry {e}"),
        ));
    }
    let messages = core_db::inbox::messages(&mut tx, id).await?;
    let (contact_id, organisation_id, created) = match before.contact_id {
        Some(c) => (c, before.organisation_id, false),
        None => {
            let sender = messages
                .iter()
                .find(|m| m.direction == "inbound" && !m.automatic)
                .ok_or_else(|| {
                    ApiError::conflict("no_sender", "nobody has written in this thread yet")
                })?;
            let email = sender.from_address.clone().ok_or_else(|| {
                ApiError::conflict("no_sender", "the message has no sender address")
            })?;
            match core_db::inbox::contact_for(&mut tx, std::slice::from_ref(&email)).await? {
                Some((c, org)) => (c, org, false),
                None => {
                    ctx.authorize(Permission::ContactWrite)?;
                    let name = req
                        .contact_name
                        .clone()
                        .map(|n| n.trim().to_owned())
                        .filter(|n| !n.is_empty())
                        .or(sender.from_name.clone())
                        .unwrap_or_else(|| email.clone());
                    let contact = super::contacts::create_in_tx(
                        &mut tx,
                        ctx,
                        super::contacts::NewContact {
                            name: name.chars().take(200).collect(),
                            emails: vec![email],
                            ..Default::default()
                        },
                        None,
                    )
                    .await?;
                    (contact.id, contact.organisation_id, true)
                }
            }
        }
    };
    let title = req
        .title
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .or(before.subject.clone())
        .unwrap_or_else(|| "Email enquiry".into());
    let enquiry = super::enquiries::create_in_tx(
        &mut tx,
        ctx,
        super::enquiries::NewEnquiry {
            title: title.chars().take(200).collect(),
            source: Some("email".into()),
            contact_id: Some(contact_id),
            organisation_id,
            ..Default::default()
        },
    )
    .await?;
    core_db::inbox::set_links(
        &mut tx,
        id,
        &Links {
            contact_id: Some(contact_id),
            organisation_id,
            enquiry_id: Some(enquiry.id),
            booking_id: before.booking_id,
        },
    )
    .await?;
    let after = load(&mut tx, ctx, id).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "thread.updated",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(event_json(&before)),
            after: Some(event_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(ThreadEnquiryMade {
        enquiry_id: enquiry.id,
        contact_id,
        contact_created: created,
        thread: after.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitising_keeps_text_and_links_only() {
        let html = r#"<p onclick="x()">Hi <b>there</b></p><script>alert(1)</script>
            <style>p{color:red}</style><img src="https://track.test/p.gif"><img src="data:image/png;base64,iVBOR">
            <a href="https://grand.test/rider" style="color:red">rider</a><a href="javascript:alert(1)">x</a>
            <form action="https://evil.test"><input name="pw"></form><iframe src="https://evil.test"></iframe>"#;
        let clean = sanitise(html);
        assert!(clean.contains("<b>there</b>"), "{clean}");
        assert!(
            !clean.contains("onclick") && !clean.contains("<script") && !clean.contains("alert"),
            "{clean}"
        );
        assert!(
            !clean.contains("color:red") && !clean.contains("<style"),
            "{clean}"
        );
        assert!(!clean.contains("track.test"), "{clean}");
        assert!(clean.contains("data:image/png"), "{clean}");
        assert!(
            clean.contains(r#"href="https://grand.test/rider""#),
            "{clean}"
        );
        assert!(clean.contains("noopener"), "{clean}");
        assert!(!clean.contains("javascript"), "{clean}");
        assert!(
            !clean.contains("<form") && !clean.contains("<input") && !clean.contains("<iframe"),
            "{clean}"
        );
    }

    #[test]
    fn subjects_lose_their_reply_prefixes() {
        assert_eq!(strip_subject("Re: RE: Fwd: Summer ball"), "Summer ball");
        assert_eq!(strip_subject("Summer ball"), "Summer ball");
        assert_eq!(strip_subject("Re:"), "");
    }

    #[test]
    fn plain_text_becomes_safe_html() {
        assert_eq!(
            text_to_html("Hi <Bea>\nSee you & thanks"),
            "<div style=\"white-space:pre-wrap\">Hi &lt;Bea&gt;<br>See you &amp; thanks</div>"
        );
    }
}
