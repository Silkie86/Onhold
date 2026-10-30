//! Suggesting fields from a document's text with fixed patterns, no AI.
//!
//! The approach follows invoice2data and most hand-written invoice readers:
//! look for a known label (`Invoice number`, `Due date`, `Total`) and read
//! the value next to it, on the same line or the line below, with a parser
//! for that kind of value. Values with a structure of their own (emails,
//! IBANs, UK VAT numbers) are also found without a label, and the ones with
//! check digits are verified. Everything comes back as a suggestion with the
//! text it was read from, ranked, for a person to confirm.

use std::collections::HashMap;
use std::sync::LazyLock;

use chrono::NaiveDate;
use core_domain::Money;
use core_engines::import::{DateOrder, parse_date, parse_money};
use regex::Regex;
use serde::{Deserialize, Serialize};

/// The fields we look for.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    InvoiceNumber,
    OrderNumber,
    IssueDate,
    DueDate,
    StartDate,
    EndDate,
    SignedDate,
    Subtotal,
    Tax,
    Total,
    Email,
    Phone,
    VatNumber,
    CompanyNumber,
    Iban,
    /// The other side: who the invoice is to, or a party to a contract.
    Party,
    /// Who issued the document.
    Issuer,
}

/// The kind of value a field holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ValueKind {
    Text,
    Reference,
    Date,
    Money,
}

impl Field {
    pub fn kind(self) -> ValueKind {
        match self {
            Field::InvoiceNumber | Field::OrderNumber | Field::VatNumber | Field::CompanyNumber => {
                ValueKind::Reference
            }
            Field::IssueDate
            | Field::DueDate
            | Field::StartDate
            | Field::EndDate
            | Field::SignedDate => ValueKind::Date,
            Field::Subtotal | Field::Tax | Field::Total => ValueKind::Money,
            Field::Email | Field::Phone | Field::Iban | Field::Party | Field::Issuer => {
                ValueKind::Text
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Value {
    Text(String),
    Date(NaiveDate),
    Money(Money),
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

/// Where in the text a suggestion came from, so the screen can show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Source {
    /// From 1.
    pub page: u32,
    /// From 1, within the page.
    pub line: u32,
    /// The line as written.
    pub text: String,
    /// The label the value was found by, if any (`Invoice No.`).
    pub label: Option<String>,
    /// The template that found it, if any.
    pub template: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Suggestion {
    /// A [`Field`] in snake case, or `custom.<key>` from a template.
    pub field: String,
    pub value: Value,
    pub confidence: Confidence,
    pub source: Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    Invoice,
    CreditNote,
    Receipt,
    Quote,
    PurchaseOrder,
    Statement,
    Contract,
}

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct KindGuess {
    pub kind: DocumentKind,
    /// The share of keyword evidence for this kind, 0 to 1.
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct Suggestions {
    /// Likely kinds of document, best first.
    pub kinds: Vec<KindGuess>,
    /// Best first within each field; fields in [`Field`] order.
    pub fields: Vec<Suggestion>,
}

impl Suggestions {
    /// The best suggestion for a field, if any.
    pub fn best(&self, field: &str) -> Option<&Suggestion> {
        self.fields.iter().find(|s| s.field == field)
    }

    /// Every suggestion for a field, best first.
    pub fn all<'a>(&'a self, field: &'a str) -> impl Iterator<Item = &'a Suggestion> {
        self.fields.iter().filter(move |s| s.field == field)
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub date_order: DateOrder,
    /// For amounts that name no currency, when the document names none
    /// either.
    pub default_currency: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            date_order: DateOrder::Dmy,
            default_currency: "GBP".into(),
        }
    }
}

/// Labels for each field, matched case-insensitively as whole words.
/// Overlapping matches keep the longest, so `Sub-total` is not also read as
/// `total`, nor `Total VAT` as `Total`.
const LABELS: &[(Field, Confidence, &[&str])] = &[
    (
        Field::InvoiceNumber,
        Confidence::High,
        &[
            "invoice number",
            "invoice no",
            "invoice num",
            "invoice #",
            "invoice ref",
            "invoice reference",
            "invoice id",
            "inv no",
            "inv #",
            "credit note number",
            "credit note no",
            "receipt number",
            "receipt no",
            "bill number",
            "bill no",
            "document number",
            "document no",
            "quote number",
            "quote no",
            "quotation number",
            "quotation no",
            "estimate number",
            "estimate no",
            "rechnungsnummer",
            "numéro de facture",
            "factuurnummer",
        ],
    ),
    (
        Field::InvoiceNumber,
        Confidence::Medium,
        &["invoice", "reference", "ref", "our ref"],
    ),
    (
        Field::OrderNumber,
        Confidence::High,
        &[
            "purchase order number",
            "purchase order no",
            "purchase order",
            "po number",
            "po no",
            "p.o. number",
            "p.o. no",
            "po #",
            "p.o.",
            "order number",
            "order no",
            "order #",
            "your ref",
            "your reference",
            "customer ref",
        ],
    ),
    (
        Field::IssueDate,
        Confidence::High,
        &[
            "invoice date",
            "date of invoice",
            "date of issue",
            "issue date",
            "issued on",
            "date issued",
            "tax point",
            "tax date",
            "billing date",
            "credit note date",
            "receipt date",
            "quote date",
            "rechnungsdatum",
        ],
    ),
    (Field::IssueDate, Confidence::Medium, &["date", "issued"]),
    (
        Field::DueDate,
        Confidence::High,
        &[
            "due date",
            "payment due",
            "payment due date",
            "date due",
            "due by",
            "due on",
            "pay by",
            "please pay by",
            "payable by",
        ],
    ),
    (Field::DueDate, Confidence::Medium, &["due"]),
    (
        Field::StartDate,
        Confidence::High,
        &[
            "start date",
            "commencement date",
            "effective date",
            "effective from",
            "commencing",
            "commences on",
            "with effect from",
            "term start",
            "date of commencement",
        ],
    ),
    (
        Field::EndDate,
        Confidence::High,
        &[
            "end date",
            "expiry date",
            "expiration date",
            "termination date",
            "expires on",
            "expires",
            "term end",
            "term ends",
            "until",
            "valid until",
            "end of term",
        ],
    ),
    (
        Field::SignedDate,
        Confidence::High,
        &[
            "date signed",
            "signed on",
            "signature date",
            "date of signature",
            "signed this",
        ],
    ),
    (Field::SignedDate, Confidence::Medium, &["dated", "signed"]),
    (
        Field::Subtotal,
        Confidence::High,
        &[
            "subtotal",
            "sub total",
            "sub-total",
            "net total",
            "total net",
            "net amount",
            "total excl",
            "total excluding",
            "total ex vat",
            "total before tax",
            "amount before vat",
            "net",
        ],
    ),
    (
        Field::Tax,
        Confidence::High,
        &[
            "total vat",
            "vat total",
            "vat amount",
            "total tax",
            "tax amount",
            "sales tax",
            "vat",
            "gst",
            "tax",
            "mwst",
            "tva",
            "btw",
        ],
    ),
    (
        Field::Total,
        Confidence::High,
        &[
            "grand total",
            "total due",
            "amount due",
            "balance due",
            "total payable",
            "amount payable",
            "total to pay",
            "invoice total",
            "total amount",
            "total incl",
            "total including",
            "total inc vat",
            "total (inc vat)",
            "balance",
            "gesamtbetrag",
            "montant total",
        ],
    ),
    (Field::Total, Confidence::Medium, &["total"]),
    (
        Field::VatNumber,
        Confidence::High,
        &[
            "vat number",
            "vat no",
            "vat reg no",
            "vat reg",
            "vat registration number",
            "vat registration no",
            "vat id",
            "vat #",
            "ust-idnr",
            "tax id",
            "gst number",
            "gst no",
        ],
    ),
    (
        Field::CompanyNumber,
        Confidence::High,
        &[
            "company number",
            "company no",
            "company registration number",
            "company reg no",
            "registered number",
            "registered no",
            "registration number",
            "registration no",
            "crn",
        ],
    ),
    (
        Field::Party,
        Confidence::Medium,
        &[
            "bill to",
            "billed to",
            "invoice to",
            "invoiced to",
            "sold to",
            "customer",
            "client",
            "to",
        ],
    ),
    (
        Field::Issuer,
        Confidence::Medium,
        &[
            "from",
            "supplier",
            "seller",
            "issued by",
            "payable to",
            "pay to",
            "make cheques payable to",
        ],
    ),
];

struct Label {
    field: Field,
    confidence: Confidence,
    text: &'static str,
    regex: Regex,
}

static LABEL_SET: LazyLock<Vec<Label>> = LazyLock::new(|| {
    let mut labels: Vec<Label> = LABELS
        .iter()
        .flat_map(|(field, confidence, texts)| {
            texts.iter().map(move |text| {
                // Whole words, any run of spaces, optional dots after
                // abbreviations; must be followed by a separator or the end.
                let words: Vec<String> = text
                    .split(' ')
                    .map(|w| regex::escape(w.trim_end_matches('.')) + r"\.?")
                    .collect();
                let first = text.chars().next().is_some_and(char::is_alphanumeric);
                let pattern = format!(
                    r"(?i){}{}(?:[\s:.#)\-]|$)",
                    if first { r"\b" } else { "" },
                    words.join(r"\s+")
                );
                Label {
                    field: *field,
                    confidence: *confidence,
                    text,
                    #[allow(clippy::unwrap_used)] // Built from fixed, escaped text.
                    regex: Regex::new(&pattern).unwrap(),
                }
            })
        })
        .collect();
    // Longest first, so overlap resolution keeps the most specific.
    labels.sort_by_key(|l| std::cmp::Reverse(l.text.len()));
    labels
});

/// All label patterns at once, in [`LABEL_SET`] order.
static LABEL_FILTER: LazyLock<regex::RegexSet> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // The same patterns as LABEL_SET.
    regex::RegexSet::new(LABEL_SET.iter().map(|l| l.regex.as_str())).unwrap()
});

#[allow(clippy::unwrap_used)] // Fixed patterns, checked by the tests.
fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap()
}

static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b[a-z0-9._%+\-]+@[a-z0-9](?:[a-z0-9\-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9\-]*[a-z0-9])?)*\.[a-z]{2,}\b",
    )
});
static PHONE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?:\+\d{1,3}[\s\-.]?)?(?:\(0\)\s?)?(?:\(?\d{2,5}\)?[\s\-.]?){2,4}\d{2,4}")
});
static IBAN: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b[A-Z]{2}\d{2}(?:\s?[A-Z0-9]{4}){2,7}(?:\s?[A-Z0-9]{1,3})?\b"));
static GB_VAT: LazyLock<Regex> = LazyLock::new(|| re(r"\bGB\s?(\d{3})\s?(\d{4})\s?(\d{2})\b"));
static EU_VAT: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b(?:GB|XI|[A-Z]{2})\s?[0-9A-Z][0-9A-Z\s]{6,13}[0-9A-Z]\b"));
static COMPANY_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b(?:\d{8}|(?:SC|NI|OC|SO|NC|R0|FC)\d{6})\b"));
static REFERENCE: LazyLock<Regex> =
    LazyLock::new(|| re(r"[A-Za-z0-9][A-Za-z0-9\-/_.]*[A-Za-z0-9]|[0-9]"));
static INVOICE_LIKE: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b(?:INV|INVOICE|CN|SI)[\-/#]?\s?\d[\w\-/]*\b"));
/// An amount: optional currency code or symbol either side, digits with
/// thousands separators, an optional decimal part.
static AMOUNT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?x)
        \(?
        (?:\b[A-Z]{3}\s?|US\$|A\$|C\$|NZ\$|HK\$|R\$|[£€$¥₹])?\s?
        -?\(?
        \d{1,3}(?:[,.\s']\d{3})*(?:[.,]\d{1,3})?
        \)?
        (?:\s?(?:[A-Z]{3}\b|[£€$¥₹]|zł))?
        (?:\s?%)?")
});
static DATE_NUMERIC: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(?:\d{4}[\-/.]\d{1,2}[\-/.]\d{1,2}|\d{1,2}[\-/.]\d{1,2}[\-/.](?:\d{4}|\d{2}))\b")
});
static DATE_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?ix)
        \b(?:
          (\d{1,2})(?:st|nd|rd|th)?(?:\s+day)?(?:\s+of)?[\s\-.]+([a-z]{3,9})\.?,?[\s\-.]+(\d{4})
        | ([a-z]{3,9})\.?[\s\-.]+(\d{1,2})(?:st|nd|rd|th)?,?\s+(\d{4})
        )\b")
});
static BETWEEN: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\bbetween\b\s*:?\s*"));
/// Where the second party starts: "and" after the first party's closing
/// bracket or comma, so "England and Wales" inside a description of the
/// first party does not split it.
static SECOND_PARTY: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)(?:\)\s*,?|,|;|\n)\s*and\s+"));
static AND: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\band\s+"));
static SPACES: LazyLock<Regex> = LazyLock::new(|| re(r"[ \t]+"));
static DECIMALS: LazyLock<Regex> = LazyLock::new(|| re(r"[.,]\d{2}\)?(?:\s|$|[A-Z£€$])"));
static PHONE_LABEL: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\b(?:tel|telephone|phone|mobile|mob|t)\b\.?\s*:?"));
static REGISTERED: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)registered in|company (?:no|number)|registration"));
static PARTY_NUMBER: LazyLock<Regex> = LazyLock::new(|| re(r"^\(?\d\)?\.?\s+"));
static COMPANY_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(?:ltd|limited|llp|plc|inc|incorporated|llc|gmbh|ag|s\.?a\.?r?\.?l?|b\.?v\.?|pty|corp|corporation|company|co\.)\.?(?:\s|$|,)",
    )
});

fn month(name: &str) -> Option<u32> {
    let n = name.to_ascii_lowercase();
    let months = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    if n.len() < 3 {
        return None;
    }
    let i = months.iter().position(|m| n.starts_with(m))?;
    let month = u32::try_from(i + 1).ok()?;
    // Accept the abbreviation or a prefix of the full name, `Sept` too.
    let full = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    (full[i].starts_with(&n) || n == "sept").then_some(month)
}

/// Finds the first date in `s` and where it ends.
fn find_date(s: &str, order: DateOrder) -> Option<(NaiveDate, usize, usize)> {
    let words = DATE_WORDS.captures(s).and_then(|c| {
        let m = c.get(0)?;
        let date = if let Some(d) = c.get(1) {
            NaiveDate::from_ymd_opt(c[3].parse().ok()?, month(&c[2])?, d.as_str().parse().ok()?)
        } else {
            NaiveDate::from_ymd_opt(c[6].parse().ok()?, month(&c[4])?, c[5].parse().ok()?)
        }?;
        Some((date, m.start(), m.end()))
    });
    let numeric = DATE_NUMERIC
        .find_iter(s)
        .find_map(|m| parse_date(m.as_str(), order).map(|d| (d, m.start(), m.end())));
    match (words, numeric) {
        (Some(a), Some(b)) => Some(if a.1 <= b.1 { a } else { b }),
        (a, b) => a.or(b),
    }
}

/// Finds amounts in `s`, skipping percentages and things that are dates.
fn find_amounts(s: &str, currency: &str) -> Vec<(Money, usize)> {
    let mut out = Vec::new();
    for m in AMOUNT.find_iter(s) {
        let text = m.as_str().trim();
        if text.ends_with('%') || text.is_empty() {
            continue;
        }
        // A bare small integer is a quantity or a percentage more often than
        // an amount; insist on a decimal part or a currency mark.
        let has_currency = text.chars().any(|c| "£€$¥₹".contains(c))
            || text
                .split(|c: char| !c.is_ascii_alphabetic())
                .any(|w| w.len() == 3 && core_domain::workspace::validate_currency(w).is_ok());
        let has_decimals = DECIMALS.is_match(text);
        if !has_currency && !has_decimals {
            continue;
        }
        let negative = text.contains('(') && text.contains(')');
        let clean = text.replace(['(', ')'], "");
        if let Ok(mut money) = parse_money(&clean, currency) {
            if negative {
                money.minor = -money.minor.abs();
            }
            out.push((money, m.start()));
        }
    }
    out
}

/// Reads a reference (invoice number and the like): the first token with a
/// digit in it that is not a date or an amount.
fn find_reference(s: &str, order: DateOrder) -> Option<String> {
    let s = s.trim_start_matches(|c: char| c.is_whitespace() || ":#.-".contains(c));
    // Only look at the first column: a wide gap ends the value.
    let first = s.split("   ").next().unwrap_or(s);
    for m in REFERENCE.find_iter(first).take(3) {
        let tok = m.as_str();
        if tok.chars().any(|c| c.is_ascii_digit())
            && parse_date(tok, order).is_none()
            && tok.len() <= 40
        {
            return Some(tok.to_owned());
        }
        // Skip filler words such as "No" or "Number" left after a short label.
        if !matches!(
            tok.to_ascii_lowercase().as_str(),
            "no" | "number" | "num" | "nr" | "ref"
        ) {
            return None;
        }
    }
    None
}

/// The text that follows a label up to the next column, cleaned of the
/// separator.
fn after_label(rest: &str) -> &str {
    rest.trim_start_matches(|c: char| {
        c == ':' || c == '#' || c == '-' || c == '.' || c.is_whitespace()
    })
}

/// Checks an IBAN's mod-97 check digits.
pub fn valid_iban(iban: &str) -> bool {
    let s: String = iban.chars().filter(|c| !c.is_whitespace()).collect();
    if !(15..=34).contains(&s.len()) || !s.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    let (head, tail) = s.split_at(4);
    let mut rem: u32 = 0;
    for c in tail.chars().chain(head.chars()) {
        let Some(digits) = c.to_digit(36) else {
            return false;
        };
        rem = if digits >= 10 {
            (rem * 100 + digits) % 97
        } else {
            (rem * 10 + digits) % 97
        };
    }
    rem == 1
}

/// Checks a UK VAT number's check digits (the weighted mod-97 rule, old and
/// "9755" variants).
pub fn valid_gb_vat(digits: &str) -> bool {
    let d: Vec<u32> = digits.chars().filter_map(|c| c.to_digit(10)).collect();
    if d.len() != 9 && d.len() != 12 {
        return false;
    }
    let sum: u32 = d[..7]
        .iter()
        .zip((2..=8).rev())
        .map(|(x, w)| x * w)
        .sum::<u32>()
        + d[7] * 10
        + d[8];
    sum.is_multiple_of(97) || (sum + 55).is_multiple_of(97)
}

#[derive(Debug)]
struct Line<'a> {
    page: u32,
    number: u32,
    text: &'a str,
}

fn lines<'a>(pages: &[&'a str]) -> Vec<Line<'a>> {
    pages
        .iter()
        .zip(1..)
        .flat_map(|(text, page)| {
            text.lines().zip(1..).map(move |(l, number)| Line {
                page,
                number,
                text: l,
            })
        })
        .collect()
}

/// The document's own currency: the code or symbol written most often.
fn document_currency(text: &str, default: &str) -> String {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for m in AMOUNT.find_iter(text) {
        let t = m.as_str();
        for (sym, code) in [
            ("£", "GBP"),
            ("€", "EUR"),
            ("US$", "USD"),
            ("$", "USD"),
            ("¥", "JPY"),
            ("₹", "INR"),
        ] {
            if t.contains(sym) {
                *counts.entry(code.to_owned()).or_default() += 1;
                break;
            }
        }
        for w in t.split(|c: char| !c.is_ascii_uppercase()) {
            if w.len() == 3 && core_domain::workspace::validate_currency(w).is_ok() {
                *counts.entry(w.to_owned()).or_default() += 1;
            }
        }
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
        .map(|(c, _)| c)
        .unwrap_or_else(|| default.to_owned())
}

/// Scores the document against keywords for each kind. Words in the first
/// few lines (the title) count three times.
pub fn guess_kind(text: &str) -> Vec<KindGuess> {
    const KEYWORDS: &[(DocumentKind, &[&str])] = &[
        (
            DocumentKind::Invoice,
            &[
                "invoice",
                "tax invoice",
                "amount due",
                "payment terms",
                "bill to",
            ],
        ),
        (DocumentKind::CreditNote, &["credit note", "credit memo"]),
        (
            DocumentKind::Receipt,
            &[
                "receipt",
                "paid with",
                "amount paid",
                "thank you for your payment",
            ],
        ),
        (
            DocumentKind::Quote,
            &["quote", "quotation", "estimate", "valid until"],
        ),
        (DocumentKind::PurchaseOrder, &["purchase order"]),
        (
            DocumentKind::Statement,
            &["statement of account", "statement", "opening balance"],
        ),
        (
            DocumentKind::Contract,
            &[
                "agreement",
                "contract",
                "hereby",
                "whereas",
                "the parties",
                "terms and conditions",
                "in witness whereof",
                "signed by",
                "governed by",
                "term of",
                "shall",
            ],
        ),
    ];
    let lower = text.to_lowercase();
    let head: String = lower
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(5)
        .collect::<Vec<_>>()
        .join("\n");
    let count = |hay: &str, needle: &str| -> usize {
        // Whole-word count without a regex per keyword.
        hay.match_indices(needle)
            .filter(|(i, _)| {
                let before = hay[..*i].chars().next_back();
                let after = hay[i + needle.len()..].chars().next();
                !before.is_some_and(char::is_alphanumeric)
                    && !after.is_some_and(char::is_alphanumeric)
            })
            .count()
    };
    let mut scores: Vec<(DocumentKind, f32)> = KEYWORDS
        .iter()
        .map(|(kind, words)| {
            let s: usize = words
                .iter()
                .map(|w| count(&lower, w).min(5) + 3 * count(&head, w).min(1))
                .sum();
            (*kind, s as f32)
        })
        .collect();
    // "Purchase order number" on an invoice should not make it an order, nor
    // a credit note's "invoice" reference an invoice: the specific kinds
    // also contain the general word.
    let take = |scores: &mut Vec<(DocumentKind, f32)>, kind, from| {
        let s = scores
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|x| x.1)
            .unwrap_or(0.0);
        if s > 0.0
            && let Some(e) = scores.iter_mut().find(|(k, _)| *k == from)
        {
            e.1 = (e.1 - s).max(0.0);
        }
    };
    take(&mut scores, DocumentKind::CreditNote, DocumentKind::Invoice);
    let total: f32 = scores.iter().map(|s| s.1).sum();
    if total == 0.0 {
        return Vec::new();
    }
    scores.retain(|s| s.1 > 0.0);
    scores.sort_by(|a, b| b.1.total_cmp(&a.1));
    scores
        .into_iter()
        .map(|(kind, s)| KindGuess {
            kind,
            score: (s / total * 100.0).round() / 100.0,
        })
        .collect()
}

struct Found {
    field: Field,
    value: Value,
    confidence: Confidence,
    page: u32,
    line: u32,
    text: String,
    label: Option<String>,
}

fn starts_with_label(s: &str) -> bool {
    LABEL_FILTER
        .matches(s)
        .into_iter()
        .any(|i| LABEL_SET[i].regex.find(s).is_some_and(|m| m.start() == 0))
}

fn is_mostly_label(s: &str) -> bool {
    // A value that is itself another label ("Date", "Total") is not a value.
    let t = s.trim().to_ascii_lowercase();
    LABEL_SET.iter().any(|l| l.text == t)
}

/// Reads a field's value from the text after its label, or, if there is
/// none, from the line below in roughly the same column.
fn read_value(
    field: Field,
    rest: &str,
    below: Option<&str>,
    column: usize,
    opts: &Options,
    currency: &str,
) -> Option<(Value, bool)> {
    let here = after_label(rest);
    let candidates: Vec<(&str, bool)> = {
        let mut v = vec![(here, false)];
        // Only a label alone in its column takes its value from below.
        let alone = here.split("   ").next().unwrap_or("").trim().is_empty();
        if let Some(below) = below.filter(|_| alone) {
            // The same column of the next line: from a little left of the
            // label to the next wide gap.
            let start = below
                .char_indices()
                .map(|(i, _)| i)
                .find(|&i| i + 4 >= column)
                // A short line below a label at the end of a line is the
                // sentence wrapping: read it from the start.
                .unwrap_or(0);
            let seg = below[start..].trim_start();
            let seg = seg.split("   ").next().unwrap_or(seg);
            // A line that starts with a label of its own is not this
            // label's value ("Subtotal" with its amount lost, then "VAT").
            if !starts_with_label(seg) {
                v.push((seg, true));
            }
        }
        v
    };
    for (text, is_below) in candidates {
        if text.trim().is_empty() {
            continue;
        }
        let value = match field.kind() {
            ValueKind::Date => find_date(text, opts.date_order).map(|(d, _, _)| Value::Date(d)),
            ValueKind::Money => {
                // Totals sit at the right: take the last amount on the line.
                find_amounts(text, currency)
                    .pop()
                    .map(|(m, _)| Value::Money(m))
            }
            ValueKind::Reference => {
                let first = text.split("   ").next().unwrap_or(text);
                if field == Field::VatNumber {
                    EU_VAT
                        .find(first)
                        .map(|m| Value::Text(m.as_str().split_whitespace().collect::<String>()))
                } else {
                    find_reference(first, opts.date_order).map(Value::Text)
                }
            }
            ValueKind::Text => {
                let first = text.split("   ").next().unwrap_or(text).trim();
                let first = first.trim_end_matches([',', ';']);
                // A name, not a sentence: short, not just another label.
                (first.chars().any(char::is_alphabetic)
                    && first.len() <= 80
                    && first.split_whitespace().count() <= 8
                    && !is_mostly_label(first))
                .then(|| Value::Text(first.to_owned()))
            }
        };
        if let Some(v) = value {
            return Some((v, is_below));
        }
    }
    None
}

/// Finds field suggestions in the pages' text (one string per page, as
/// [`crate::Extracted`] gives them).
pub fn suggest(pages: &[&str], opts: &Options) -> Suggestions {
    let all_text = pages.join("\n");
    let currency = document_currency(&all_text, &opts.default_currency);
    let lines = lines(pages);
    let mut found: Vec<Found> = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let below = lines
            .get(i + 1)
            .filter(|l| l.page == line.page && !l.text.trim().is_empty())
            .map(|l| l.text);

        // Label matches on this line, longest first, without overlaps.
        let mut taken: Vec<(usize, usize)> = Vec::new();
        let mut matches: Vec<(&Label, usize, usize)> = Vec::new();
        // One pass over the line says which labels are on it at all.
        for label in LABEL_FILTER
            .matches(line.text)
            .into_iter()
            .map(|i| &LABEL_SET[i])
        {
            for m in label.regex.find_iter(line.text) {
                let (s, e) = (
                    m.start(),
                    m.start() + label_len(line.text, m.start(), m.end()),
                );
                if taken.iter().any(|&(ts, te)| s < te && ts < e) {
                    continue;
                }
                taken.push((s, e));
                matches.push((label, s, e));
            }
        }
        matches.sort_by_key(|m| m.1);
        for (idx, &(label, s, e)) in matches.iter().enumerate() {
            // The value runs to the next label on the line.
            let next = matches.get(idx + 1).map(|m| m.1).unwrap_or(line.text.len());
            let rest = &line.text[e..next];
            // A short generic label ("to", "date", "net") must be the first
            // thing in its column, so prose ("pay to the order of", "dated
            // as above") does not count.
            let starts_column = line.text[..s].trim().is_empty() || line.text[..s].ends_with("   ");
            let generic = label.confidence < Confidence::High
                || (!label.text.contains(' ') && label.text.len() <= 5)
                || matches!(label.field, Field::Party | Field::Issuer);
            // In prose, a date may still follow closely: "dated 1st
            // February 2023", "signed by the parties on 3 May 2024".
            let date_follows = label.field.kind() == ValueKind::Date
                && find_date(rest, opts.date_order)
                    .is_some_and(|(_, at, _)| at <= 40 && !rest[..at].contains(". "));
            if generic && !starts_column && !date_follows {
                continue;
            }
            // The VAT label also starts "VAT number": skip tax amounts that
            // are really registration numbers.
            if label.field == Field::Tax
                && rest
                    .trim_start()
                    .to_ascii_lowercase()
                    .starts_with(['n', 'r', 'i'])
            {
                continue;
            }
            let Some((value, from_below)) =
                read_value(label.field, rest, below, s, opts, &currency)
            else {
                continue;
            };
            let (page, number, text) = if from_below {
                let l = &lines[i + 1];
                (l.page, l.number, l.text.trim().to_owned())
            } else {
                (line.page, line.number, line.text.trim().to_owned())
            };
            found.push(Found {
                field: label.field,
                value,
                confidence: label.confidence,
                page,
                line: number,
                text,
                label: Some(line.text[s..e].trim().to_owned()),
            });
        }

        // Unlabelled values with a shape of their own.
        for m in EMAIL.find_iter(line.text) {
            found.push(unlabelled(
                line,
                Field::Email,
                m.as_str().to_lowercase(),
                Confidence::High,
            ));
        }
        for m in IBAN.find_iter(line.text) {
            if valid_iban(m.as_str()) {
                let iban: String = m.as_str().split_whitespace().collect();
                found.push(unlabelled(line, Field::Iban, iban, Confidence::High));
            }
        }
        for c in GB_VAT.captures_iter(line.text) {
            let digits = format!("{}{}{}", &c[1], &c[2], &c[3]);
            if valid_gb_vat(&digits) {
                found.push(unlabelled(
                    line,
                    Field::VatNumber,
                    format!("GB{digits}"),
                    Confidence::High,
                ));
            }
        }
        if PHONE_LABEL.is_match(line.text)
            && let Some(m) = PHONE.find_iter(line.text).find(|m| {
                let digits = m.as_str().chars().filter(char::is_ascii_digit).count();
                (9..=15).contains(&digits) && find_date(m.as_str(), opts.date_order).is_none()
            })
        {
            found.push(unlabelled(
                line,
                Field::Phone,
                m.as_str().trim().to_owned(),
                Confidence::Medium,
            ));
        }
        if REGISTERED.is_match(line.text)
            && let Some(m) = COMPANY_NUMBER.find(line.text)
        {
            found.push(unlabelled(
                line,
                Field::CompanyNumber,
                m.as_str().to_owned(),
                Confidence::Medium,
            ));
        }
        for m in INVOICE_LIKE.find_iter(line.text) {
            found.push(unlabelled(
                line,
                Field::InvoiceNumber,
                m.as_str().to_owned(),
                Confidence::Low,
            ));
        }
        // A company name on a line of its own near the top is likely the
        // issuer (letterheads come first).
        let trimmed = line.text.trim();
        if line.page == 1
            && line.number <= 8
            && COMPANY_SUFFIX.is_match(trimmed)
            && trimmed.len() <= 60
            && !trimmed.contains("   ")
        {
            found.push(unlabelled(
                line,
                Field::Issuer,
                trimmed.to_owned(),
                Confidence::Low,
            ));
        }
    }

    // "This agreement is made between A ("the Agency") and B": contract
    // parties. Read from the text with lines joined, as the sentence often
    // wraps.
    let joined = SPACES.replace_all(&all_text, " ");
    for (name, at, snippet) in between_parties(&joined) {
        let (page, line) = locate(pages, &joined, at);
        found.push(Found {
            field: Field::Party,
            value: Value::Text(name),
            confidence: Confidence::Medium,
            page,
            line,
            text: snippet,
            label: Some("between".into()),
        });
    }

    Suggestions {
        kinds: guess_kind(&all_text),
        fields: rank(found),
    }
}

/// The two parties named in "made between A (...) and B (...)", with
/// where each starts and the sentence they came from.
fn between_parties(text: &str) -> Vec<(String, usize, String)> {
    let Some(m) = BETWEEN.find(text) else {
        return Vec::new();
    };
    let start = m.end();
    let mut end = (start + 600).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let rest = &text[start..end];
    let name_end = |s: &str| -> usize {
        let lower = s.to_ascii_lowercase();
        [",", "(", ";", "\n\n", " and ", ". "]
            .iter()
            .filter_map(|p| {
                if *p == " and " {
                    lower.find(p)
                } else {
                    s.find(p)
                }
            })
            .min()
            .unwrap_or(s.len())
    };
    let first_skip = PARTY_NUMBER.find(rest).map_or(0, |m| m.end());
    let first_end = first_skip + name_end(&rest[first_skip..]);
    let first = clean_party(&rest[first_skip..first_end]);
    let after = &rest[first_end..];
    let second_at = SECOND_PARTY
        .find(after)
        .or_else(|| AND.find(after))
        .map(|m| first_end + m.end());
    let mut out = Vec::new();
    if !first.is_empty() {
        out.push((first, start + first_skip, String::new()));
    }
    let mut sentence_end = rest.len();
    if let Some(at) = second_at {
        let skip = PARTY_NUMBER.find(&rest[at..]).map_or(0, |m| m.end());
        let from = at + skip;
        let to = from + name_end(&rest[from..]);
        sentence_end = to;
        let second = clean_party(&rest[from..to]);
        if !second.is_empty() {
            out.push((second, start + from, String::new()));
        }
    }
    let snippet = text[m.start()..start + sentence_end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for p in &mut out {
        p.2 = snippet.clone();
    }
    out
}

/// The matched label's length without the separator the regex consumed
/// (spaces, a colon, `#` or `-`). Dots and brackets stay, as in `p.o.` and
/// `Total (inc VAT)`.
fn label_len(text: &str, start: usize, end: usize) -> usize {
    let m = &text[start..end];
    let trimmed = m.trim_end_matches(|c: char| c.is_whitespace() || ":#-".contains(c));
    trimmed.len().max(1)
}

fn clean_party(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let s = s
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim_end_matches(|c: char| c == ',' || c == ';' || c == '.' || c.is_whitespace());
    // Stop at a clause that describes the party rather than names it.
    let cut = [
        ", a company",
        " a company",
        " of ",
        " whose ",
        " registered ",
        " (company",
        " trading as",
        " t/a ",
    ]
    .iter()
    .filter_map(|p| s.to_ascii_lowercase().find(p))
    .min()
    .unwrap_or(s.len());
    let name = s[..cut].trim().trim_end_matches(',');
    if name.split_whitespace().count() > 10 || name.len() < 2 {
        String::new()
    } else {
        name.to_owned()
    }
}

/// The page and line of a byte offset into `pages.join("\n")` (or a copy
/// of it with only spaces changed).
pub(crate) fn locate(pages: &[&str], joined: &str, at: usize) -> (u32, u32) {
    let mut n = joined[..at.min(joined.len())].matches('\n').count();
    let last = pages.len();
    for (p, page) in pages.iter().zip(1..) {
        // The page's own line breaks, plus the one `join` adds after it.
        let breaks = p.matches('\n').count() + 1;
        if n < breaks || usize::try_from(page).is_ok_and(|page| page == last) {
            return (page, u32::try_from(n + 1).unwrap_or(u32::MAX));
        }
        n -= breaks;
    }
    (1, 1)
}

/// Reads a date written any way [`suggest`] understands.
pub(crate) fn read_date(s: &str, order: DateOrder) -> Option<NaiveDate> {
    find_date(s, order).map(|d| d.0)
}

/// Reads an amount; a bare number counts here, as a template's pattern has
/// already said where the amount is.
pub(crate) fn read_money(s: &str, currency: &str) -> Option<Money> {
    find_amounts(s, currency)
        .pop()
        .map(|m| m.0)
        .or_else(|| parse_money(s, currency).ok())
}

fn unlabelled(line: &Line<'_>, field: Field, value: String, confidence: Confidence) -> Found {
    Found {
        field,
        value: Value::Text(value),
        confidence,
        page: line.page,
        line: line.number,
        text: line.text.trim().to_owned(),
        label: None,
    }
}

fn field_name(field: Field) -> String {
    serde_json::to_value(field)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Merges duplicates (same field and value, keeping the best evidence) and
/// orders by field, then confidence, then position in the document. For
/// totals, a larger amount at the same confidence ranks first: the grand
/// total is the largest of the "total" lines.
fn rank(found: Vec<Found>) -> Vec<Suggestion> {
    let mut merged: Vec<Found> = Vec::new();
    for f in found {
        if let Some(existing) = merged
            .iter_mut()
            .find(|m| m.field == f.field && same_value(&m.value, &f.value))
        {
            if f.confidence > existing.confidence {
                *existing = f;
            }
        } else {
            merged.push(f);
        }
    }
    merged.sort_by(|a, b| {
        a.field
            .cmp(&b.field)
            .then(b.confidence.cmp(&a.confidence))
            .then_with(|| match (a.field, &a.value, &b.value) {
                (Field::Total, Value::Money(x), Value::Money(y)) => y.minor.cmp(&x.minor),
                _ => std::cmp::Ordering::Equal,
            })
            .then((a.page, a.line).cmp(&(b.page, b.line)))
    });
    merged
        .into_iter()
        .map(|f| Suggestion {
            field: field_name(f.field),
            value: f.value,
            confidence: f.confidence,
            source: Source {
                page: f.page,
                line: f.line,
                text: f.text,
                label: f.label,
                template: None,
            },
        })
        .collect()
}

fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Text(x), Value::Text(y)) => {
            let norm = |s: &str| {
                s.chars()
                    .filter(|c| c.is_alphanumeric())
                    .collect::<String>()
                    .to_lowercase()
            };
            norm(x) == norm(y)
        }
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ibans() {
        assert!(valid_iban("GB82 WEST 1234 5698 7654 32"));
        assert!(valid_iban("DE89370400440532013000"));
        assert!(!valid_iban("GB82 WEST 1234 5698 7654 33"));
    }

    #[test]
    fn gb_vat_numbers() {
        // HMRC's published examples.
        assert!(valid_gb_vat("562235945"));
        assert!(valid_gb_vat("980780684"));
        assert!(!valid_gb_vat("562235946"));
    }

    #[test]
    fn dates_in_words_and_numbers() {
        let d = |s| find_date(s, DateOrder::Dmy).map(|x| x.0.to_string());
        assert_eq!(d("on 7th March 2024").as_deref(), Some("2024-03-07"));
        assert_eq!(d("March 7, 2024").as_deref(), Some("2024-03-07"));
        assert_eq!(d("the 1st day of Sept 2023").as_deref(), Some("2023-09-01"));
        assert_eq!(d("07/03/2024").as_deref(), Some("2024-03-07"));
        assert_eq!(d("2024-03-07").as_deref(), Some("2024-03-07"));
        assert_eq!(d("31/02/2024"), None);
        assert_eq!(d("Mayday 7, 2024"), None);
        assert_eq!(
            find_date("07/03/2024", DateOrder::Mdy)
                .map(|x| x.0.to_string())
                .as_deref(),
            Some("2024-07-03")
        );
    }

    #[test]
    fn amounts() {
        let a = |s| {
            find_amounts(s, "GBP")
                .into_iter()
                .map(|(m, _)| format!("{} {}", m.minor, m.currency))
                .collect::<Vec<_>>()
        };
        assert_eq!(a("£1,250.50"), ["125050 GBP"]);
        assert_eq!(a("1.250,50 €"), ["125050 EUR"]);
        assert_eq!(a("USD 300.00"), ["30000 USD"]);
        assert_eq!(a("VAT @ 20%   200.00"), ["20000 GBP"]);
        assert_eq!(a("Qty 3"), Vec::<String>::new());
        assert_eq!(a("(45.00)"), ["-4500 GBP"]);
    }

    #[test]
    fn references() {
        let r = |s| find_reference(s, DateOrder::Dmy);
        assert_eq!(r(": INV-2024-0042").as_deref(), Some("INV-2024-0042"));
        assert_eq!(r(" No. 1042   Date 01/02/2024").as_deref(), Some("1042"));
        assert_eq!(r(" 01/02/2024"), None);
        assert_eq!(r(" to be confirmed"), None);
    }

    #[test]
    fn months() {
        assert_eq!(month("Sept"), Some(9));
        assert_eq!(month("September"), Some(9));
        assert_eq!(month("Septembre"), None);
        assert_eq!(month("ma"), None);
    }
}
