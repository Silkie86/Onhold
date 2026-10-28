//! Text messages. A workspace connects its own SMS number (a Twilio
//! account) and its own WhatsApp number (Meta's WhatsApp Cloud API);
//! conversations on them are inbox threads, one per number they are with.
//!
//! Texts written here are queued like mail and sent by the worker
//! ([`send`], through the inbox's `inbox.send` job). The providers post
//! what comes in, and what became of texts sent, to the number's own
//! address under `/hooks/messaging/`: each request is checked against the
//! provider's signature before anything in it is used.
//!
//! Someone who texts STOP gets nothing more on that number until they text
//! START. Rules text only contacts who agreed to texts (`sms_opt_in`,
//! `whatsapp_opt_in`). WhatsApp allows free text only within 24 hours of
//! the person's last message; outside that, a template approved in
//! WhatsApp Manager is sent. Pictures and files sent in are noted, not
//! kept.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, TimeZone, Utc};
use core_db::Tx;
use core_db::inbox::{Links, Thread};
use core_db::messaging::{Channel, NewChannel, NewText, NewThread};
use core_domain::{Ctx, Permission, WorkspaceId};
use core_engines::templates::Kind as TemplateKind;
use core_integrations::messaging::{
    self as texts, MAX_SMS, MAX_WHATSAPP, Received, Twilio, WhatsApp,
};
use core_integrations::provider::ProviderError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::inbox::{self, MessageView, Reply, SendOutcome, ThreadDetail};
use super::{Change, begin, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

pub const ENTITY: &str = "messaging_channel";
const KINDS: &[&str] = &["sms", "whatsapp"];
/// How long after someone's last WhatsApp message free text may be sent.
const WHATSAPP_WINDOW: Duration = Duration::hours(24);

// Credentials -------------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "lowercase")]
enum Secret {
    Twilio {
        auth_token: String,
    },
    Meta {
        access_token: String,
        app_secret: String,
    },
}

fn aad(id: Uuid) -> Vec<u8> {
    [b"messaging:".as_slice(), id.as_bytes()].concat()
}

fn seal(state: &AppState, id: Uuid, secret: &Secret) -> ApiResult<Vec<u8>> {
    let json = serde_json::to_vec(secret).map_err(ApiError::internal)?;
    Ok(state.config.keyring.encrypt(&json, &aad(id)))
}

fn open(state: &AppState, c: &Channel) -> ApiResult<Secret> {
    let plain = state
        .config
        .keyring
        .decrypt(&c.secret, &aad(c.id))
        .map_err(ApiError::internal)?;
    serde_json::from_slice(&plain).map_err(ApiError::internal)
}

/// Where the provider sends what comes in on a number.
fn hook_url(state: &AppState, c: &Channel) -> String {
    format!("{}{}", state.config.messaging.hook_base, c.hook_token)
}

// Views -------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MessagingChannel {
    pub id: Uuid,
    /// `sms` or `whatsapp`.
    pub kind: String,
    /// `twilio` or `meta`.
    pub provider: String,
    /// The number, in international form.
    pub sender: String,
    /// Twilio's account SID; Meta's phone number id.
    pub account_id: String,
    /// WhatsApp's verified name for the number.
    pub display_name: Option<String>,
    /// `active`, or `failing` when the provider refused the account itself.
    pub status: String,
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub connected_at: DateTime<Utc>,
    /// For setting up the provider (members who manage integrations
    /// only): where it sends messages. Twilio: the number's "A message
    /// comes in" webhook. Meta: the app's callback URL.
    pub webhook_url: Option<String>,
    /// Meta's "Verify token" for that callback URL.
    pub verify_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Messaging {
    pub channels: Vec<MessagingChannel>,
}

fn view(state: &AppState, c: &Channel, manage: bool) -> MessagingChannel {
    MessagingChannel {
        id: c.id,
        kind: c.kind.clone(),
        provider: c.provider.clone(),
        sender: c.sender.clone(),
        account_id: c.account_id.clone(),
        display_name: c.display_name.clone(),
        status: c.status.clone(),
        last_error: c.last_error.clone(),
        last_error_at: c.last_error_at,
        connected_at: c.connected_at,
        webhook_url: manage.then(|| hook_url(state, c)),
        verify_token: (manage && c.provider == "meta").then(|| c.hook_token.clone()),
    }
}

fn audit_json(c: &Channel) -> Value {
    json!({
        "kind": c.kind,
        "provider": c.provider,
        "sender": c.sender,
        "account_id": c.account_id,
        "display_name": c.display_name,
    })
}

/// The workspace's numbers.
pub async fn list(state: &AppState, ctx: &Ctx) -> ApiResult<Messaging> {
    ctx.authorize(Permission::InboxRead)?;
    let manage = ctx.permissions.contains(&Permission::IntegrationManage);
    let mut tx = begin(state, ctx).await?;
    let channels = core_db::messaging::live(&mut tx).await?;
    tx.commit().await?;
    Ok(Messaging {
        channels: channels.iter().map(|c| view(state, c, manage)).collect(),
    })
}

// Connecting --------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ConnectSms {
    /// The Twilio account's SID (`AC…`).
    pub account_sid: String,
    pub auth_token: String,
    /// A number in that account, in international form (`+447700900123`).
    pub number: String,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ConnectWhatsapp {
    /// The number's "Phone number ID" in WhatsApp Manager.
    pub phone_number_id: String,
    /// A system user's permanent access token with `whatsapp_business_messaging`.
    pub access_token: String,
    /// The Meta app's secret, which signs what Meta sends.
    pub app_secret: String,
}

fn required(field: &str, v: &str, max: usize) -> ApiResult<String> {
    let v = v.trim();
    if v.is_empty() || v.len() > max {
        return Err(ApiError::invalid(
            field,
            format!("must be 1 to {max} characters"),
        ));
    }
    Ok(v.to_owned())
}

/// What the provider said about credentials being checked.
fn refused_check(field: &'static str, e: ProviderError) -> ApiError {
    match e {
        ProviderError::Unauthorized | ProviderError::Revoked(_) => {
            ApiError::invalid(field, "the provider did not accept these details")
        }
        ProviderError::Refused { message, .. } => ApiError::invalid(field, message),
        e if e.is_temporary() => ApiError::unavailable(
            "provider_unavailable",
            format!("the provider could not be reached ({e}); try again"),
        ),
        e => ApiError::invalid(field, e.to_string()),
    }
}

async fn not_connected(state: &AppState, ctx: &Ctx, kind: &str) -> ApiResult<()> {
    let mut tx = begin(state, ctx).await?;
    let live = core_db::messaging::live_kind(&mut tx, kind).await?;
    tx.commit().await?;
    match live {
        Some(c) => Err(ApiError::conflict(
            "already_connected",
            format!("{} is connected already; disconnect it first", c.sender),
        )),
        None => Ok(()),
    }
}

async fn save(
    state: &AppState,
    ctx: &Ctx,
    new: NewChannel<'_>,
    secret: &Secret,
) -> ApiResult<MessagingChannel> {
    let sealed = seal(state, new.id, secret)?;
    let id = new.id;
    let mut tx = begin(state, ctx).await?;
    if core_db::messaging::live_kind(&mut tx, new.kind)
        .await?
        .is_some()
    {
        return Err(ApiError::conflict(
            "already_connected",
            "a number is connected already; disconnect it first",
        ));
    }
    core_db::messaging::insert(
        &mut tx,
        NewChannel {
            secret: &sealed,
            ..new
        },
    )
    .await?;
    let c = core_db::messaging::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // The same number connected again (a new token, say) carries on its
    // conversations: they move to it, so replies and new texts join them.
    core_db::messaging::adopt_threads(&mut tx, id, &c.kind, &c.sender).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "messaging.connected",
            resource: ENTITY,
            object_id: Some(id),
            before: None,
            after: Some(audit_json(&c)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(view(state, &c, true))
}

/// Connects the workspace's Twilio number, checked with Twilio first.
pub async fn connect_sms(
    state: &AppState,
    ctx: &Ctx,
    req: ConnectSms,
) -> ApiResult<MessagingChannel> {
    ctx.authorize(Permission::IntegrationManage)?;
    let sid = required("account_sid", &req.account_sid, 64)?;
    if !(sid.starts_with("AC") && sid.bytes().all(|b| b.is_ascii_alphanumeric())) {
        return Err(ApiError::invalid(
            "account_sid",
            "Twilio's account SID starts with AC",
        ));
    }
    let token = required("auth_token", &req.auth_token, 200)?;
    let number = texts::e164(&req.number)
        .filter(|_| req.number.trim().starts_with('+'))
        .ok_or_else(|| {
            ApiError::invalid("number", "give the number with its country code, +44…")
        })?;
    not_connected(state, ctx, "sms").await?;
    let m = &state.config.messaging;
    Twilio::new(m.http.clone(), m.twilio_api.clone(), &sid, &token)
        .check(&number)
        .await
        .map_err(|e| refused_check("auth_token", e))?;
    let id = core_domain::ids::new_id();
    let hook = core_domain::crypto::new_token();
    save(
        state,
        ctx,
        NewChannel {
            id,
            kind: "sms",
            provider: "twilio",
            sender: &number,
            account_id: &sid,
            display_name: None,
            secret: &[],
            hook_token: &hook,
            connected_by: ctx.user_id(),
        },
        &Secret::Twilio { auth_token: token },
    )
    .await
}

/// Connects the workspace's WhatsApp number, checked with Meta first.
pub async fn connect_whatsapp(
    state: &AppState,
    ctx: &Ctx,
    req: ConnectWhatsapp,
) -> ApiResult<MessagingChannel> {
    ctx.authorize(Permission::IntegrationManage)?;
    let number_id = required("phone_number_id", &req.phone_number_id, 64)?;
    if !number_id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ApiError::invalid(
            "phone_number_id",
            "the phone number ID is all digits",
        ));
    }
    let token = required("access_token", &req.access_token, 1000)?;
    let app_secret = required("app_secret", &req.app_secret, 200)?;
    not_connected(state, ctx, "whatsapp").await?;
    let m = &state.config.messaging;
    let number = WhatsApp::new(m.http.clone(), m.meta_api.clone(), &number_id, &token)
        .check()
        .await
        .map_err(|e| refused_check("access_token", e))?;
    let id = core_domain::ids::new_id();
    let hook = core_domain::crypto::new_token();
    save(
        state,
        ctx,
        NewChannel {
            id,
            kind: "whatsapp",
            provider: "meta",
            sender: &number.number,
            account_id: &number_id,
            display_name: number.name.as_deref(),
            secret: &[],
            hook_token: &hook,
            connected_by: ctx.user_id(),
        },
        &Secret::Meta {
            access_token: token,
            app_secret,
        },
    )
    .await
}

/// Stops using a number and forgets its credentials. Its conversations stay.
pub async fn disconnect(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::IntegrationManage)?;
    let mut tx = begin(state, ctx).await?;
    let c = core_db::messaging::get(&mut tx, id)
        .await?
        .filter(|c| c.disconnected_at.is_none())
        .ok_or(ApiError::NotFound)?;
    disconnect_in(&mut tx, ctx, &c, None).await?;
    tx.commit().await?;
    Ok(())
}

async fn disconnect_in(tx: &mut Tx, ctx: &Ctx, c: &Channel, why: Option<&str>) -> ApiResult<()> {
    if core_db::messaging::disconnect(tx, c.id).await? {
        record(
            tx,
            ctx,
            Change {
                topic: "messaging.disconnected",
                resource: ENTITY,
                object_id: Some(c.id),
                before: Some(audit_json(c)),
                after: None,
                event_extra: why.map(|r| json!({ "reason": r })),
            },
        )
        .await?;
    }
    Ok(())
}

/// Disconnects a closing workspace's numbers.
pub(crate) async fn disconnect_on_close(tx: &mut Tx, ctx: &Ctx) -> ApiResult<()> {
    for c in core_db::messaging::live(tx).await? {
        disconnect_in(tx, ctx, &c, Some("workspace_closed")).await?;
    }
    Ok(())
}

// Writing -----------------------------------------------------------------------------------

/// What a text says: free text, or (WhatsApp) an approved template.
struct Text {
    body: String,
    /// The WhatsApp template's name, when it is one.
    template_name: Option<String>,
    template_key: Option<String>,
}

fn check_length(kind: &str, body: &str) -> ApiResult<()> {
    let max = if kind == "sms" { MAX_SMS } else { MAX_WHATSAPP };
    let n = body.chars().count();
    if n == 0 {
        return Err(ApiError::invalid("body", "write a message"));
    }
    if n > max {
        return Err(ApiError::invalid(
            "body",
            format!("a text may be at most {max} characters"),
        ));
    }
    Ok(())
}

/// A text from a template of the number's kind, filled in with `tree`.
/// For WhatsApp, the template's subject is the name of the template
/// approved in WhatsApp Manager (optionally followed by its language,
/// `booking_reminder en_GB`), and each line of its body fills in one of
/// that template's placeholders, in order.
async fn from_template(
    tx: &mut Tx,
    kind: &str,
    key: &str,
    tree: &Value,
    options: &core_engines::templates::Options,
) -> ApiResult<Text> {
    let template = core_db::templates::find(tx, key, &options.locale)
        .await?
        .filter(|t| t.kind == kind)
        .ok_or_else(|| {
            ApiError::invalid(
                "template_key",
                format!("there is no {kind} template `{key}`"),
            )
        })?;
    let engine_kind = if kind == "sms" {
        TemplateKind::Sms
    } else {
        TemplateKind::Whatsapp
    };
    if kind != "whatsapp" {
        let rendered =
            super::templates::render_in(tx, engine_kind, None, &template.body, tree, options)
                .await?;
        return Ok(Text {
            body: rendered.text.trim().to_owned(),
            template_name: None,
            template_key: Some(key.to_owned()),
        });
    }
    let name = template
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            ApiError::invalid(
                "template_key",
                "a WhatsApp template's subject names the template approved in WhatsApp Manager",
            )
        })?;
    // The language of the template found, made specific by the workspace's
    // locale when that is the same language (`en` and `en-GB` → `en_GB`).
    let same_language = options.locale.split('-').next() == template.locale.split('-').next();
    let locale = if same_language {
        &options.locale
    } else {
        &template.locale
    };
    let (name, lang) = template_language(name, locale);
    // Each line of the template fills one value, rendered on its own so an
    // empty value or one with line breaks keeps every value in its place.
    let mut values = Vec::new();
    for line in template.body.lines().filter(|l| !l.trim().is_empty()) {
        let rendered =
            super::templates::render_in(tx, engine_kind, None, line, tree, options).await?;
        let value = rendered
            .text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        values.push(if value.is_empty() {
            "-".to_owned()
        } else {
            value
        });
    }
    let body = values.join("\n");
    let template_name = Some(format!("{name} {lang}"));
    Ok(Text {
        body,
        template_name,
        template_key: Some(key.to_owned()),
    })
}

/// Meta's name for the language a template is in: `name en_GB`, else the
/// workspace's locale (`en-GB` → `en_GB`, `fr-FR` → `fr`).
fn template_language(subject: &str, locale: &str) -> (String, String) {
    let mut parts = subject.split_whitespace();
    let name = parts.next().unwrap_or_default().to_owned();
    if let Some(lang) = parts.next() {
        return (name, lang.to_owned());
    }
    let lang = match locale {
        "en-GB" | "en-US" | "pt-BR" | "pt-PT" | "zh-CN" | "zh-HK" | "zh-TW" | "es-AR" | "es-ES"
        | "es-MX" => locale.replace('-', "_"),
        other => other.split(['-', '_']).next().unwrap_or("en").to_owned(),
    };
    (name, lang)
}

/// Queues a text on a thread; the worker sends it.
#[allow(clippy::too_many_arguments)]
async fn queue(
    tx: &mut Tx,
    ctx: &Ctx,
    channel: &Channel,
    thread: &Thread,
    phone: &str,
    text: Text,
    automatic: bool,
) -> ApiResult<Uuid> {
    if core_db::messaging::opted_out(tx, &channel.kind, phone).await? {
        return Err(ApiError::conflict(
            "opted_out",
            "they texted STOP to this number; nothing more can be sent until they text START",
        ));
    }
    if text.template_name.is_none() {
        check_length(&channel.kind, &text.body)?;
        if channel.kind == "whatsapp"
            && thread
                .last_inbound_at
                .is_none_or(|at| Utc::now() - at > WHATSAPP_WINDOW)
        {
            return Err(ApiError::conflict(
                "whatsapp_window_closed",
                "WhatsApp allows free text only within 24 hours of their last message; send an \
                 approved template instead",
            ));
        }
    }
    let author = match ctx.user_id() {
        Some(u) => core_db::users::get(tx, u).await?.map(|u| u.name),
        None => None,
    };
    let id = core_domain::ids::new_id();
    core_db::messaging::insert_text(
        tx,
        NewText {
            id,
            thread_id: thread.id,
            channel: channel.id,
            direction: "outbound",
            external_id: None,
            from: None,
            from_name: author.as_deref(),
            to: Some(phone),
            subject: text.template_name.as_deref(),
            body: &text.body,
            sent_at: Utc::now(),
            status: "queued",
            automatic,
            author_id: ctx.user_id(),
            template_key: text.template_key.as_deref(),
        },
    )
    .await?;
    core_db::inbox::refresh(tx, thread.id, false).await?;
    let payload = json!({ "message_id": id });
    core_db::jobs::enqueue(
        tx,
        core_db::jobs::NewJob::new(Some(ctx.workspace_id), inbox::JOB_SEND, &payload)
            .dedupe(&id.to_string()),
    )
    .await?;
    record(
        tx,
        ctx,
        Change {
            topic: "thread.replied",
            resource: inbox::ENTITY,
            object_id: Some(thread.id),
            before: None,
            after: Some(json!({
                "message_id": id,
                "channel": channel.kind,
                "template_key": text.template_key,
                "contact_id": thread.contact_id,
                "enquiry_id": thread.enquiry_id,
                "booking_id": thread.booking_id,
            })),
            event_extra: None,
        },
    )
    .await?;
    Ok(id)
}

/// A live number of the thread's, for writing on it.
async fn writable(tx: &mut Tx, channel: Uuid) -> ApiResult<Channel> {
    core_db::messaging::get(tx, channel)
        .await?
        .filter(|c| c.disconnected_at.is_none())
        .ok_or_else(|| {
            ApiError::conflict(
                "not_connected",
                "this number is no longer connected; connect it again to reply",
            )
        })
}

/// A reply in a text thread (from [`inbox::reply`]).
pub(crate) async fn reply(
    state: &AppState,
    tx: &mut Tx,
    ctx: &Ctx,
    thread: &Thread,
    channel: Uuid,
    r: Reply,
) -> ApiResult<MessageView> {
    if !r.attachments.is_empty() {
        return Err(ApiError::invalid("attachments", "texts carry no files"));
    }
    if !r.cc.is_empty() || r.to.as_ref().is_some_and(|t| !t.is_empty()) {
        return Err(ApiError::invalid(
            "to",
            "a text goes to the number the conversation is with",
        ));
    }
    let channel = writable(tx, channel).await?;
    let phone = thread.external_id.clone().ok_or(ApiError::NotFound)?;
    let text = write(
        state,
        tx,
        ctx,
        &channel.kind,
        r.body.as_deref(),
        r.template_key.as_deref(),
        &links(thread),
    )
    .await?;
    let id = queue(tx, ctx, &channel, thread, &phone, text, false).await?;
    let t = inbox::load(tx, ctx, thread.id).await?;
    let d = inbox::detail(state, tx, t).await?;
    d.messages
        .into_iter()
        .find(|m| m.id == id)
        .ok_or(ApiError::NotFound)
}

fn links(t: &Thread) -> Links {
    Links {
        contact_id: t.contact_id,
        organisation_id: t.organisation_id,
        enquiry_id: t.enquiry_id,
        booking_id: t.booking_id,
    }
}

/// What a member wrote: a message, or a template.
async fn write(
    state: &AppState,
    tx: &mut Tx,
    ctx: &Ctx,
    kind: &str,
    body: Option<&str>,
    template_key: Option<&str>,
    about: &Links,
) -> ApiResult<Text> {
    match (body.map(str::trim).filter(|b| !b.is_empty()), template_key) {
        (Some(_), Some(_)) => Err(ApiError::invalid(
            "body",
            "give the message or a template, not both",
        )),
        (None, None) => Err(ApiError::invalid(
            "body",
            "write a message or choose a template",
        )),
        (Some(b), None) => Ok(Text {
            body: b.to_owned(),
            template_name: None,
            template_key: None,
        }),
        (None, Some(key)) => {
            let (tree, options) = inbox::template_context(state, tx, ctx, about).await?;
            from_template(tx, kind, key, &tree, &options).await
        }
    }
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct StartText {
    /// `sms` or `whatsapp`.
    pub channel: String,
    /// Who to text: a contact (their first number that can take texts) or
    /// a number with its country code.
    pub contact_id: Option<Uuid>,
    pub to: Option<String>,
    pub body: Option<String>,
    pub template_key: Option<String>,
    pub enquiry_id: Option<Uuid>,
    pub booking_id: Option<Uuid>,
}

/// The contact's first number that can take texts.
async fn contact_number(tx: &mut Tx, contact: Uuid) -> ApiResult<Option<(String, Contact)>> {
    let Some(c) = core_db::contacts::get(tx, contact).await? else {
        return Ok(None);
    };
    let country = core_db::messaging::country(tx).await?;
    let number = c
        .phones
        .iter()
        .find_map(|p| texts::international(p, &country));
    Ok(number.map(|n| {
        (
            n,
            Contact {
                id: c.id,
                organisation_id: c.organisation_id,
                sms_opt_in: c.sms_opt_in,
                whatsapp_opt_in: c.whatsapp_opt_in,
            },
        )
    }))
}

struct Contact {
    id: Uuid,
    organisation_id: Option<Uuid>,
    sms_opt_in: bool,
    whatsapp_opt_in: bool,
}

/// The thread with `phone` on this number, started if need be.
async fn thread_with(
    tx: &mut Tx,
    channel: &Channel,
    phone: &str,
    about: &Links,
    assigned_to: Option<Uuid>,
) -> ApiResult<Thread> {
    let id = match core_db::messaging::thread_for(tx, channel.id, phone).await? {
        Some(id) => id,
        None => {
            let id = core_domain::ids::new_id();
            core_db::messaging::insert_thread(
                tx,
                NewThread {
                    id,
                    kind: &channel.kind,
                    channel: channel.id,
                    phone,
                    contact_id: about.contact_id,
                    organisation_id: about.organisation_id,
                    enquiry_id: about.enquiry_id,
                    booking_id: about.booking_id,
                    assigned_to,
                    unread: false,
                    at: Utc::now(),
                },
            )
            .await?;
            core_db::messaging::thread_for(tx, channel.id, phone)
                .await?
                .ok_or(ApiError::NotFound)?
        }
    };
    core_db::inbox::get(tx, id, None)
        .await?
        .ok_or(ApiError::NotFound)
}

/// Texts someone from the workspace's number: the conversation with them
/// on that number, started if need be.
pub async fn start(state: &AppState, ctx: &Ctx, s: StartText) -> ApiResult<ThreadDetail> {
    ctx.authorize(Permission::InboxWrite)?;
    if !KINDS.contains(&s.channel.as_str()) {
        return Err(ApiError::invalid("channel", "must be sms or whatsapp"));
    }
    let mut tx = begin(state, ctx).await?;
    let channel = core_db::messaging::live_kind(&mut tx, &s.channel)
        .await?
        .ok_or_else(|| {
            ApiError::conflict(
                "not_connected",
                format!("connect a {} number first", s.channel),
            )
        })?;
    let mut about = Links {
        contact_id: None,
        organisation_id: None,
        enquiry_id: s.enquiry_id,
        booking_id: s.booking_id,
    };
    let phone = match (s.contact_id, s.to.as_deref()) {
        (Some(_), Some(_)) => {
            return Err(ApiError::invalid(
                "to",
                "give a contact or a number, not both",
            ));
        }
        (Some(c), None) => {
            ctx.authorize(Permission::ContactRead)?;
            let (phone, contact) = contact_number(&mut tx, c).await?.ok_or_else(|| {
                ApiError::invalid(
                    "contact_id",
                    "that contact has no number that can take texts",
                )
            })?;
            about.contact_id = Some(contact.id);
            about.organisation_id = contact.organisation_id;
            phone
        }
        (None, Some(to)) => {
            let phone = texts::e164(to)
                .filter(|_| to.trim().starts_with('+'))
                .ok_or_else(|| {
                    ApiError::invalid("to", "give the number with its country code, +44…")
                })?;
            if let Some(key) = core_domain::crm::phone_key(&phone)
                && let Some((c, org)) =
                    core_db::messaging::contact_with_phone(&mut tx, &key).await?
            {
                about.contact_id = Some(c);
                about.organisation_id = org;
            }
            phone
        }
        (None, None) => return Err(ApiError::invalid("to", "say who to text")),
    };
    if let Some(e) = about.enquiry_id {
        ctx.authorize(Permission::EnquiryRead)?;
        core_db::enquiries::get(&mut tx, e)
            .await?
            .ok_or_else(|| ApiError::invalid("enquiry_id", "no such enquiry"))?;
    }
    if let Some(b) = about.booking_id {
        ctx.authorize(Permission::BookingRead)?;
        core_db::bookings::get(&mut tx, b)
            .await?
            .ok_or_else(|| ApiError::invalid("booking_id", "no such booking"))?;
    }
    let text = write(
        state,
        &mut tx,
        ctx,
        &channel.kind,
        s.body.as_deref(),
        s.template_key.as_deref(),
        &about,
    )
    .await?;
    core_db::messaging::lock_number(&mut tx, channel.id, &phone).await?;
    let thread = thread_with(
        &mut tx,
        &channel,
        &phone,
        &about,
        ctx.user_id().map(|u| u.0),
    )
    .await?;
    queue(&mut tx, ctx, &channel, &thread, &phone, text, false).await?;
    let t = inbox::load(&mut tx, ctx, thread.id).await?;
    let d = inbox::detail(state, &mut tx, t).await?;
    tx.commit().await?;
    Ok(d)
}

/// A rule's text to a contact: from a template, on the workspace's number
/// of that kind, only if they agreed to texts and did not text STOP.
pub(crate) async fn from_rule(
    state: &AppState,
    tx: &mut Tx,
    ctx: &Ctx,
    kind: &str,
    template: &str,
    contact: Uuid,
    tree: &Value,
) -> ApiResult<Value> {
    let channel = core_db::messaging::live_kind(tx, kind)
        .await?
        .ok_or_else(|| {
            ApiError::conflict("not_connected", format!("no {kind} number is connected"))
        })?;
    let (phone, c) = contact_number(tx, contact).await?.ok_or_else(|| {
        ApiError::conflict("no_number", "the contact has no number that can take texts")
    })?;
    let agreed = if kind == "sms" {
        c.sms_opt_in
    } else {
        c.whatsapp_opt_in
    };
    if !agreed {
        return Err(ApiError::conflict(
            "not_opted_in",
            format!("the contact has not agreed to {kind} messages"),
        ));
    }
    let (_, options) = super::context::workspace(tx, state).await?;
    let text = from_template(tx, kind, template, tree, &options).await?;
    let about = Links {
        contact_id: Some(c.id),
        organisation_id: c.organisation_id,
        enquiry_id: None,
        booking_id: None,
    };
    core_db::messaging::lock_number(tx, channel.id, &phone).await?;
    let thread = thread_with(tx, &channel, &phone, &about, None).await?;
    let id = queue(tx, ctx, &channel, &thread, &phone, text, true).await?;
    Ok(
        json!({ "template": template, "contact_id": contact, "message_id": id, "thread_id": thread.id }),
    )
}

// Sending -----------------------------------------------------------------------------------

/// Sends a text written here (from [`inbox::send`], which has locked it).
pub(crate) async fn send(
    state: &AppState,
    mut tx: Tx,
    ctx: &Ctx,
    m: &core_db::inbox::Message,
    thread: &Thread,
    channel: Uuid,
    last_attempt: bool,
) -> ApiResult<SendOutcome> {
    let c = core_db::messaging::get(&mut tx, channel).await?;
    let to = m.to_addresses.first().cloned();
    let (Some(c), Some(to)) = (c.filter(|c| c.disconnected_at.is_none()), to) else {
        tx.commit().await?;
        return inbox::finish(
            state,
            ctx,
            m,
            Err("the number is no longer connected".into()),
        )
        .await;
    };
    if core_db::messaging::opted_out(&mut tx, &c.kind, &to).await? {
        tx.commit().await?;
        return inbox::finish(state, ctx, m, Err("they texted STOP to this number".into())).await;
    }
    let secret = open(state, &c)?;
    let (_, options) = super::context::workspace(&mut tx, state).await?;
    core_db::inbox::start_sending(&mut tx, m.id).await?;
    tx.commit().await?;

    let cfg = &state.config.messaging;
    let result = match secret {
        Secret::Twilio { auth_token } => {
            Twilio::new(
                cfg.http.clone(),
                cfg.twilio_api.clone(),
                &c.account_id,
                &auth_token,
            )
            .send(&c.sender, &to, &m.body_text, &hook_url(state, &c))
            .await
        }
        Secret::Meta { access_token, .. } => {
            let w = WhatsApp::new(
                cfg.http.clone(),
                cfg.meta_api.clone(),
                &c.account_id,
                &access_token,
            );
            match m.subject.as_deref() {
                Some(subject) => {
                    let (name, lang) = template_language(subject, &options.locale);
                    let params: Vec<String> = m
                        .body_text
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(str::to_owned)
                        .collect();
                    w.send_template(&to, &name, &lang, &params).await
                }
                None => w.send_text(&to, &m.body_text).await,
            }
        }
    };
    match result {
        Ok(sent) => {
            let record_sent = async {
                let mut tx = begin(state, ctx).await?;
                core_db::inbox::set_outcome(&mut tx, m.id, "sent", Some(&sent.id), None).await?;
                core_db::messaging::set_health(&mut tx, c.id, None).await?;
                core_db::inbox::refresh(&mut tx, thread.id, false).await?;
                record(
                    &mut tx,
                    ctx,
                    Change {
                        topic: "thread.message_sent",
                        resource: inbox::ENTITY,
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
                tracing::error!(message_id = %m.id, error = %e, "a text was sent but could not be recorded as sent");
            }
            Ok(SendOutcome::Sent {
                via: c.sender.clone(),
            })
        }
        Err(e) => {
            let why = e.to_string();
            let account = matches!(e, ProviderError::Unauthorized | ProviderError::Revoked(_));
            if account {
                let mut tx = begin(state, ctx).await?;
                core_db::messaging::set_health(
                    &mut tx,
                    c.id,
                    Some("the provider no longer accepts this number's credentials"),
                )
                .await?;
                tx.commit().await?;
            }
            // Twilio's 21610: they texted STOP to Twilio directly.
            if why.contains("unsubscribed") {
                let mut tx = begin(state, ctx).await?;
                core_db::messaging::set_opt_out(&mut tx, &c.kind, &to, true).await?;
                tx.commit().await?;
            }
            match e {
                // Never handed over: back in the queue.
                ProviderError::RateLimited { .. } if !last_attempt => {
                    let mut tx = begin(state, ctx).await?;
                    core_db::inbox::set_outcome(&mut tx, m.id, "queued", None, None).await?;
                    tx.commit().await?;
                    Err(ApiError::unavailable("provider_busy", why))
                }
                // May have been handed over: not sent twice.
                ProviderError::Unavailable(_) => {
                    inbox::finish(
                        state,
                        ctx,
                        m,
                        Err(format!(
                            "{why}; it may have been sent, check before sending again"
                        )),
                    )
                    .await
                }
                _ => inbox::finish(state, ctx, m, Err(why)).await,
            }
        }
    }
}

// What the providers send -------------------------------------------------------------------

/// The live number a provider request is for, with its credentials.
async fn hooked(state: &AppState, token: &str) -> ApiResult<(WorkspaceId, Channel, Secret)> {
    if token.len() > 100 {
        return Err(ApiError::NotFound);
    }
    let mut tx = core_db::begin(&state.db, None, None).await?;
    let Some((ws, id)) = core_db::messaging::for_hook(&mut tx, token).await? else {
        tx.commit().await?;
        return Err(ApiError::NotFound);
    };
    core_db::set_context(&mut tx, Some(ws), None).await?;
    let c = core_db::messaging::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    let secret = open(state, &c)?;
    Ok((ws, c, secret))
}

fn bad_signature() -> ApiError {
    ApiError::forbidden(
        "signature_invalid",
        "the request is not signed by this number's provider",
    )
}

/// What a provider posts for a number: Twilio's form (a text in, or what
/// became of one sent), checked with `X-Twilio-Signature`, or Meta's JSON,
/// checked with `X-Hub-Signature-256`. Twilio is answered with an empty
/// TwiML document (`Some`), so it sends no reply of its own.
pub async fn hook(
    state: &AppState,
    token: &str,
    twilio_signature: Option<&str>,
    meta_signature: Option<&str>,
    body: &[u8],
) -> ApiResult<Option<&'static str>> {
    let (ws, c, secret) = hooked(state, token).await?;
    match secret {
        Secret::Twilio { auth_token } => {
            let form: BTreeMap<String, String> = url::form_urlencoded::parse(body)
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            let url = hook_url(state, &c);
            if !twilio_signature.is_some_and(|s| texts::twilio_signed(&auth_token, &url, &form, s))
            {
                return Err(bad_signature());
            }
            if form.get("AccountSid") == Some(&c.account_id) {
                receive(state, ws, &c, texts::twilio_received(&form)).await?;
            }
            Ok(Some(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Response></Response>",
            ))
        }
        Secret::Meta { app_secret, .. } => {
            if !meta_signature.is_some_and(|s| texts::meta_signed(&app_secret, body, s)) {
                return Err(bad_signature());
            }
            let v: Value = serde_json::from_slice(body)
                .map_err(|_| ApiError::bad_request("body_invalid", "the body is not JSON"))?;
            receive(state, ws, &c, texts::meta_received(&v, &c.account_id)).await?;
            Ok(None)
        }
    }
}

/// Meta's check of the callback URL: its challenge, echoed back when the
/// verify token is this number's.
pub async fn meta_verify(
    state: &AppState,
    token: &str,
    mode: Option<&str>,
    verify_token: Option<&str>,
    challenge: Option<&str>,
) -> ApiResult<String> {
    let (_, c, _) = hooked(state, token).await?;
    match (mode, verify_token, challenge) {
        (Some("subscribe"), Some(v), Some(ch)) if v == c.hook_token && ch.len() <= 200 => {
            Ok(ch.to_owned())
        }
        _ => Err(bad_signature()),
    }
}

/// Takes in what a provider sent: texts into their threads (STOP and START
/// noted), and failures of texts sent from here.
async fn receive(state: &AppState, ws: WorkspaceId, c: &Channel, r: Received) -> ApiResult<()> {
    let ctx = Ctx::system(ws);
    for o in r.outcomes.iter().filter(|o| o.status == "failed") {
        let mut tx = begin(state, &ctx).await?;
        if let Some((message, thread)) =
            core_db::messaging::sent_message(&mut tx, c.id, &o.id).await?
        {
            let why = o
                .error
                .as_deref()
                .unwrap_or("the provider could not deliver it");
            core_db::messaging::failed_later(&mut tx, message, why).await?;
            core_db::inbox::refresh(&mut tx, thread, false).await?;
            record(
                &mut tx,
                &ctx,
                Change {
                    topic: "thread.message_failed",
                    resource: inbox::ENTITY,
                    object_id: Some(thread),
                    before: None,
                    after: Some(json!({ "message_id": message })),
                    event_extra: None,
                },
            )
            .await?;
        }
        tx.commit().await?;
    }
    for m in r.messages {
        let mut tx = begin(state, &ctx).await?;
        // Nothing more is taken in once the number is disconnected.
        if core_db::messaging::get(&mut tx, c.id)
            .await?
            .is_none_or(|c| c.disconnected_at.is_some())
        {
            tx.commit().await?;
            return Ok(());
        }
        if let Some(out) = texts::opt_keyword(&c.kind, &m.text) {
            core_db::messaging::set_opt_out(&mut tx, &c.kind, &m.from, out).await?;
        }
        core_db::messaging::lock_number(&mut tx, c.id, &m.from).await?;
        let existing = core_db::messaging::thread_for(&mut tx, c.id, &m.from).await?;
        let created = existing.is_none();
        let thread_id = match existing {
            Some(t) => t,
            None => {
                let contact = match core_domain::crm::phone_key(&m.from) {
                    Some(key) => core_db::messaging::contact_with_phone(&mut tx, &key).await?,
                    None => None,
                };
                let id = core_domain::ids::new_id();
                core_db::messaging::insert_thread(
                    &mut tx,
                    NewThread {
                        id,
                        kind: &c.kind,
                        channel: c.id,
                        phone: &m.from,
                        contact_id: contact.map(|x| x.0),
                        organisation_id: contact.and_then(|x| x.1),
                        enquiry_id: None,
                        booking_id: None,
                        assigned_to: None,
                        unread: true,
                        at: Utc::now(),
                    },
                )
                .await?;
                id
            }
        };
        let sent_at =
            m.at.and_then(|s| Utc.timestamp_opt(s, 0).single())
                .filter(|at| *at <= Utc::now() + Duration::hours(1))
                .unwrap_or_else(Utc::now);
        let message_id = core_domain::ids::new_id();
        let inserted = core_db::messaging::insert_text(
            &mut tx,
            NewText {
                id: message_id,
                thread_id,
                channel: c.id,
                direction: "inbound",
                external_id: Some(&m.id),
                from: Some(&m.from),
                from_name: m.name.as_deref(),
                to: None,
                subject: None,
                body: &m.text,
                sent_at,
                status: "received",
                automatic: false,
                author_id: None,
                template_key: None,
            },
        )
        .await?;
        if !inserted {
            // Providers send a message again when unsure it arrived.
            tx.rollback().await?;
            continue;
        }
        core_db::inbox::refresh(&mut tx, thread_id, true).await?;
        let thread = core_db::inbox::get(&mut tx, thread_id, None)
            .await?
            .ok_or(ApiError::NotFound)?;
        record(
            &mut tx,
            &ctx,
            Change {
                topic: "message.received",
                resource: inbox::ENTITY,
                object_id: Some(thread_id),
                before: None,
                after: Some(json!({
                    "message_id": message_id,
                    "new_thread": created,
                    "automatic": false,
                    "bulk": false,
                    "channel": c.kind,
                    "messaging_channel_id": c.id,
                    "contact_id": thread.contact_id,
                    "organisation_id": thread.organisation_id,
                    "enquiry_id": thread.enquiry_id,
                    "booking_id": thread.booking_id,
                })),
                event_extra: None,
            },
        )
        .await?;
        tx.commit().await?;
    }
    Ok(())
}
