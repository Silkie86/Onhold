//! Custom fields: per-workspace, per-entity field definitions, and the
//! validation every write of a row's `custom` JSON goes through.
//!
//! Values are plain JSON in the row's `custom` object, keyed by field key.
//! Each type has one canonical shape:
//!
//! | type | value |
//! |---|---|
//! | text, long_text, phone | string |
//! | number | number |
//! | money | `{ "minor": 125000, "currency": "GBP" }` |
//! | date | `"2027-03-07"` |
//! | datetime | RFC 3339, stored in UTC: `"2027-03-07T19:30:00Z"` |
//! | boolean | `true` / `false` |
//! | select | one option value |
//! | multi_select | array of option values, no repeats |
//! | url | `http(s)://…` |
//! | email | an address |
//! | file, relation | the id of the linked record |

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use core_domain::workspace::validate_currency;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::Issue;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Text,
    LongText,
    Number,
    Money,
    Date,
    Datetime,
    Boolean,
    Select,
    MultiSelect,
    Url,
    Email,
    Phone,
    File,
    Relation,
}

impl FieldType {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldType::Text => "text",
            FieldType::LongText => "long_text",
            FieldType::Number => "number",
            FieldType::Money => "money",
            FieldType::Date => "date",
            FieldType::Datetime => "datetime",
            FieldType::Boolean => "boolean",
            FieldType::Select => "select",
            FieldType::MultiSelect => "multi_select",
            FieldType::Url => "url",
            FieldType::Email => "email",
            FieldType::Phone => "phone",
            FieldType::File => "file",
            FieldType::Relation => "relation",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(Value::String(s.to_owned())).ok()
    }
}

/// One choice of a select or multi-select field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

/// Type-specific rules. Only those that make sense for the type may be set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Validation {
    /// text, phone: the whole value must match this regular expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    /// number: the smallest allowed value; money: in minor units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// text, long_text: length limits in characters; multi_select: number of choices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u32>,
    /// select, multi_select: the allowed values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<SelectOption>,
    /// money: the only allowed currency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// relation: the entity linked to, e.g. `contact`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
}

/// A field definition, as the engine needs it.
#[derive(Debug, Clone)]
pub struct FieldDef {
    pub key: String,
    pub label: String,
    pub field_type: FieldType,
    pub required: bool,
    pub default_value: Option<Value>,
    pub validation: Validation,
    /// Archived fields keep their stored values but take no new ones.
    pub archived: bool,
}

const TEXT_MAX: u32 = 1_000;
const LONG_TEXT_MAX: u32 = 100_000;
const MAX_OPTIONS: usize = 500;

/// The longest regex source accepted, and the compiled size limit, so a
/// tenant's pattern cannot exhaust memory.
const REGEX_MAX_SOURCE: usize = 500;
const REGEX_SIZE_LIMIT: usize = 1 << 20;

pub fn is_valid_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    (1..=63).contains(&key.len())
        && bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn compile_regex(source: &str) -> Result<regex::Regex, String> {
    if source.len() > REGEX_MAX_SOURCE {
        return Err(format!("at most {REGEX_MAX_SOURCE} characters"));
    }
    regex::RegexBuilder::new(&format!("^(?:{source})$"))
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|e| format!("not a valid regular expression: {e}"))
}

/// Checks a definition on its own: a sensible key, rules that fit the type,
/// and a default value that would itself pass validation.
pub fn check_definition(def: &FieldDef) -> Result<(), Issue> {
    if !is_valid_key(&def.key) {
        return Err(Issue::new(
            "key",
            "start with a lowercase letter; use lowercase letters, digits and `_`, at most 63",
        ));
    }
    let label = def.label.trim();
    if label.is_empty() || label.chars().count() > 200 {
        return Err(Issue::new("label", "must be 1 to 200 characters"));
    }
    let v = &def.validation;
    let t = def.field_type;
    let not_for = |name: &str| {
        Issue::new(
            format!("validation.{name}"),
            format!("does not apply to {} fields", t.as_str()),
        )
    };

    if v.regex.is_some() && !matches!(t, FieldType::Text | FieldType::Phone) {
        return Err(not_for("regex"));
    }
    if let Some(re) = &v.regex {
        compile_regex(re).map_err(|m| Issue::new("validation.regex", m))?;
    }
    if (v.min.is_some() || v.max.is_some()) && !matches!(t, FieldType::Number | FieldType::Money) {
        return Err(not_for(if v.min.is_some() { "min" } else { "max" }));
    }
    for (name, bound) in [("min", v.min), ("max", v.max)] {
        if bound.is_some_and(|b| !b.is_finite()) {
            return Err(Issue::new(format!("validation.{name}"), "must be a number"));
        }
    }
    if let (Some(min), Some(max)) = (v.min, v.max)
        && min > max
    {
        return Err(Issue::new("validation.min", "must not be above max"));
    }
    if (v.min_length.is_some() || v.max_length.is_some())
        && !matches!(
            t,
            FieldType::Text | FieldType::LongText | FieldType::MultiSelect
        )
    {
        return Err(not_for(if v.min_length.is_some() {
            "min_length"
        } else {
            "max_length"
        }));
    }
    if let (Some(min), Some(max)) = (v.min_length, v.max_length)
        && min > max
    {
        return Err(Issue::new(
            "validation.min_length",
            "must not be above max_length",
        ));
    }
    let cap = match t {
        FieldType::LongText => LONG_TEXT_MAX,
        _ => TEXT_MAX,
    };
    if v.max_length.is_some_and(|m| m > cap) {
        return Err(Issue::new(
            "validation.max_length",
            format!("at most {cap}"),
        ));
    }

    let is_select = matches!(t, FieldType::Select | FieldType::MultiSelect);
    if !v.options.is_empty() && !is_select {
        return Err(not_for("options"));
    }
    if is_select {
        if v.options.is_empty() || v.options.len() > MAX_OPTIONS {
            return Err(Issue::new(
                "validation.options",
                format!("give 1 to {MAX_OPTIONS} options"),
            ));
        }
        let mut seen = BTreeSet::new();
        for (i, o) in v.options.iter().enumerate() {
            if o.value.is_empty() || o.value.chars().count() > 100 || o.label.trim().is_empty() {
                return Err(Issue::new(
                    format!("validation.options[{i}]"),
                    "needs a value (at most 100 characters) and a label",
                ));
            }
            if !seen.insert(o.value.as_str()) {
                return Err(Issue::new(
                    format!("validation.options[{i}].value"),
                    format!("`{}` appears twice", o.value),
                ));
            }
        }
    }

    if v.currency.is_some() && t != FieldType::Money {
        return Err(not_for("currency"));
    }
    if let Some(c) = &v.currency {
        validate_currency(c).map_err(|e| Issue::new("validation.currency", e.message))?;
    }
    if v.entity.is_some() && t != FieldType::Relation {
        return Err(not_for("entity"));
    }
    if t == FieldType::Relation {
        let ok = v.entity.as_deref().is_some_and(|e| {
            (1..=40).contains(&e.len()) && e.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        });
        if !ok {
            return Err(Issue::new(
                "validation.entity",
                "name the entity this field links to, e.g. `contact`",
            ));
        }
    }

    if let Some(default) = &def.default_value {
        if default.is_null() {
            return Ok(());
        }
        if matches!(t, FieldType::File | FieldType::Relation) {
            return Err(Issue::new(
                "default_value",
                "file and relation fields cannot have a default",
            ));
        }
        normalize(def, default).map_err(|m| Issue::new("default_value", m))?;
    }
    Ok(())
}

/// Validates one value against its definition and returns it in canonical
/// form. `null` is never passed here; it means "no value".
pub fn normalize(def: &FieldDef, value: &Value) -> Result<Value, String> {
    let v = &def.validation;
    match def.field_type {
        FieldType::Text | FieldType::LongText | FieldType::Phone => {
            let s = value.as_str().ok_or("must be text")?;
            let s = if def.field_type == FieldType::LongText {
                s.to_owned()
            } else {
                s.trim().to_owned()
            };
            let len = s.chars().count() as u32;
            let cap = if def.field_type == FieldType::LongText {
                LONG_TEXT_MAX
            } else {
                TEXT_MAX
            };
            let max = v.max_length.unwrap_or(cap).min(cap);
            if len > max {
                return Err(format!("must be at most {max} characters"));
            }
            if let Some(min) = v.min_length
                && len < min
            {
                return Err(format!("must be at least {min} characters"));
            }
            if def.field_type == FieldType::Phone {
                let digits = s.chars().filter(char::is_ascii_digit).count();
                let shape_ok = s
                    .chars()
                    .all(|c| c.is_ascii_digit() || " +-().".contains(c));
                if !shape_ok || !(5..=20).contains(&digits) {
                    return Err("must be a phone number".into());
                }
            }
            if let Some(re) = &v.regex {
                let re = compile_regex(re)?;
                if !re.is_match(&s) {
                    return Err("is not in the expected format".into());
                }
            }
            Ok(Value::String(s))
        }
        FieldType::Number => {
            let n = value.as_f64().ok_or("must be a number")?;
            check_bounds(n, v)?;
            Ok(value.clone())
        }
        FieldType::Money => {
            let m: core_domain::Money = serde_json::from_value(value.clone())
                .map_err(|_| "must be {\"minor\": <integer>, \"currency\": \"<ISO 4217>\"}")?;
            validate_currency(&m.currency).map_err(|e| e.message)?;
            if let Some(c) = &v.currency
                && &m.currency != c
            {
                return Err(format!("must be in {c}"));
            }
            check_bounds(m.minor as f64, v)?;
            Ok(serde_json::json!({ "minor": m.minor, "currency": m.currency }))
        }
        FieldType::Date => {
            let s = value.as_str().ok_or("must be a date like 2027-03-07")?;
            let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map_err(|_| "must be a date like 2027-03-07")?;
            Ok(Value::String(d.format("%Y-%m-%d").to_string()))
        }
        FieldType::Datetime => {
            let s = value
                .as_str()
                .ok_or("must be a date and time like 2027-03-07T19:30:00Z")?;
            let dt = DateTime::parse_from_rfc3339(s)
                .map_err(|_| "must be a date and time like 2027-03-07T19:30:00Z")?;
            Ok(Value::String(
                dt.with_timezone(&Utc)
                    .to_rfc3339_opts(SecondsFormat::AutoSi, true),
            ))
        }
        FieldType::Boolean => value
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| "must be true or false".into()),
        FieldType::Select => {
            let s = value.as_str().ok_or("must be one of the options")?;
            if v.options.iter().any(|o| o.value == s) {
                Ok(Value::String(s.to_owned()))
            } else {
                Err(format!("`{s}` is not one of the options"))
            }
        }
        FieldType::MultiSelect => {
            let items = value.as_array().ok_or("must be a list of options")?;
            let mut seen = BTreeSet::new();
            for item in items {
                let s = item.as_str().ok_or("must be a list of options")?;
                if !v.options.iter().any(|o| o.value == s) {
                    return Err(format!("`{s}` is not one of the options"));
                }
                if !seen.insert(s) {
                    return Err(format!("`{s}` is chosen twice"));
                }
            }
            let n = items.len() as u32;
            if v.min_length.is_some_and(|m| n < m) || v.max_length.is_some_and(|m| n > m) {
                return Err(format!(
                    "choose between {} and {} options",
                    v.min_length.unwrap_or(0),
                    v.max_length.unwrap_or(v.options.len() as u32)
                ));
            }
            Ok(value.clone())
        }
        FieldType::Url => {
            let s = value.as_str().ok_or("must be a web address")?.trim();
            let url = url::Url::parse(s).map_err(|_| "must be a web address")?;
            if !matches!(url.scheme(), "http" | "https") || url.host().is_none() || s.len() > 2000 {
                return Err("must be an http or https address".into());
            }
            Ok(Value::String(s.to_owned()))
        }
        FieldType::Email => {
            let s = value.as_str().ok_or("must be an email address")?.trim();
            if is_email(s) {
                Ok(Value::String(s.to_owned()))
            } else {
                Err("must be an email address".into())
            }
        }
        FieldType::File | FieldType::Relation => {
            let s = value.as_str().ok_or("must be a record id")?;
            uuid::Uuid::try_parse(s)
                .map(|id| Value::String(id.to_string()))
                .map_err(|_| "must be a record id".into())
        }
    }
}

fn check_bounds(n: f64, v: &Validation) -> Result<(), String> {
    if let Some(min) = v.min
        && n < min
    {
        return Err(format!("must be at least {min}"));
    }
    if let Some(max) = v.max
        && n > max
    {
        return Err(format!("must be at most {max}"));
    }
    Ok(())
}

/// A deliberately loose check: one `@`, something before it, a dotted
/// domain after it, no spaces. Deliverability is the mail server's call.
pub fn is_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && s.len() <= 320
        && !domain.contains('@')
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A new row: defaults fill missing fields and every required field must have a value.
    Create,
    /// A change: only the fields sent are checked, and a required field cannot be cleared.
    /// (Rows that predate a newly required field stay editable.)
    Update,
}

/// Applies `patch` to a row's `current` custom values. Keys in the patch
/// must be active fields; `null` clears a value. Values of archived fields
/// are kept untouched. Returns the new `custom` object or every problem found.
pub fn apply(
    defs: &[FieldDef],
    current: &Map<String, Value>,
    patch: &Map<String, Value>,
    mode: Mode,
) -> Result<Map<String, Value>, Vec<Issue>> {
    let by_key: HashMap<&str, &FieldDef> = defs.iter().map(|d| (d.key.as_str(), d)).collect();
    let mut out = current.clone();
    let mut issues = Vec::new();
    let at = |key: &str| format!("custom.{key}");

    for (key, value) in patch {
        let Some(def) = by_key.get(key.as_str()) else {
            issues.push(Issue::new(at(key), "there is no such field"));
            continue;
        };
        if def.archived {
            issues.push(Issue::new(at(key), "this field has been removed"));
            continue;
        }
        if value.is_null() {
            if def.required {
                issues.push(Issue::new(at(key), "is required"));
            } else {
                out.remove(key);
            }
            continue;
        }
        match normalize(def, value) {
            Ok(v) => {
                out.insert(key.clone(), v);
            }
            Err(m) => issues.push(Issue::new(at(key), m)),
        }
    }

    if mode == Mode::Create {
        for def in defs.iter().filter(|d| !d.archived) {
            if out.contains_key(&def.key) || patch.contains_key(&def.key) {
                continue;
            }
            match def.default_value.as_ref().filter(|v| !v.is_null()) {
                Some(default) => match normalize(def, default) {
                    Ok(v) => {
                        out.insert(def.key.clone(), v);
                    }
                    Err(m) => issues.push(Issue::new(at(&def.key), m)),
                },
                None if def.required => issues.push(Issue::new(at(&def.key), "is required")),
                None => {}
            }
        }
    }

    if issues.is_empty() {
        Ok(out)
    } else {
        issues.sort_by(|a, b| a.field.cmp(&b.field));
        Err(issues)
    }
}

/// Whether a stored value counts as filled in (for required-to-enter-stage checks).
pub fn has_value(custom: &Map<String, Value>, key: &str) -> bool {
    match custom.get(key) {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn def(key: &str, t: FieldType) -> FieldDef {
        FieldDef {
            key: key.into(),
            label: key.into(),
            field_type: t,
            required: false,
            default_value: None,
            validation: Validation::default(),
            archived: false,
        }
    }

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().cloned().expect("object")
    }

    #[test]
    fn every_type_accepts_its_canonical_shape() {
        let mut select = def("s", FieldType::Select);
        select.validation.options = vec![
            SelectOption {
                value: "a".into(),
                label: "A".into(),
            },
            SelectOption {
                value: "b".into(),
                label: "B".into(),
            },
        ];
        let mut multi = select.clone();
        multi.field_type = FieldType::MultiSelect;
        let mut relation = def("r", FieldType::Relation);
        relation.validation.entity = Some("contact".into());
        let id = uuid::Uuid::now_v7().to_string();
        let cases = [
            (def("t", FieldType::Text), json!("  hi "), json!("hi")),
            (
                def("l", FieldType::LongText),
                json!(" keep\n"),
                json!(" keep\n"),
            ),
            (def("n", FieldType::Number), json!(2.5), json!(2.5)),
            (
                def("m", FieldType::Money),
                json!({"minor": 125000, "currency": "GBP"}),
                json!({"minor": 125000, "currency": "GBP"}),
            ),
            (
                def("d", FieldType::Date),
                json!("2027-03-07"),
                json!("2027-03-07"),
            ),
            (
                def("dt", FieldType::Datetime),
                json!("2027-03-07T20:30:00+01:00"),
                json!("2027-03-07T19:30:00Z"),
            ),
            (def("b", FieldType::Boolean), json!(true), json!(true)),
            (select, json!("b"), json!("b")),
            (multi, json!(["a", "b"]), json!(["a", "b"])),
            (
                def("u", FieldType::Url),
                json!("https://example.com/x"),
                json!("https://example.com/x"),
            ),
            (def("e", FieldType::Email), json!("a@b.co"), json!("a@b.co")),
            (
                def("p", FieldType::Phone),
                json!("+44 20 7946 0000"),
                json!("+44 20 7946 0000"),
            ),
            (relation, json!(id.clone()), json!(id)),
        ];
        for (d, input, expected) in cases {
            check_definition(&d).unwrap_or_else(|e| panic!("{}: {e}", d.key));
            assert_eq!(normalize(&d, &input), Ok(expected), "{}", d.key);
        }
    }

    #[test]
    fn wrong_shapes_are_rejected() {
        let cases = [
            (FieldType::Text, json!(5)),
            (FieldType::Number, json!("5")),
            (FieldType::Money, json!(12.5)),
            (FieldType::Money, json!({"minor": 1.5, "currency": "GBP"})),
            (FieldType::Money, json!({"minor": 1, "currency": "ZZZ"})),
            (FieldType::Date, json!("07/03/2027")),
            (FieldType::Datetime, json!("2027-03-07 19:30")),
            (FieldType::Boolean, json!("yes")),
            (FieldType::Url, json!("javascript:alert(1)")),
            (FieldType::Email, json!("not an email")),
            (FieldType::Phone, json!("call me")),
            (FieldType::File, json!("abc")),
        ];
        for (t, input) in cases {
            assert!(normalize(&def("x", t), &input).is_err(), "{t:?} {input}");
        }
    }

    #[test]
    fn validation_rules_apply() {
        let mut d = def("code", FieldType::Text);
        d.validation.regex = Some("[A-Z]{3}".into());
        assert!(normalize(&d, &json!("ABC")).is_ok());
        assert!(normalize(&d, &json!("ABCD")).is_err(), "anchored");
        let mut n = def("n", FieldType::Number);
        n.validation.min = Some(0.0);
        n.validation.max = Some(10.0);
        assert!(normalize(&n, &json!(11)).is_err());
        assert!(normalize(&n, &json!(-1)).is_err());
        let mut m = def("fee", FieldType::Money);
        m.validation.currency = Some("EUR".into());
        assert!(normalize(&m, &json!({"minor": 1, "currency": "GBP"})).is_err());
    }

    #[test]
    fn definitions_must_make_sense() {
        let mut d = def("Bad Key", FieldType::Text);
        assert_eq!(check_definition(&d).map_err(|i| i.field), Err("key".into()));
        d.key = "ok".into();
        d.validation.options = vec![SelectOption {
            value: "a".into(),
            label: "A".into(),
        }];
        assert!(check_definition(&d).is_err(), "options on text");
        let s = def("s", FieldType::Select);
        assert!(check_definition(&s).is_err(), "select without options");
        let mut r = def("r", FieldType::Text);
        r.validation.regex = Some("(".into());
        assert!(check_definition(&r).is_err(), "bad regex");
        let mut n = def("n", FieldType::Number);
        n.default_value = Some(json!("x"));
        assert_eq!(
            check_definition(&n).map_err(|i| i.field),
            Err("default_value".into())
        );
        let mut rel = def("r", FieldType::Relation);
        assert!(check_definition(&rel).is_err(), "relation needs an entity");
        rel.validation.entity = Some("contact".into());
        assert!(check_definition(&rel).is_ok());
    }

    #[test]
    fn apply_merges_defaults_and_requires() {
        let mut budget = def("budget", FieldType::Number);
        budget.required = true;
        let mut region = def("region", FieldType::Text);
        region.default_value = Some(json!("UK"));
        let mut old = def("old", FieldType::Text);
        old.archived = true;
        let defs = [budget, region, old];

        let err = apply(&defs, &Map::new(), &Map::new(), Mode::Create).expect_err("budget missing");
        assert_eq!(err, vec![Issue::new("custom.budget", "is required")]);

        let created =
            apply(&defs, &Map::new(), &obj(json!({"budget": 5})), Mode::Create).expect("valid");
        assert_eq!(
            Value::Object(created.clone()),
            json!({"budget": 5, "region": "UK"})
        );

        let mut stored = created;
        stored.insert("old".into(), json!("kept"));
        let updated = apply(&defs, &stored, &obj(json!({"region": null})), Mode::Update)
            .expect("clearing optional");
        assert_eq!(Value::Object(updated), json!({"budget": 5, "old": "kept"}));

        let err = apply(
            &defs,
            &stored,
            &obj(json!({"budget": null, "old": "x", "nope": 1})),
            Mode::Update,
        )
        .expect_err("three problems");
        let fields: Vec<&str> = err.iter().map(|i| i.field.as_str()).collect();
        assert_eq!(fields, ["custom.budget", "custom.nope", "custom.old"]);
    }

    #[test]
    fn has_value_ignores_blanks() {
        let custom = obj(json!({"a": "", "b": [], "c": 0, "d": false, "e": " x "}));
        assert!(!has_value(&custom, "a"));
        assert!(!has_value(&custom, "b"));
        assert!(has_value(&custom, "c"));
        assert!(has_value(&custom, "d"));
        assert!(has_value(&custom, "e"));
        assert!(!has_value(&custom, "missing"));
    }
}
