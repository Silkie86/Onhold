//! What the CRM's records (talent, organisations, contacts) share: input
//! checks that report every problem at once, tags, list parameters, and
//! duplicate candidates.

use core_db::Tx;
use core_domain::Money;
use core_engines::Issue;
use core_engines::fields::{FieldDef, Mode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::{IntoParams, ToSchema};

use crate::error::{ApiError, ApiResult};

/// The CRM's record types, as custom fields, events and imports name them.
pub const ENTITIES: &[&str] = &["talent", "organisation", "contact"];

/// The most tags one record carries, and the longest tag.
pub const MAX_TAGS: usize = 50;
pub const MAX_TAG_LEN: usize = 50;

/// Names at least this similar (trigram similarity, 0 to 1) are offered as
/// possible duplicates.
pub const SIMILAR_NAME: f32 = 0.5;
/// The most duplicate candidates returned.
/// The sort that puts the best search matches first.
const RELEVANCE: &str = "relevance";

pub const MAX_CANDIDATES: i64 = 10;

/// Collects every problem with an input so one response can list them all.
#[derive(Debug, Default)]
pub(crate) struct Check {
    pub issues: Vec<Issue>,
}

impl Check {
    pub fn issue(&mut self, field: impl Into<String>, message: impl Into<String>) {
        self.issues.push(Issue::new(field, message));
    }

    /// Trimmed text of 1 to `max` characters.
    pub fn required(&mut self, field: &str, value: &str, max: usize) -> String {
        let v = value.trim();
        if v.is_empty() || v.chars().count() > max {
            self.issue(field, format!("must be 1 to {max} characters"));
        }
        v.to_owned()
    }

    /// Trimmed text of at most `max` characters; blank is `None`.
    pub fn optional(&mut self, field: &str, value: Option<&str>, max: usize) -> Option<String> {
        let v = value.map(str::trim).filter(|v| !v.is_empty())?;
        if v.chars().count() > max {
            self.issue(field, format!("must be at most {max} characters"));
        }
        Some(v.to_owned())
    }

    /// Like [`Check::optional`], keeping line breaks and inner spacing.
    pub fn long(&mut self, field: &str, value: Option<&str>, max: usize) -> Option<String> {
        let v = value.filter(|v| !v.trim().is_empty())?;
        if v.chars().count() > max {
            self.issue(field, format!("must be at most {max} characters"));
        }
        Some(v.trim_end().to_owned())
    }

    /// An ISO 4217 code, upper-cased.
    pub fn currency(&mut self, field: &str, value: Option<&str>) -> Option<String> {
        let v = value
            .map(|c| c.trim().to_ascii_uppercase())
            .filter(|c| !c.is_empty())?;
        if core_domain::workspace::validate_currency(&v).is_err() {
            self.issue(field, format!("`{v}` is not an ISO 4217 currency code"));
        }
        Some(v)
    }

    /// A non-negative amount in a known currency.
    pub fn money(&mut self, field: &str, value: Option<&Money>) -> Option<Money> {
        let m = value?;
        let currency = m.currency.trim().to_ascii_uppercase();
        if core_domain::workspace::validate_currency(&currency).is_err() {
            self.issue(
                format!("{field}.currency"),
                format!("`{currency}` is not an ISO 4217 currency code"),
            );
        }
        if m.minor < 0 {
            self.issue(format!("{field}.minor"), "must not be negative");
        }
        Some(Money {
            minor: m.minor,
            currency,
        })
    }

    /// Lower-cased, trimmed, without repeats.
    pub fn tags(&mut self, tags: &[String]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for t in tags {
            let t = t
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            if t.is_empty() || out.contains(&t) {
                continue;
            }
            if t.chars().count() > MAX_TAG_LEN {
                self.issue(
                    "tags",
                    format!("`{t}` is longer than {MAX_TAG_LEN} characters"),
                );
                continue;
            }
            out.push(t);
        }
        if out.len() > MAX_TAGS {
            self.issue("tags", format!("give at most {MAX_TAGS} tags"));
        }
        out
    }

    /// Validates custom values against the entity's field definitions.
    pub fn custom(
        &mut self,
        defs: &[FieldDef],
        current: &Map<String, Value>,
        changes: &Map<String, Value>,
        mode: Mode,
    ) -> Map<String, Value> {
        match core_engines::fields::apply(defs, current, changes, mode) {
            Ok(v) => v,
            Err(issues) => {
                self.issues.extend(issues);
                current.clone()
            }
        }
    }

    pub fn finish(self) -> ApiResult<()> {
        if self.issues.is_empty() {
            Ok(())
        } else {
            Err(ApiError::Invalid(self.issues))
        }
    }
}

/// The keys of an entity's searchable custom fields.
pub(crate) async fn searchable(tx: &mut Tx, entity: &str) -> ApiResult<Vec<String>> {
    Ok(core_db::fields::list(tx, Some(entity), false)
        .await?
        .into_iter()
        .filter(|f| f.searchable)
        .map(|f| f.key)
        .collect())
}

/// Searching, filtering, sorting and paging a record list.
#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct ListQuery {
    /// Search text: matches anywhere in the name and other short fields,
    /// searchable custom fields, and close misspellings.
    pub q: Option<String>,
    /// Only records with this tag.
    pub tag: Option<String>,
    /// Only records whose custom values include these, as a JSON object,
    /// e.g. `{"genre":"jazz"}`.
    pub custom: Option<String>,
    /// A column to sort by, `-` first for descending: `name`, `-created_at`,
    /// `custom.genre`, or `relevance` (best search matches first). Default
    /// `relevance` when searching, otherwise `name`.
    pub sort: Option<String>,
    /// Rows to skip, default 0.
    pub offset: Option<i64>,
    /// 1 to 500, default 50.
    pub limit: Option<i64>,
}

/// [`ListQuery`], checked, owning what [`core_db::crm::ListParams`] borrows.
#[derive(Debug, Clone)]
pub(crate) struct ListArgs {
    pub q: Option<String>,
    pub searchable: Vec<String>,
    pub tag: Option<String>,
    pub custom: Option<Value>,
    pub sort: String,
    pub sort_custom: Option<String>,
    pub desc: bool,
    pub offset: i64,
    pub limit: i64,
}

impl ListArgs {
    pub async fn parse(
        tx: &mut Tx,
        entity: &str,
        sorts: &[&str],
        q: &ListQuery,
    ) -> ApiResult<Self> {
        let searching = q.q.as_deref().is_some_and(|s| !s.trim().is_empty());
        // Searches list the best matches first unless asked otherwise.
        let fallback = if searching { RELEVANCE } else { "name" };
        let raw = q
            .sort
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(fallback);
        let (desc, key) = match raw.strip_prefix('-') {
            Some(k) => (true, k),
            None => (false, raw),
        };
        let (sort, sort_custom) = match key.strip_prefix("custom.") {
            Some(field) if core_engines::fields::is_valid_key(field) => {
                ("custom".to_owned(), Some(field.to_owned()))
            }
            None if sorts.contains(&key) || key == RELEVANCE => (key.to_owned(), None),
            _ => {
                return Err(ApiError::invalid(
                    "sort",
                    format!("sort by one of {} or custom.<field>", sorts.join(", ")),
                ));
            }
        };
        if let Some(field) = &sort_custom {
            let defs = core_db::fields::list(tx, Some(entity), false).await?;
            if !defs.iter().any(|d| &d.key == field) {
                return Err(ApiError::invalid(
                    "sort",
                    format!("there is no custom field `{field}`"),
                ));
            }
        }
        let custom = match q.custom.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            None => None,
            Some(text) => match serde_json::from_str::<Value>(text) {
                Ok(v @ Value::Object(_)) => Some(v),
                _ => {
                    return Err(ApiError::invalid(
                        "custom",
                        "must be a JSON object such as {\"genre\":\"jazz\"}",
                    ));
                }
            },
        };
        let search =
            q.q.as_deref()
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty());
        if search.as_ref().is_some_and(|s| s.chars().count() > 200) {
            return Err(ApiError::invalid("q", "must be at most 200 characters"));
        }
        let searchable = match &search {
            Some(_) => searchable(tx, entity).await?,
            None => Vec::new(),
        };
        Ok(Self {
            q: search,
            searchable,
            tag: q
                .tag
                .as_deref()
                .map(|t| t.trim().to_lowercase())
                .filter(|t| !t.is_empty()),
            custom,
            sort,
            sort_custom,
            desc,
            offset: q.offset.unwrap_or(0).max(0),
            limit: q.limit.unwrap_or(50).clamp(1, 500),
        })
    }

    pub fn params(&self) -> core_db::crm::ListParams<'_> {
        core_db::crm::ListParams {
            q: self.q.as_deref(),
            searchable: &self.searchable,
            tag: self.tag.as_deref(),
            custom: self.custom.as_ref(),
            sort: &self.sort,
            sort_custom: self.sort_custom.as_deref(),
            desc: self.desc,
            offset: self.offset,
            limit: self.limit,
        }
    }
}

/// A record that may be the one being entered.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Duplicate {
    pub id: uuid::Uuid,
    pub name: String,
    /// Why it matched, for people: "same email jo@example.com",
    /// "similar name (82%)".
    pub reasons: Vec<String>,
}

pub(crate) fn name_reason(similarity: f32) -> Option<String> {
    if similarity >= 0.999 {
        Some("same name".into())
    } else if similarity >= SIMILAR_NAME {
        Some(format!("similar name ({:.0}%)", similarity * 100.0))
    } else {
        None
    }
}
