//! Templates: Jinja (`minijinja`) for email subjects and bodies, SMS,
//! WhatsApp, in-app notifications and documents (contracts, invoices…,
//! printed to PDF; see [`crate::documents`]).
//!
//! Rendering is sandboxed: no filesystem, no network, a fuel budget so a
//! runaway loop stops, and a cap on output size. HTML bodies are
//! auto-escaped. Workspace partials (a signature, a legal footer) are
//! available to `{% include "signature" %}` by their key.
//!
//! Filters on top of the Jinja built-ins:
//!
//! - `money`: `{{ thing.custom.fee | money }}` → `£1,250.00`, from
//!   `{minor, currency}`; or `{{ 125000 | money("GBP") }}`.
//! - `date` / `datetime`: `{{ thing.due_at | date }}` → `7 March 2027`, in
//!   the workspace timezone; optional strftime format and timezone,
//!   `{{ x | datetime("%H:%M", "America/New_York") }}`.
//! - `address`: an address object on one line.
//! - `percent`: hundredths of a percent as a percentage, `{{ 1550 | percent }}`
//!   → `15.5%`.

use std::collections::{BTreeMap, BTreeSet};

use chrono::format::{Item, StrftimeItems};
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use minijinja::value::Value as JValue;
use minijinja::{AutoEscape, Environment, ErrorKind, UndefinedBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::path;

/// Wide enough that the plain-text version of an email never wraps a line.
const TEXT_WIDTH: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Email,
    Sms,
    Whatsapp,
    Notification,
    /// A fragment other templates include, such as a signature.
    Partial,
    /// A printed document: the subject is its title, the body its HTML.
    Document,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Email => "email",
            Kind::Sms => "sms",
            Kind::Whatsapp => "whatsapp",
            Kind::Notification => "notification",
            Kind::Partial => "partial",
            Kind::Document => "document",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(Value::String(s.to_owned())).ok()
    }

    /// Whether the body is HTML (and so auto-escaped).
    pub fn is_html(self) -> bool {
        matches!(self, Kind::Email | Kind::Partial | Kind::Document)
    }

    /// A WhatsApp template's subject names the template approved in
    /// WhatsApp Manager that it fills in.
    pub fn has_subject(self) -> bool {
        matches!(
            self,
            Kind::Email | Kind::Notification | Kind::Document | Kind::Whatsapp
        )
    }
}

/// How dates and money are shown.
#[derive(Debug, Clone)]
pub struct Options {
    pub locale: String,
    pub timezone: Tz,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            locale: "en".into(),
            timezone: Tz::UTC,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rendered {
    pub subject: Option<String>,
    pub body: String,
    /// The plain-text alternative of an HTML body.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, utoipa::ToSchema)]
#[error("{part}{}: {message}", line.map(|l| format!(" line {l}")).unwrap_or_default())]
pub struct TemplateError {
    /// `subject`, `body` or the key of the partial at fault.
    pub part: String,
    pub line: Option<usize>,
    pub message: String,
}

/// Instructions a template may run before it is stopped.
const FUEL: u64 = 250_000;
/// The largest rendered output allowed.
const MAX_OUTPUT: usize = 1024 * 1024;
const SUBJECT: &str = "subject";
const BODY: &str = "body";

fn error(err: &minijinja::Error, default_part: &str) -> TemplateError {
    let message = match (err.kind(), err.detail()) {
        (ErrorKind::OutOfFuel, _) => "the template did too much work; simplify loops".into(),
        (ErrorKind::TemplateNotFound, _) => match err.name() {
            Some(_) | None => err
                .detail()
                .map_or_else(|| "that partial does not exist".into(), str::to_owned),
        },
        (_, Some(detail)) => detail.to_owned(),
        (kind, None) => kind.to_string(),
    };
    TemplateError {
        part: err.name().unwrap_or(default_part).to_owned(),
        line: err.line(),
        message,
    }
}

fn check_format(fmt: &str) -> Result<(), minijinja::Error> {
    let ok = StrftimeItems::new(fmt)
        .parse()
        .is_ok_and(|items| !items.iter().any(|i| matches!(i, Item::Error)));
    if ok && fmt.len() <= 100 {
        Ok(())
    } else {
        Err(minijinja::Error::new(
            ErrorKind::InvalidOperation,
            format!("`{fmt}` is not a valid date format"),
        ))
    }
}

fn zone(tz: Option<&str>, default: Tz) -> Result<Tz, minijinja::Error> {
    match tz {
        None => Ok(default),
        Some(name) => name.parse::<Tz>().map_err(|_| {
            minijinja::Error::new(
                ErrorKind::InvalidOperation,
                format!("`{name}` is not an IANA timezone"),
            )
        }),
    }
}

fn format_time(
    value: &JValue,
    fmt: &str,
    tz: Tz,
    date_only: bool,
) -> Result<String, minijinja::Error> {
    if value.is_undefined() || value.is_none() {
        return Ok(String::new());
    }
    let s = value.as_str().ok_or_else(|| {
        minijinja::Error::new(ErrorKind::InvalidOperation, "expected a date or date-time")
    })?;
    check_format(fmt)?;
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        if date_only {
            return Ok(d.format(fmt).to_string());
        }
        // A bare date has no time to show; render it as a date.
        return Ok(d.format("%-d %B %Y").to_string());
    }
    let dt = DateTime::parse_from_rfc3339(s).map_err(|_| {
        minijinja::Error::new(
            ErrorKind::InvalidOperation,
            format!("`{s}` is not a date or date-time"),
        )
    })?;
    Ok(dt
        .with_timezone(&Utc)
        .with_timezone(&tz)
        .format(fmt)
        .to_string())
}

fn environment(
    kind: Kind,
    partials: BTreeMap<String, String>,
    opts: &Options,
) -> Environment<'static> {
    let mut env = Environment::new();
    env.set_fuel(Some(FUEL));
    env.set_undefined_behavior(UndefinedBehavior::Chainable);
    env.set_recursion_limit(20);
    let html = kind.is_html();
    env.set_auto_escape_callback(move |name| {
        if html && name != SUBJECT {
            AutoEscape::Html
        } else {
            AutoEscape::None
        }
    });
    env.set_loader(move |name| Ok(partials.get(name).cloned()));

    let locale = opts.locale.clone();
    env.add_filter(
        "money",
        move |value: JValue, currency: Option<String>| -> Result<String, minijinja::Error> {
            if value.is_undefined() || value.is_none() {
                return Ok(String::new());
            }
            let invalid = || {
                minijinja::Error::new(
                    ErrorKind::InvalidOperation,
                    "money expects {minor, currency} or an amount with a currency, e.g. money(\"GBP\")",
                )
            };
            let (minor, code) = match (value.as_i64(), currency) {
                (Some(minor), Some(code)) => (minor, code),
                _ => {
                    let json = serde_json::to_value(&value).map_err(|_| invalid())?;
                    let m: core_domain::Money =
                        serde_json::from_value(json).map_err(|_| invalid())?;
                    (m.minor, m.currency)
                }
            };
            Ok(core_domain::money::format_minor(minor, &code, &locale))
        },
    );
    let tz = opts.timezone;
    env.add_filter(
        "date",
        move |value: JValue, fmt: Option<String>, zone_name: Option<String>| {
            let tz = zone(zone_name.as_deref(), tz)?;
            format_time(&value, fmt.as_deref().unwrap_or("%-d %B %Y"), tz, true)
        },
    );
    env.add_filter(
        "datetime",
        move |value: JValue, fmt: Option<String>, zone_name: Option<String>| {
            let tz = zone(zone_name.as_deref(), tz)?;
            format_time(
                &value,
                fmt.as_deref().unwrap_or("%-d %B %Y, %H:%M %Z"),
                tz,
                false,
            )
        },
    );
    env.add_filter("address", |value: JValue| -> String {
        let parts = ["line1", "line2", "city", "region", "postcode", "country"];
        parts
            .iter()
            .filter_map(|p| value.get_attr(p).ok())
            .filter_map(|v| v.as_str().map(str::trim).map(str::to_owned))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
    });
    env.add_filter("percent", |value: JValue| -> String {
        match value.as_i64() {
            Some(bps) => percent(bps),
            None => String::new(),
        }
    });
    env
}

/// `1550` hundredths of a percent as `15.5%`.
pub fn percent(bps: i64) -> String {
    let sign = if bps < 0 { "-" } else { "" };
    let (whole, frac) = (bps.unsigned_abs() / 100, bps.unsigned_abs() % 100);
    if frac == 0 {
        format!("{sign}{whole}%")
    } else {
        let digits = format!("{frac:02}");
        format!("{sign}{whole}.{}%", digits.trim_end_matches('0'))
    }
}

/// Checks a template's syntax without rendering it.
pub fn check(kind: Kind, subject: Option<&str>, body: &str) -> Result<(), TemplateError> {
    let env = environment(kind, BTreeMap::new(), &Options::default());
    if let Some(s) = subject {
        env.template_from_named_str(SUBJECT, s)
            .map_err(|e| error(&e, SUBJECT))?;
    }
    env.template_from_named_str(BODY, body)
        .map_err(|e| error(&e, BODY))?;
    Ok(())
}

/// Renders a template against `ctx`. `partials` maps partial keys to their source.
pub fn render(
    kind: Kind,
    subject: Option<&str>,
    body: &str,
    partials: BTreeMap<String, String>,
    ctx: &Value,
    opts: &Options,
) -> Result<Rendered, TemplateError> {
    let env = environment(kind, partials, opts);
    let ctx = JValue::from(minijinja::value::Serde(ctx));
    let subject = match subject {
        Some(s) if kind.has_subject() => {
            let t = env
                .template_from_named_str(SUBJECT, s)
                .map_err(|e| error(&e, SUBJECT))?;
            let out = t.render(&ctx).map_err(|e| error(&e, SUBJECT))?;
            // Subjects are one line.
            Some(out.split_whitespace().collect::<Vec<_>>().join(" "))
        }
        _ => None,
    };
    let t = env
        .template_from_named_str(BODY, body)
        .map_err(|e| error(&e, BODY))?;
    let body = t.render(&ctx).map_err(|e| error(&e, BODY))?;
    if body.len() > MAX_OUTPUT {
        return Err(TemplateError {
            part: BODY.into(),
            line: None,
            message: "the rendered template is larger than 1 MB".into(),
        });
    }
    let text = if kind.is_html() {
        // No wrapping: a link broken across lines no longer works when
        // copied, and mail clients wrap plain text themselves.
        html2text::from_read(body.as_bytes(), TEXT_WIDTH).unwrap_or_else(|_| body.clone())
    } else {
        body.clone()
    };
    Ok(Rendered {
        subject,
        body,
        text,
    })
}

/// Variables a template reads (as dotted paths) that `ctx` does not have:
/// the "you referenced `thing.custom.fee` but there's no such field" warning.
pub fn missing_variables(
    kind: Kind,
    subject: Option<&str>,
    body: &str,
    ctx: &Value,
) -> Vec<String> {
    let env = environment(kind, BTreeMap::new(), &Options::default());
    let mut used = BTreeSet::new();
    for (name, source) in [(SUBJECT, subject), (BODY, Some(body))] {
        if let Some(src) = source
            && let Ok(t) = env.template_from_named_str(name, src)
        {
            used.extend(t.undeclared_variables(true));
        }
    }
    used.into_iter()
        .filter(|v| v != "loop" && path::lookup(ctx, v).is_none())
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn opts() -> Options {
        Options {
            locale: "en-GB".into(),
            timezone: "Europe/London".parse().expect("tz"),
        }
    }

    fn ctx() -> Value {
        json!({
            "workspace": { "name": "Acme <Agency>" },
            "thing": {
                "name": "Spring tour",
                "due_at": "2027-07-05T18:30:00Z",
                "custom": { "fee": { "minor": 125000, "currency": "GBP" }, "show": "2027-03-07",
                            "venue": { "line1": "1 High St", "city": "Leeds", "postcode": "LS1 1AA", "country": "GB" } }
            }
        })
    }

    #[test]
    fn the_text_version_keeps_long_links_whole() {
        let url = format!(
            "https://app.example/invitations/accept?token={}",
            "x".repeat(120)
        );
        let body = format!(
            "<p>Hello. {}</p><p><a href=\"{url}\">Accept</a></p>",
            "word ".repeat(40)
        );
        let out =
            render(Kind::Email, None, &body, BTreeMap::new(), &ctx(), &opts()).expect("renders");
        assert!(out.text.lines().any(|l| l.contains(&url)), "{}", out.text);
    }

    #[test]
    fn renders_email_with_filters_and_escaping() {
        let out = render(
            Kind::Email,
            Some("Booking for {{ thing.name }}\n"),
            "<p>{{ workspace.name }}: {{ thing.custom.fee | money }} on {{ thing.custom.show | date }} \
             at {{ thing.due_at | datetime(\"%H:%M\") }} ({{ thing.due_at | datetime(\"%H:%M\", \"America/New_York\") }}), \
             {{ thing.custom.venue | address }}</p>",
            BTreeMap::new(),
            &ctx(),
            &opts(),
        )
        .expect("renders");
        assert_eq!(out.subject.as_deref(), Some("Booking for Spring tour"));
        assert_eq!(
            out.body,
            "<p>Acme &lt;Agency&gt;: £1,250.00 on 7 March 2027 at 19:30 (14:30), 1 High St, Leeds, LS1 1AA, GB</p>"
        );
        assert!(out.text.contains("Acme <Agency>"), "{}", out.text);
    }

    #[test]
    fn subjects_and_sms_are_not_html_escaped() {
        let out = render(
            Kind::Sms,
            None,
            "Hi {{ workspace.name }}",
            BTreeMap::new(),
            &ctx(),
            &opts(),
        )
        .expect("renders");
        assert_eq!(out.body, "Hi Acme <Agency>");
        let out = render(
            Kind::Email,
            Some("{{ workspace.name }}"),
            "x",
            BTreeMap::new(),
            &ctx(),
            &opts(),
        )
        .expect("renders");
        assert_eq!(out.subject.as_deref(), Some("Acme <Agency>"));
    }

    #[test]
    fn partials_are_included_by_key() {
        let partials = BTreeMap::from([(
            "signature".to_owned(),
            "<em>{{ workspace.name }}</em>".to_owned(),
        )]);
        let out = render(
            Kind::Email,
            None,
            "Hi{% include \"signature\" %}",
            partials,
            &ctx(),
            &opts(),
        )
        .expect("renders");
        assert_eq!(out.body, "Hi<em>Acme &lt;Agency&gt;</em>");
        let err = render(
            Kind::Email,
            None,
            "{% include \"nope\" %}",
            BTreeMap::new(),
            &ctx(),
            &opts(),
        )
        .expect_err("missing partial");
        assert_eq!(err.part, "body");
    }

    #[test]
    fn errors_name_part_and_line() {
        let err = check(Kind::Email, Some("ok"), "line one\n{% if %}").expect_err("syntax");
        assert_eq!((err.part.as_str(), err.line), ("body", Some(2)));
        let err = check(Kind::Email, Some("{{ "), "ok").expect_err("syntax");
        assert_eq!(err.part, "subject");
        let err = render(
            Kind::Email,
            None,
            "{{ thing.name | date(\"%Q\") }}",
            BTreeMap::new(),
            &ctx(),
            &opts(),
        )
        .expect_err("bad format");
        assert!(err.message.contains("date format"), "{}", err.message);
    }

    #[test]
    fn runaway_templates_stop() {
        let err = render(
            Kind::Email,
            None,
            "{% for i in range(100000) %}{% for j in range(100) %}x{% endfor %}{% endfor %}",
            BTreeMap::new(),
            &ctx(),
            &opts(),
        )
        .expect_err("out of fuel");
        assert!(err.message.contains("too much work"), "{}", err.message);
    }

    #[test]
    fn missing_values_render_empty_and_are_reported() {
        let out = render(
            Kind::Email,
            None,
            "[{{ thing.custom.nope.deeper }}]",
            BTreeMap::new(),
            &ctx(),
            &opts(),
        )
        .expect("chainable undefined");
        assert_eq!(out.body, "[]");
        let missing = missing_variables(
            Kind::Email,
            Some("{{ thing.name }}"),
            "{{ thing.custom.fee | money }} {{ thing.custom.nope }} {% for x in [1] %}{{ loop.index }}{% endfor %}",
            &ctx(),
        );
        assert_eq!(missing, ["thing.custom.nope"]);
    }

    #[test]
    fn templates_cannot_reach_the_filesystem() {
        let err = render(
            Kind::Email,
            None,
            "{% include \"/etc/passwd\" %}",
            BTreeMap::new(),
            &ctx(),
            &opts(),
        )
        .expect_err("no files");
        assert_eq!(err.part, "body");
    }
}
