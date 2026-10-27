//! Issuer templates, in the manner of invoice2data: when a workspace gets
//! the same supplier's invoices again and again, it can describe that
//! layout once. A template applies when all its keywords appear in the
//! document, and its patterns then read fields exactly, ahead of the
//! general suggestions.
//!
//! ```json
//! {
//!   "name": "Acme Studios",
//!   "keywords": ["Acme Studios Ltd", "GB562235945"],
//!   "kind": "invoice",
//!   "currency": "GBP",
//!   "fields": {
//!     "invoice_number": "Invoice\\s+(AS-\\d+)",
//!     "total": "Amount payable\\s+(\\S+)",
//!     "custom.booking_ref": { "pattern": "Job ref: (\\w+)" }
//!   }
//! }
//! ```
//!
//! Templates are tenant data, so patterns are size-limited like custom
//! field patterns.

use std::collections::BTreeMap;

use core_engines::Issue;
use core_engines::import::DateOrder;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::suggest::{
    Confidence, DocumentKind, Field, KindGuess, Options, Source, Suggestion, Suggestions, Value,
    ValueKind,
};

const MAX_NAME: usize = 100;
const MAX_KEYWORDS: usize = 20;
const MAX_KEYWORD: usize = 200;
const MAX_FIELDS: usize = 50;
const MAX_PATTERN: usize = 500;
const PATTERN_SIZE_LIMIT: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub name: String,
    /// All must appear in the document (ignoring case) for the template to
    /// apply.
    pub keywords: Vec<String>,
    /// None may appear.
    #[serde(default)]
    pub exclude_keywords: Vec<String>,
    #[serde(default)]
    pub kind: Option<DocumentKind>,
    /// For amounts without a currency mark.
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub date_order: Option<DateOrder>,
    /// A [`Field`] name or `custom.<key>`, to a regular expression. The
    /// first capture group is the value; with none, the whole match.
    #[serde(default)]
    pub fields: BTreeMap<String, TemplateField>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum TemplateField {
    Pattern(String),
    Detailed {
        pattern: String,
        /// How to read the value. Defaults to the field's own kind, or text
        /// for custom fields.
        #[serde(default, rename = "type")]
        kind: Option<ValueKind>,
    },
}

impl TemplateField {
    fn parts(&self) -> (&str, Option<ValueKind>) {
        match self {
            TemplateField::Pattern(p) => (p, None),
            TemplateField::Detailed { pattern, kind } => (pattern, *kind),
        }
    }
}

/// A checked template, ready to apply.
#[derive(Debug, Clone)]
pub struct Compiled {
    name: String,
    keywords: Vec<String>,
    exclude: Vec<String>,
    kind: Option<DocumentKind>,
    currency: Option<String>,
    date_order: Option<DateOrder>,
    fields: Vec<(String, ValueKind, Regex)>,
}

fn field_kind(key: &str) -> Option<ValueKind> {
    if let Some(custom) = key.strip_prefix("custom.") {
        return core_engines::fields::is_valid_key(custom).then_some(ValueKind::Text);
    }
    serde_json::from_value::<Field>(serde_json::Value::String(key.to_owned()))
        .ok()
        .map(Field::kind)
}

/// Checks one template, naming each problem by its path.
pub fn compile(t: &Template) -> Result<Compiled, Vec<Issue>> {
    let mut issues = Vec::new();
    if t.name.trim().is_empty() || t.name.len() > MAX_NAME {
        issues.push(Issue::new("name", format!("1 to {MAX_NAME} characters")));
    }
    if t.keywords.is_empty() || t.keywords.len() > MAX_KEYWORDS {
        issues.push(Issue::new(
            "keywords",
            format!("1 to {MAX_KEYWORDS} keywords"),
        ));
    }
    for (list, name) in [
        (&t.keywords, "keywords"),
        (&t.exclude_keywords, "exclude_keywords"),
    ] {
        for (i, k) in list.iter().enumerate() {
            if k.trim().is_empty() || k.len() > MAX_KEYWORD {
                issues.push(Issue::new(
                    format!("{name}[{i}]"),
                    format!("1 to {MAX_KEYWORD} characters"),
                ));
            }
        }
    }
    if let Some(c) = &t.currency
        && core_domain::workspace::validate_currency(c).is_err()
    {
        issues.push(Issue::new(
            "currency",
            format!("`{c}` is not an ISO 4217 currency code"),
        ));
    }
    if t.fields.len() > MAX_FIELDS {
        issues.push(Issue::new("fields", format!("at most {MAX_FIELDS} fields")));
    }
    let mut fields = Vec::new();
    for (key, f) in &t.fields {
        let (pattern, kind) = f.parts();
        let Some(default_kind) = field_kind(key) else {
            issues.push(Issue::new(
                format!("fields.{key}"),
                "not a known field; custom fields are `custom.<key>`",
            ));
            continue;
        };
        if pattern.len() > MAX_PATTERN {
            issues.push(Issue::new(
                format!("fields.{key}"),
                format!("at most {MAX_PATTERN} characters"),
            ));
            continue;
        }
        match RegexBuilder::new(pattern)
            .size_limit(PATTERN_SIZE_LIMIT)
            .build()
        {
            Ok(re) => fields.push((key.clone(), kind.unwrap_or(default_kind), re)),
            Err(e) => issues.push(Issue::new(
                format!("fields.{key}"),
                format!("not a valid regular expression: {e}"),
            )),
        }
    }
    if !issues.is_empty() {
        return Err(issues);
    }
    Ok(Compiled {
        name: t.name.trim().to_owned(),
        keywords: t.keywords.iter().map(|k| k.to_lowercase()).collect(),
        exclude: t
            .exclude_keywords
            .iter()
            .map(|k| k.to_lowercase())
            .collect(),
        kind: t.kind,
        currency: t.currency.clone(),
        date_order: t.date_order,
        fields,
    })
}

/// Checks a list of templates; issue paths start with the index (`[2].keywords`).
pub fn compile_all(ts: &[Template]) -> Result<Vec<Compiled>, Vec<Issue>> {
    let mut out = Vec::new();
    let mut issues = Vec::new();
    for (i, t) in ts.iter().enumerate() {
        match compile(t) {
            Ok(c) => out.push(c),
            Err(es) => issues.extend(
                es.into_iter()
                    .map(|e| Issue::new(format!("[{i}].{}", e.field), e.message)),
            ),
        }
    }
    if issues.is_empty() {
        Ok(out)
    } else {
        Err(issues)
    }
}

impl Compiled {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the template applies to a document's text.
    pub fn matches(&self, text: &str) -> bool {
        let lower = text.to_lowercase();
        self.keywords.iter().all(|k| lower.contains(k.as_str()))
            && !self.exclude.iter().any(|k| lower.contains(k.as_str()))
    }
}

/// Applies the templates that match, adding their findings to `found`
/// ahead of the general suggestions for the same field. Returns the names
/// of the templates that applied.
pub fn apply(
    templates: &[Compiled],
    pages: &[&str],
    opts: &Options,
    found: &mut Suggestions,
) -> Vec<String> {
    let text = pages.join("\n");
    let mut applied = Vec::new();
    let mut extra: Vec<Suggestion> = Vec::new();
    for t in templates.iter().filter(|t| t.matches(&text)) {
        applied.push(t.name.clone());
        let order = t.date_order.unwrap_or(opts.date_order);
        let currency = t.currency.as_deref().unwrap_or(&opts.default_currency);
        for (key, kind, re) in &t.fields {
            for caps in re.captures_iter(&text).take(5) {
                let Some(m) = caps.get(1).or_else(|| caps.get(0)) else {
                    continue;
                };
                let raw = m.as_str().trim();
                let value = match kind {
                    ValueKind::Text | ValueKind::Reference => {
                        (!raw.is_empty()).then(|| Value::Text(raw.to_owned()))
                    }
                    ValueKind::Date => crate::suggest::read_date(raw, order).map(Value::Date),
                    ValueKind::Money => crate::suggest::read_money(raw, currency).map(Value::Money),
                };
                let Some(value) = value else { continue };
                let (page, line, line_text) = position(pages, &text, m.start());
                extra.push(Suggestion {
                    field: key.clone(),
                    value,
                    confidence: Confidence::High,
                    source: Source {
                        page,
                        line,
                        text: line_text,
                        label: None,
                        template: Some(t.name.clone()),
                    },
                });
            }
        }
        if let Some(kind) = t.kind {
            found.kinds.retain(|k| k.kind != kind);
            found.kinds.insert(0, KindGuess { kind, score: 1.0 });
        }
    }
    // Template findings go first within their field; fields keep their
    // order, custom fields come last.
    for s in extra.into_iter().rev() {
        found
            .fields
            .retain(|f| !(f.field == s.field && f.value == s.value && f.source.template.is_none()));
        let at = found
            .fields
            .iter()
            .position(|f| f.field == s.field)
            .unwrap_or(found.fields.len());
        found.fields.insert(at, s);
    }
    applied
}

fn position(pages: &[&str], joined: &str, at: usize) -> (u32, u32, String) {
    let line_start = joined[..at].rfind('\n').map_or(0, |i| i + 1);
    let line_end = joined[at..].find('\n').map_or(joined.len(), |i| at + i);
    let (page, line) = crate::suggest::locate(pages, joined, at);
    (page, line, joined[line_start..line_end].trim().to_owned())
}
