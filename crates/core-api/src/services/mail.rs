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
                    "url": url,
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
            tx.commit().await?;
            let name = workspace
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("OnHold")
                .to_owned();
            Ok(outgoing(
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
                vec![url],
            ))
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
                q.insert("url".into(), Value::String(url.clone()));
            }
            let mut tx = core_db::begin(&state.db, Some(ws), None).await?;
            let (workspace, options) = context::workspace(&mut tx, state).await?;
            let ctx_json = json!({
                "workspace": workspace,
                "quote": quote,
                "booking": event.get("booking"),
            });
            // A workspace may reword its quote email.
            let custom = core_db::templates::find(&mut tx, SYSTEM_QUOTE, &options.locale).await?;
            let (subject, body) = match &custom {
                Some(t) => (t.subject.as_deref(), t.body.as_str()),
                None => (Some(QUOTE_SUBJECT), QUOTE_BODY),
            };
            let rendered = super::templates::render_in(
                &mut tx,
                Kind::Email,
                subject,
                body,
                &ctx_json,
                &options,
            )
            .await?;
            tx.commit().await?;
            let name = workspace
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("OnHold")
                .to_owned();
            Ok(outgoing(
                state,
                job.id,
                rendered,
                &to,
                Some(name.clone()),
                &format!("Sent by {name} via OnHold."),
                Some(ws),
                None,
                SYSTEM_QUOTE,
                None,
                vec![url],
            ))
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
                i.insert("url".into(), Value::String(url.clone()));
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
                    "link": { "url": url },
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
/// `key`, else the default, sent in its name.
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
    let name = workspace
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("OnHold")
        .to_owned();
    Ok(outgoing(
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
    ))
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
    tx.commit().await?;
    let name = workspace
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("OnHold")
        .to_owned();
    Ok(outgoing(
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
    ))
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
            subject: Some(&out.email.subject),
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
    fn layout_wraps_fragments_only() {
        assert!(layout("<p>x</p>", "f").starts_with("<!doctype html>"));
        assert_eq!(
            layout("<html><body>x</body></html>", "f"),
            "<html><body>x</body></html>"
        );
        assert!(layout("<p>x</p>", "<b>").contains("&lt;b&gt;"));
    }
}
