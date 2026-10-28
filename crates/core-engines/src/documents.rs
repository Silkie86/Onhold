//! Documents: the HTML page a document template is printed in, the
//! templates every workspace starts with (contract, confirmation, advance
//! sheet, invoice, credit note, settlement statement) and the signing
//! certificate added to a signed copy.
//!
//! A template renders only the document's content. [`page`] wraps it in a
//! complete page with the workspace's colour and logo, a header naming the
//! issuer, print rules for A4, and a Content-Security-Policy that lets the
//! page load nothing from outside itself.

use serde::Serialize;

/// The workspace's look, checked so it can go into CSS and HTML safely.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Branding {
    /// Shown in the header when there is no logo.
    pub name: String,
    /// A `data:image/…;base64,…` URI.
    pub logo: Option<String>,
    /// `#rrggbb` or `#rgb`.
    pub primary: Option<String>,
    /// The issuer's address and numbers, one line, under the name.
    pub header_line: Option<String>,
    /// The theme's typeface, by its key (see [`font_stack`]).
    pub font: Option<String>,
}

/// The fonts a theme may use, as system font stacks so nothing is
/// downloaded; the same list as the web app's.
pub fn font_stack(key: &str) -> Option<&'static str> {
    Some(match key {
        "default" => {
            r#""Inter", "Helvetica Neue", Helvetica, Arial, "Liberation Sans", sans-serif"#
        }
        "system" => {
            r#"system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif"#
        }
        "humanist" => {
            r#"Seravek, "Gill Sans Nova", Ubuntu, Calibri, "DejaVu Sans", source-sans-pro, sans-serif"#
        }
        "geometric" => r#"Avenir, Montserrat, Corbel, "URW Gothic", source-sans-pro, sans-serif"#,
        "serif" => r#"Charter, "Bitstream Charter", "Sitka Text", Cambria, Georgia, serif"#,
        "rounded" => {
            r#"ui-rounded, "Hiragino Maru Gothic ProN", Quicksand, Comfortaa, Manjari, "Arial Rounded MT", Calibri, sans-serif"#
        }
        "mono" => {
            r#"ui-monospace, "Cascadia Code", "Source Code Pro", Menlo, Consolas, "DejaVu Sans Mono", monospace"#
        }
        _ => return None,
    })
}

fn channels(hex: &str) -> Option<[f64; 3]> {
    let h = hex.strip_prefix('#')?;
    let h: String = if h.len() == 3 {
        h.chars().flat_map(|c| [c, c]).collect()
    } else {
        h.to_owned()
    };
    let v = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok().map(f64::from);
    Some([v(0)?, v(2)?, v(4)?])
}

fn luminance(rgb: [f64; 3]) -> f64 {
    let lin = |c: f64| {
        let c = c / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(rgb[0]) + 0.7152 * lin(rgb[1]) + 0.0722 * lin(rgb[2])
}

/// A brand colour that reads as text on white paper and in email (at least
/// 4.5:1): the colour itself, or a darker shade of it when it is too light.
pub fn readable_on_white(hex: &str) -> Option<String> {
    let rgb = channels(&colour(hex)?)?;
    for step in 0..=20 {
        let k = 1.0 - f64::from(step) * 0.05;
        let shade = rgb.map(|c| (c * k).round());
        if 1.05 / (luminance(shade) + 0.05) >= 4.5 {
            let [r, g, b] = shade.map(|c| c as u8);
            return Some(format!("#{r:02x}{g:02x}{b:02x}"));
        }
    }
    None
}

/// A colour if it is `#rgb` or `#rrggbb`; anything else is ignored.
pub fn colour(value: &str) -> Option<String> {
    let v = value.trim();
    let hex = v.strip_prefix('#')?;
    ((hex.len() == 3 || hex.len() == 6) && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| v.to_ascii_lowercase())
}

/// A logo if it is a base64 data URI of an image type browsers print.
pub fn logo(value: &str) -> Option<String> {
    let v = value.trim();
    let rest = v.strip_prefix("data:")?;
    let (mime, data) = rest.split_once(";base64,")?;
    let ok_mime = matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/svg+xml"
    );
    let ok_data = !data.is_empty()
        && data.len() <= 1_500_000
        && data
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='));
    (ok_mime && ok_data).then(|| v.to_owned())
}

pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// What a printed page may load: its own styles, and images and fonts
/// written into it. No scripts, frames, files or addresses.
pub const POLICY: &str =
    "default-src 'none'; style-src 'unsafe-inline' data:; img-src data:; font-src data:";

const BASE_CSS: &str = r#"
@page { size: A4; margin: 16mm 16mm 18mm; }
* { box-sizing: border-box; }
html { -webkit-print-color-adjust: exact; print-color-adjust: exact; }
body { margin: 0; font-family: var(--font);
  font-size: 10pt; line-height: 1.45; color: #1d1d1f; }
h1 { font-size: 20pt; margin: 0 0 4mm; color: var(--brand); font-weight: 600; }
h2 { font-size: 12pt; margin: 7mm 0 2mm; color: var(--brand); font-weight: 600; }
p { margin: 0 0 2.5mm; }
table { width: 100%; border-collapse: collapse; margin: 2mm 0 4mm; }
th, td { text-align: left; vertical-align: top; padding: 1.8mm 2mm; border-bottom: 0.3mm solid #e2e2e6; }
th { font-size: 8.5pt; text-transform: uppercase; letter-spacing: .04em; color: #6e6e73; font-weight: 600; }
td.num, th.num { text-align: right; white-space: nowrap; }
tr.total td { font-weight: 600; border-bottom: none; }
tr.grand td { font-size: 12pt; font-weight: 700; color: var(--brand); border-top: 0.5mm solid var(--brand); border-bottom: none; }
.doc-header { display: flex; justify-content: space-between; align-items: flex-start; gap: 8mm;
  padding-bottom: 5mm; margin-bottom: 7mm; border-bottom: 0.8mm solid var(--brand); }
.doc-header img { max-height: 18mm; max-width: 70mm; }
.doc-header .issuer { text-align: right; font-size: 8.5pt; color: #6e6e73; max-width: 95mm; }
.doc-header .name { font-size: 15pt; font-weight: 700; color: var(--brand); }
.meta { display: flex; flex-wrap: wrap; gap: 4mm 12mm; margin: 0 0 6mm; }
.meta div { min-width: 35mm; }
.label { display: block; font-size: 8pt; text-transform: uppercase; letter-spacing: .05em; color: #6e6e73; }
.parties { display: flex; gap: 10mm; margin-bottom: 6mm; }
.parties > div { flex: 1; }
.box { border: 0.3mm solid #e2e2e6; border-radius: 2mm; padding: 3mm 4mm; margin: 3mm 0; }
.note { color: #6e6e73; font-size: 8.5pt; }
.terms { white-space: pre-line; }
.signatures { display: flex; gap: 12mm; margin-top: 12mm; page-break-inside: avoid; }
.signatures > div { flex: 1; border-top: 0.3mm solid #1d1d1f; padding-top: 2mm; font-size: 9pt; }
.pre { white-space: pre-line; }
footer { margin-top: 10mm; font-size: 8pt; color: #6e6e73; white-space: pre-line; }
"#;

/// A complete, self-contained page for a rendered document body.
pub fn page(title: &str, body: &str, branding: &Branding) -> String {
    let brand = branding
        .primary
        .as_deref()
        .and_then(readable_on_white)
        .unwrap_or_else(|| "#1d1d1f".into());
    let font = branding
        .font
        .as_deref()
        .and_then(font_stack)
        .unwrap_or(r#""Helvetica Neue", Helvetica, Arial, "Liberation Sans", sans-serif"#);
    let mark = match branding.logo.as_deref().and_then(logo) {
        Some(src) => format!(
            "<img src=\"{}\" alt=\"{}\">",
            escape(&src),
            escape(&branding.name)
        ),
        None => format!("<div class=\"name\">{}</div>", escape(&branding.name)),
    };
    let line = branding
        .header_line
        .as_deref()
        .map(escape)
        .unwrap_or_default();
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{POLICY}\">\
         <title>{title}</title><style>:root {{ --brand: {brand}; --font: {font}; }}{BASE_CSS}</style></head>\
         <body><header class=\"doc-header\"><div>{mark}</div><div class=\"issuer\">{line}</div></header>\
         {body}</body></html>",
        title = escape(title),
    )
}

/// One person's signature, for the certificate.
#[derive(Debug, Clone, Serialize)]
pub struct SignatureRecord {
    pub name: String,
    pub email: String,
    pub signed_name: String,
    /// RFC 3339, UTC.
    pub signed_at: String,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
}

/// The page added to the end of a signed copy: who signed what, when and
/// from where, and the fingerprint of exactly the PDF they signed.
pub fn certificate(
    document_title: &str,
    reference: &str,
    pdf_sha256_hex: &str,
    signatures: &[SignatureRecord],
    branding: &Branding,
) -> String {
    let rows: String = signatures
        .iter()
        .map(|s| {
            format!(
                "<tr><td><strong>{}</strong><br>{}</td><td>{}</td><td>{}</td><td>{}<br><span class=\"note\">{}</span></td></tr>",
                escape(&s.name),
                escape(&s.email),
                escape(&s.signed_name),
                escape(&s.signed_at),
                escape(s.ip.as_deref().unwrap_or("unknown")),
                escape(s.user_agent.as_deref().unwrap_or("")),
            )
        })
        .collect();
    let body = format!(
        "<h1>Signing certificate</h1>\
         <p>This page records the electronic signatures given to <strong>{title}</strong> ({reference}).</p>\
         <div class=\"box\"><span class=\"label\">Fingerprint of the signed document (SHA-256)</span>\
         <code style=\"word-break:break-all\">{hash}</code></div>\
         <p class=\"note\">Each person below opened the document through a link sent to their email address only, \
         read it, and typed their name to sign it. The fingerprint identifies exactly the PDF they were shown: \
         any later change to it gives a different fingerprint.</p>\
         <table><tr><th>Signer</th><th>Typed signature</th><th>Signed at (UTC)</th><th>From</th></tr>{rows}</table>",
        title = escape(document_title),
        reference = escape(reference),
        hash = escape(pdf_sha256_hex),
    );
    page("Signing certificate", &body, branding)
}

/// A template every workspace has until it saves its own with the same key.
#[derive(Debug, Clone, Copy)]
pub struct BuiltIn {
    pub key: &'static str,
    /// The kind of document it makes.
    pub kind: &'static str,
    pub name: &'static str,
    /// The record it is made from.
    pub entity: &'static str,
    pub subject: &'static str,
    pub body: &'static str,
}

pub const CONTRACT: &str = "document.contract";
pub const CONFIRMATION: &str = "document.confirmation";
pub const ADVANCE_SHEET: &str = "document.advance_sheet";
pub const DAY_SHEET: &str = "document.day_sheet";
pub const INVOICE: &str = "document.invoice";
pub const CREDIT_NOTE: &str = "document.credit_note";
pub const STATEMENT: &str = "document.statement";

const CONTRACT_BODY: &str = concat!(
    r#"<h1>{{ document.title }}</h1>
<p class="note">Reference {{ document.reference }} · {{ document.date | date }}</p>
"#,
    r#"<div class="parties">
  <div><span class="label">Between</span><strong>{{ issuer.name }}</strong>{% if issuer.address %}<br>{{ issuer.address | address }}{% endif %}</div>
  <div><span class="label">And</span><strong>{{ organisation.name or contact.name or "The buyer" }}</strong>{% if organisation.address %}<br>{{ organisation.address | address }}{% endif %}{% if contact.name and organisation.name %}<br>Contact: {{ contact.name }}{% endif %}</div>
</div>
<div class="meta">
  <div><span class="label">Engagement</span>{{ booking.title }}</div>
  <div><span class="label">Date</span>{{ booking.starts_at | date(none, booking.timezone) }}{% if not booking.all_day %}, {{ booking.starts_at | datetime("%H:%M", booking.timezone) }}–{{ booking.ends_at | datetime("%H:%M", booking.timezone) }}{% endif %}</div>
  <div><span class="label">Venue</span>{{ booking.venue or "To be confirmed" }}</div>
  <div><span class="label">Artist</span>{{ booking.talent_names or talent.name }}</div>
</div>
<h2>1. The engagement</h2>
<p>{{ issuer.name }} agrees to provide {{ booking.talent_names or talent.name }} for {{ booking.title }} on the date, at the venue and at the times set out above{% if booking.schedule %}, and in the running order below{% endif %}.</p>
{% if booking.schedule %}<table><tr><th>Time</th><th>What</th></tr>{% for s in booking.schedule %}<tr><td>{{ s.at | datetime("%H:%M", booking.timezone) }}</td><td>{{ s.label }}</td></tr>{% endfor %}</table>{% endif %}
<h2>2. The fee</h2>
{% if quote %}<table><tr><th>Item</th><th class="num">Amount</th></tr>{% for l in quote.line_items %}<tr><td>{{ l.description }}</td><td class="num">{{ l.amount_minor | money(quote.currency) }}</td></tr>{% endfor %}<tr class="grand"><td>Total</td><td class="num">{{ quote.total | money }}</td></tr></table>
{% elif booking.fee %}<p>The fee is <strong>{{ booking.fee | money }}</strong>.</p>{% else %}<p>The fee is as agreed between the parties in writing.</p>{% endif %}
{% if booking.deposit_bps %}<p>A deposit of {{ booking.deposit_bps | percent }} of the fee is payable on signature, and the balance before the engagement. Invoices state the dates they are due.</p>{% else %}<p>The fee is payable against invoice by the date it states.</p>{% endif %}
<h2>3. Terms</h2>
{% if quote.terms %}<div class="terms">{{ quote.terms }}</div>{% else %}<p>The buyer provides safe access, power and the facilities agreed in the artist's rider. Either party may cancel in writing; a cancellation by the buyer within 30 days of the engagement makes the full fee payable. Neither party is liable for failure caused by events beyond its reasonable control.</p>{% endif %}
<div class="signatures">
  <div>For {{ issuer.name }}</div>
  <div>For {{ organisation.name or contact.name or "the buyer" }}</div>
</div>"#
);

const CONFIRMATION_BODY: &str = concat!(
    r#"<h1>{{ document.title }}</h1>
<p>This confirms the booking below. Please check the details and let us know at once if anything is wrong.</p>
"#,
    r#"<div class="meta">
  <div><span class="label">Engagement</span>{{ booking.title }}</div>
  <div><span class="label">Date</span>{{ booking.starts_at | date(none, booking.timezone) }}{% if not booking.all_day %}, {{ booking.starts_at | datetime("%H:%M", booking.timezone) }}–{{ booking.ends_at | datetime("%H:%M", booking.timezone) }}{% endif %}</div>
  <div><span class="label">Venue</span>{{ booking.venue or "To be confirmed" }}</div>
  <div><span class="label">Artist</span>{{ booking.talent_names or talent.name }}</div>
</div>
<table>
  <tr><th>Status</th><td>{{ booking.status | capitalize }}</td></tr>
  {% if organisation.name %}<tr><th>Buyer</th><td>{{ organisation.name }}{% if contact.name %} ({{ contact.name }}){% endif %}</td></tr>{% endif %}
  {% if booking.fee %}<tr><th>Fee</th><td>{{ booking.fee | money }}</td></tr>{% endif %}
</table>
{% if booking.notes %}<h2>Notes</h2><p class="pre">{{ booking.notes }}</p>{% endif %}
<p>{{ issuer.name }}{% if issuer.email %} · {{ issuer.email }}{% endif %}{% if issuer.phone %} · {{ issuer.phone }}{% endif %}</p>"#
);

/// Travel and hotels, shared by the advance sheet and the day sheet. Booking
/// references go only where `$refs` is true: they are enough to change a
/// booking, so they stay off pages that go to buyers and venues.
macro_rules! travel_part {
    ($ref_head:literal, $ref_cell:literal, $hotel_ref:literal) => {
        concat!(
            r#"{% set journeys = travel | rejectattr("kind", "equalto", "hotel") | list %}{% if journeys %}<h2>Travel</h2>
<table><tr><th>When</th><th>What</th><th>From</th><th>To</th>"#,
            $ref_head,
            r#"</tr>
{% for t in journeys %}<tr><td>{{ t.starts_at | datetime("%a %-d %b, %H:%M", t.start_tz) }}{% if t.ends_at %} – {% if (t.ends_at | datetime("%F", t.end_tz)) != (t.starts_at | datetime("%F", t.start_tz)) %}{{ t.ends_at | datetime("%a %-d %b, %H:%M", t.end_tz) }}{% else %}{{ t.ends_at | datetime("%H:%M", t.end_tz) }}{% endif %}{% endif %}</td><td>{{ t.label }}{% if t.status == "planned" %} <strong>(not booked yet)</strong>{% endif %}{% if t.travellers %}<br><span class="note">{{ t.travellers | join(", ") }}</span>{% endif %}</td><td>{{ t.from_code or "" }}{% if t.from_code and t.from_place %} · {% endif %}{{ t.from_place or "" }}</td><td>{{ t.to_code or "" }}{% if t.to_code and t.to_place %} · {% endif %}{{ t.to_place or "" }}</td>"#,
            $ref_cell,
            r#"</tr>{% endfor %}
</table>{% endif %}
{% for t in travel %}{% if t.kind == "hotel" %}<h2>Hotel{% if t.status == "planned" %} (not booked yet){% endif %}</h2>
<table><tr><th>Hotel</th><td>{{ t.carrier or "To be confirmed" }}{% if t.to_place %}<br>{{ t.to_place }}{% endif %}</td></tr>
<tr><th>Check-in</th><td>{{ t.starts_at | datetime("%a %-d %b, %H:%M", t.start_tz) }}</td></tr>
{% if t.ends_at %}<tr><th>Check-out</th><td>{{ t.ends_at | datetime("%a %-d %b, %H:%M", t.end_tz) }}</td></tr>{% endif %}
"#,
            $hotel_ref,
            r#"
{% if t.travellers %}<tr><th>Guests</th><td>{{ t.travellers | join(", ") }}</td></tr>{% endif %}
{% if t.notes %}<tr><th>Notes</th><td class="pre">{{ t.notes }}</td></tr>{% endif %}</table>{% endif %}{% endfor %}"#
        )
    };
    (private) => {
        travel_part!(
            "<th>Reference</th>",
            r#"<td>{{ t.reference or "" }}</td>"#,
            r#"{% if t.reference %}<tr><th>Reference</th><td>{{ t.reference }}</td></tr>{% endif %}"#
        )
    };
    (shared) => {
        travel_part!("", "", "")
    };
}

const ADVANCE_SHEET_BODY: &str = concat!(
    r#"<h1>{{ document.title }}</h1>
<div class="meta">
  <div><span class="label">Engagement</span>{{ booking.title }}</div>
  <div><span class="label">Date</span>{{ booking.starts_at | date(none, booking.timezone) }}</div>
  <div><span class="label">Venue</span>{{ booking.venue or "To be confirmed" }}</div>
  <div><span class="label">Artist</span>{{ booking.talent_names or talent.name }}</div>
</div>
<h2>Running order</h2>
<table><tr><th>Time</th><th>What</th></tr>
{% for s in booking.schedule %}<tr><td>{{ s.at | datetime("%H:%M", booking.timezone) }}</td><td>{{ s.label }}</td></tr>{% else %}<tr><td>{% if not booking.all_day %}{{ booking.starts_at | datetime("%H:%M", booking.timezone) }}{% endif %}</td><td>Start</td></tr><tr><td>{% if not booking.all_day %}{{ booking.ends_at | datetime("%H:%M", booking.timezone) }}{% endif %}</td><td>Finish</td></tr>{% endfor %}
</table>
<h2>Contacts</h2>
<table>
  {% if contact.name %}<tr><th>Buyer</th><td>{{ contact.name }}{% if contact.phones %} · {{ contact.phones[0] }}{% endif %}{% if contact.emails %} · {{ contact.emails[0] }}{% endif %}</td></tr>{% endif %}
  <tr><th>Agency</th><td>{{ issuer.name }}{% if issuer.phone %} · {{ issuer.phone }}{% endif %}{% if issuer.email %} · {{ issuer.email }}{% endif %}</td></tr>
</table>
{% if talent.members %}<h2>Party</h2><table><tr><th>Name</th><th>Role</th></tr>{% for m in talent.members %}<tr><td>{{ m.name }}</td><td>{{ m.role }}</td></tr>{% endfor %}</table>{% endif %}
"#,
    travel_part!(shared),
    r#"
<h2>Notes</h2>
<p class="pre">{{ booking.notes or "Parking, load-in access, hospitality and anything else the artist should know." }}</p>"#
);

/// One day on the road, for the people travelling: where to be and when,
/// how they get there, where they sleep and who to call.
const DAY_SHEET_BODY: &str = concat!(
    r#"<h1>{{ document.title }}</h1>
<div class="meta">
  <div><span class="label">Day</span>{{ booking.starts_at | date(none, booking.timezone) }}</div>
  <div><span class="label">Engagement</span>{{ booking.title }}</div>
  <div><span class="label">Venue</span>{{ booking.venue or "To be confirmed" }}{% if booking.venue_address %}<br>{{ booking.venue_address | address }}{% endif %}</div>
  <div><span class="label">Who</span>{{ booking.talent_names or talent.name }}</div>
</div>
<h2>Schedule</h2>
<table><tr><th>Time</th><th>What</th></tr>
{% for s in booking.schedule %}<tr><td>{{ s.at | datetime("%H:%M", booking.timezone) }}</td><td>{{ s.label }}</td></tr>{% else %}<tr><td>{% if not booking.all_day %}{{ booking.starts_at | datetime("%H:%M", booking.timezone) }}{% endif %}</td><td>Start</td></tr><tr><td>{% if not booking.all_day %}{{ booking.ends_at | datetime("%H:%M", booking.timezone) }}{% endif %}</td><td>Finish</td></tr>{% endfor %}
</table>
"#,
    travel_part!(private),
    r#"
<h2>Contacts</h2>
<table>
  {% if contact.name %}<tr><th>On the day</th><td>{{ contact.name }}{% if contact.phones %} · {{ contact.phones[0] }}{% endif %}{% if contact.emails %} · {{ contact.emails[0] }}{% endif %}</td></tr>{% endif %}
  <tr><th>Office</th><td>{{ issuer.name }}{% if issuer.phone %} · {{ issuer.phone }}{% endif %}{% if issuer.email %} · {{ issuer.email }}{% endif %}</td></tr>
</table>
{% if talent.members %}<h2>Party</h2><table><tr><th>Name</th><th>Role</th></tr>{% for m in talent.members %}<tr><td>{{ m.name }}</td><td>{{ m.role }}</td></tr>{% endfor %}</table>{% endif %}
{% if booking.notes %}<h2>Notes</h2><p class="pre">{{ booking.notes }}</p>{% endif %}"#
);

const INVOICE_BODY: &str = r#"<h1>{% if invoice.kind == "credit_note" %}Credit note{% else %}Invoice{% endif %} {{ invoice.reference }}</h1>
<div class="parties">
  <div><span class="label">From</span><strong>{{ invoice.issuer.name }}</strong>{% if invoice.issuer.address %}<br>{{ invoice.issuer.address | address }}{% endif %}{% if invoice.issuer.tax_number %}<br>Tax number {{ invoice.issuer.tax_number }}{% endif %}{% if invoice.issuer.company_number %}<br>Company number {{ invoice.issuer.company_number }}{% endif %}</div>
  <div><span class="label">To</span><strong>{{ invoice.payer.name }}</strong>{% if invoice.payer.address %}<br>{{ invoice.payer.address | address }}{% endif %}{% if invoice.payer.tax_number %}<br>Tax number {{ invoice.payer.tax_number }}{% endif %}{% if invoice.payer.contact %}<br>For the attention of {{ invoice.payer.contact }}{% endif %}</div>
</div>
<div class="meta">
  <div><span class="label">Date</span>{{ invoice.issue_date | date }}</div>
  {% if invoice.kind != "credit_note" and invoice.due_date %}<div><span class="label">Due</span>{{ invoice.due_date | date }}</div>{% endif %}
  {% if invoice.credits_reference %}<div><span class="label">Credits invoice</span>{{ invoice.credits_reference }}</div>{% endif %}
  {% if invoice.booking_title %}<div><span class="label">For</span>{{ invoice.booking_title }}{% if invoice.booking_date %}, {{ invoice.booking_date | date }}{% endif %}</div>{% endif %}
</div>
<table>
  <tr><th>Description</th><th class="num">Qty</th><th class="num">Price</th><th class="num">Tax</th><th class="num">Amount</th></tr>
  {% for l in invoice.line_items %}<tr><td>{{ l.description }}</td><td class="num">{{ l.quantity }}</td><td class="num">{{ l.unit_minor | money(invoice.currency) }}</td><td class="num">{{ l.tax_rate_bps | percent }}</td><td class="num">{{ l.amount_minor | money(invoice.currency) }}</td></tr>{% endfor %}
  <tr class="total"><td colspan="4">Subtotal</td><td class="num">{{ invoice.subtotal | money }}</td></tr>
  {% for t in invoice.tax_lines %}<tr><td colspan="4">{{ t.name }} ({{ t.rate_bps | percent }} of {{ t.net_minor | money(invoice.currency) }})</td><td class="num">{{ t.tax_minor | money(invoice.currency) }}</td></tr>{% endfor %}
  <tr class="grand"><td colspan="4">{% if invoice.kind == "credit_note" %}Total credited{% else %}Total due{% endif %}</td><td class="num">{{ invoice.total | money }}</td></tr>
</table>
{% for t in invoice.tax_lines %}{% if t.note %}<p class="note">{{ t.note }}</p>{% endif %}{% endfor %}
{% if invoice.notes %}<p class="pre">{{ invoice.notes }}</p>{% endif %}
{% if invoice.kind != "credit_note" %}
{% if invoice.bank_account %}<div class="box"><span class="label">Pay by bank transfer{% if invoice.bank_account.label %}: {{ invoice.bank_account.label }}{% endif %}</span><div class="pre">{{ invoice.bank_account.details }}</div>Please quote {{ invoice.reference }}.</div>{% endif %}
{% if invoice.payment_terms_days is not none %}<p>Payment terms: {{ invoice.payment_terms_days }} days.</p>{% endif %}
{% if invoice.late_fee_text %}<p class="note">{{ invoice.late_fee_text }}</p>{% endif %}
{% endif %}
{% if invoice.terms %}<div class="terms note">{{ invoice.terms }}</div>{% endif %}
{% if invoice.footer %}<footer>{{ invoice.footer }}</footer>{% endif %}"#;

const STATEMENT_BODY: &str = r#"<h1>{{ document.title }}</h1>
<div class="meta">
  <div><span class="label">For</span>{{ settlement.talent_name }}</div>
  <div><span class="label">Engagement</span>{{ booking.title }}</div>
  <div><span class="label">Date</span>{{ booking.starts_at | date(none, booking.timezone) }}</div>
  <div><span class="label">Statement date</span>{{ document.date | date }}</div>
</div>
<table>
  <tr><th>Item</th><th class="num">Amount</th></tr>
  <tr><td>Gross fee</td><td class="num">{{ settlement.gross | money }}</td></tr>
  <tr><td>Commission ({{ settlement.commission_bps | percent }})</td><td class="num">-{{ settlement.commission | money }}</td></tr>
  {% for e in settlement.expenses %}<tr><td>{{ e.description }}</td><td class="num">-{{ e.amount_minor | money(settlement.currency) }}</td></tr>{% endfor %}
  <tr class="grand"><td>Due to {{ settlement.talent_name }}</td><td class="num">{{ settlement.net | money }}</td></tr>
</table>
{% if settlement.notes %}<p class="pre">{{ settlement.notes }}</p>{% endif %}
<p class="note">Prepared by {{ issuer.name }}.</p>"#;

/// The templates every workspace starts with.
pub const BUILT_IN: &[BuiltIn] = &[
    BuiltIn {
        key: CONTRACT,
        kind: "contract",
        name: "Booking contract",
        entity: "booking",
        subject: "Contract: {{ booking.title }}",
        body: CONTRACT_BODY,
    },
    BuiltIn {
        key: CONFIRMATION,
        kind: "confirmation",
        name: "Booking confirmation",
        entity: "booking",
        subject: "Booking confirmation: {{ booking.title }}",
        body: CONFIRMATION_BODY,
    },
    BuiltIn {
        key: ADVANCE_SHEET,
        kind: "advance_sheet",
        name: "Advance sheet",
        entity: "booking",
        subject: "Advance sheet: {{ booking.title }}",
        body: ADVANCE_SHEET_BODY,
    },
    BuiltIn {
        key: DAY_SHEET,
        kind: "day_sheet",
        name: "Day sheet",
        entity: "booking",
        subject: "Day sheet: {{ booking.title }}",
        body: DAY_SHEET_BODY,
    },
    BuiltIn {
        key: INVOICE,
        kind: "invoice",
        name: "Invoice",
        entity: "invoice",
        subject: "Invoice {{ invoice.reference }}",
        body: INVOICE_BODY,
    },
    BuiltIn {
        key: CREDIT_NOTE,
        kind: "credit_note",
        name: "Credit note",
        entity: "invoice",
        subject: "Credit note {{ invoice.reference }}",
        body: INVOICE_BODY,
    },
    BuiltIn {
        key: STATEMENT,
        kind: "statement",
        name: "Settlement statement",
        entity: "settlement",
        subject: "Statement: {{ booking.title }}",
        body: STATEMENT_BODY,
    },
];

/// The built-in template with this key.
pub fn built_in(key: &str) -> Option<&'static BuiltIn> {
    BUILT_IN.iter().find(|b| b.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::{Kind, Options, check, render};
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn colours_and_logos_are_checked() {
        assert_eq!(colour("#AA00ff").as_deref(), Some("#aa00ff"));
        assert_eq!(colour("#abc").as_deref(), Some("#abc"));
        assert_eq!(colour("red"), None);
        assert_eq!(colour("#abc; background: url(x)"), None);
        assert!(logo("data:image/png;base64,iVBORw0KGgo=").is_some());
        assert!(logo("https://example.com/logo.png").is_none());
        assert!(logo("data:text/html;base64,PHNjcmlwdD4=").is_none());
        assert!(logo("data:image/png;base64,\"><script>").is_none());
    }

    #[test]
    fn brand_colours_read_on_white() {
        assert_eq!(readable_on_white("#123456").as_deref(), Some("#123456"));
        // A pale yellow is darkened until it reads.
        let dark = readable_on_white("#ffe066").expect("a shade");
        assert_ne!(dark, "#ffe066");
        let rgb = channels(&dark).expect("hex");
        assert!(1.05 / (luminance(rgb) + 0.05) >= 4.5);
        assert_eq!(readable_on_white("nope"), None);
        assert!(font_stack("serif").is_some_and(|f| f.contains("Charter")));
        assert_eq!(font_stack("comic"), None);
    }

    #[test]
    fn pages_carry_the_policy_and_escape_branding() {
        let html = page(
            "A <title>",
            "<p>Body</p>",
            &Branding {
                name: "Rock & <Roll>".into(),
                logo: None,
                primary: Some("#123456".into()),
                header_line: Some("1 High St".into()),
                font: Some("serif".into()),
            },
        );
        assert!(html.contains(POLICY));
        assert!(html.contains("--brand: #123456"));
        assert!(html.contains("Charter"));
        assert!(html.contains("Rock &amp; &lt;Roll&gt;"));
        assert!(html.contains("<title>A &lt;title&gt;</title>"));
        let evil = page(
            "x",
            "",
            &Branding {
                primary: Some("red;}</style><script>".into()),
                ..Default::default()
            },
        );
        assert!(!evil.contains("<script>"));
        assert!(evil.contains("--brand: #1d1d1f"));
    }

    #[test]
    fn built_in_templates_parse_and_render() {
        let ctx = json!({
            "workspace": { "name": "Northern Lights" },
            "issuer": { "name": "Northern Lights Ltd", "address": { "line1": "1 High St", "city": "Leeds" },
                        "email": "hello@example.test" },
            "document": { "title": "Contract", "reference": "D-1", "date": "2031-06-01" },
            "booking": { "title": "Summer party", "starts_at": "2031-06-14T19:00:00Z", "ends_at": "2031-06-14T23:00:00Z",
                         "timezone": "Europe/London", "all_day": false, "venue": "The Hall", "talent_names": "The Rock Lobsters",
                         "fee": { "minor": 250000, "currency": "GBP" }, "deposit_bps": 5000, "status": "pending",
                         "schedule": [{ "label": "Load-in", "at": "2031-06-14T16:00:00Z" }] },
            "talent": { "name": "The Rock Lobsters" },
            "travel": [
                { "kind": "flight", "label": "Flight · BA 1442", "from_code": "LHR", "from_place": "London Heathrow",
                  "to_code": "BCN", "to_place": null, "starts_at": "2031-06-14T06:10:00Z", "start_tz": "Europe/London",
                  "ends_at": "2031-06-14T08:25:00Z", "end_tz": "Europe/Madrid", "reference": "X7YQ2P",
                  "travellers": ["Ada", "Grace"], "notes": null },
                { "kind": "hotel", "status": "planned", "label": "Hotel", "carrier": "Hotel Arts", "to_place": "Carrer de la Marina 19",
                  "starts_at": "2031-06-14T13:00:00Z", "start_tz": "Europe/Madrid", "ends_at": "2031-06-15T10:00:00Z",
                  "end_tz": "Europe/Madrid", "reference": null, "travellers": [], "notes": "Late check-out agreed" }
            ],
            "organisation": { "name": "Party Co" },
            "contact": { "name": "Ada", "emails": ["ada@example.test"], "phones": [] },
            "invoice": { "kind": "invoice", "reference": "INV-2031-0001", "issue_date": "2031-06-01", "due_date": "2031-06-15",
                         "currency": "GBP", "issuer": { "name": "Northern Lights Ltd", "tax_number": "GB123456789" },
                         "payer": { "name": "Party Co" },
                         "line_items": [{ "description": "Deposit", "quantity": 1.0, "unit_minor": 125000, "amount_minor": 125000, "tax_rate_bps": 2000 }],
                         "tax_lines": [{ "name": "VAT 20%", "rate_bps": 2000, "net_minor": 125000, "tax_minor": 25000, "note": null }],
                         "subtotal": { "minor": 125000, "currency": "GBP" }, "total": { "minor": 150000, "currency": "GBP" },
                         "bank_account": { "label": "GBP", "details": "Sort code 00-00-00" }, "payment_terms_days": 14 },
            "settlement": { "talent_name": "The Rock Lobsters", "currency": "GBP", "gross": { "minor": 250000, "currency": "GBP" },
                            "commission_bps": 1500, "commission": { "minor": 37500, "currency": "GBP" },
                            "expenses": [{ "description": "Van", "amount_minor": 5000 }], "net": { "minor": 207500, "currency": "GBP" } },
        });
        let opts = Options {
            locale: "en-GB".into(),
            timezone: chrono_tz::Europe::London,
        };
        for b in BUILT_IN {
            check(Kind::Document, Some(b.subject), b.body)
                .unwrap_or_else(|e| panic!("{}: {e}", b.key));
            let out = render(
                Kind::Document,
                Some(b.subject),
                b.body,
                BTreeMap::new(),
                &ctx,
                &opts,
            )
            .unwrap_or_else(|e| panic!("{}: {e}", b.key));
            assert!(!out.body.trim().is_empty(), "{}", b.key);
            assert!(out.subject.is_some_and(|s| !s.is_empty()), "{}", b.key);
        }
        let day = render(
            Kind::Document,
            None,
            DAY_SHEET_BODY,
            BTreeMap::new(),
            &ctx,
            &opts,
        )
        .expect("renders");
        // Each end of a flight reads in its own zone, as on the ticket.
        assert!(
            day.body.contains("Sat 14 Jun, 07:10 – 10:25"),
            "{}",
            day.body
        );
        assert!(day.body.contains("Flight · BA 1442"));
        assert!(day.body.contains("Hotel Arts"));
        assert!(day.body.contains("Ada, Grace"));
        assert!(day.body.contains("X7YQ2P"));
        assert!(day.body.contains("Hotel (not booked yet)"));
        // References are enough to change a booking: not on pages for the buyer.
        let advance = render(
            Kind::Document,
            None,
            ADVANCE_SHEET_BODY,
            BTreeMap::new(),
            &ctx,
            &opts,
        )
        .expect("renders");
        assert!(advance.body.contains("Flight · BA 1442"));
        assert!(!advance.body.contains("X7YQ2P"));
        let invoice = render(
            Kind::Document,
            None,
            INVOICE_BODY,
            BTreeMap::new(),
            &ctx,
            &opts,
        )
        .expect("renders");
        assert!(invoice.body.contains("INV-2031-0001"));
        assert!(invoice.body.contains("£1,500.00"));
        assert!(invoice.body.contains("VAT 20% (20% of £1,250.00)"));
        assert!(invoice.body.contains("GB123456789"));
        let contract = render(
            Kind::Document,
            None,
            CONTRACT_BODY,
            BTreeMap::new(),
            &ctx,
            &opts,
        )
        .expect("renders");
        assert!(contract.body.contains("A deposit of 50% of the fee"));
        assert!(contract.body.contains("20:00–00:00"), "{}", contract.body);
        let statement = render(
            Kind::Document,
            None,
            STATEMENT_BODY,
            BTreeMap::new(),
            &ctx,
            &opts,
        )
        .expect("renders");
        assert!(statement.body.contains("£2,075.00"));
        assert!(statement.body.contains("Commission (15%)"));
    }

    #[test]
    fn certificates_list_every_signer() {
        let html = certificate(
            "Contract",
            "D-7",
            "ab12",
            &[SignatureRecord {
                name: "Ada".into(),
                email: "ada@example.test".into(),
                signed_name: "Ada Lovelace".into(),
                signed_at: "2031-06-01T10:00:00Z".into(),
                ip: Some("203.0.113.9".into()),
                user_agent: Some("<Firefox>".into()),
            }],
            &Branding::default(),
        );
        assert!(html.contains("Ada Lovelace"));
        assert!(html.contains("203.0.113.9"));
        assert!(html.contains("&lt;Firefox&gt;"));
        assert!(html.contains("ab12"));
    }
}
