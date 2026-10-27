//! What the CRM's record lists share: search, filter, sort and paging
//! parameters, and the duplicate candidates the checks return.

use serde_json::Value;
use uuid::Uuid;

/// One page of a record list.
#[derive(Debug, Clone)]
pub struct ListParams<'a> {
    /// Text to search for, lower-cased and trimmed: matched as "contains"
    /// against the record's summary and its searchable custom fields, and
    /// fuzzily (by trigram word similarity) against the summary.
    pub q: Option<&'a str>,
    /// Custom field keys whose values the search covers.
    pub searchable: &'a [String],
    /// Records carrying this tag.
    pub tag: Option<&'a str>,
    /// Records whose custom values contain this object (`{"genre": "jazz"}`).
    pub custom: Option<&'a Value>,
    /// A core column name, or `custom` to sort by `sort_custom`.
    pub sort: &'a str,
    pub sort_custom: Option<&'a str>,
    pub desc: bool,
    pub offset: i64,
    pub limit: i64,
}

impl ListParams<'_> {
    /// The `LIKE` pattern for "contains `q`".
    pub fn pattern(&self) -> Option<String> {
        self.q.map(like_contains)
    }
}

/// `%text%` with the text's own `%`, `_` and `\` escaped.
pub fn like_contains(text: &str) -> String {
    format!(
        "%{}%",
        text.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}

/// A record that may be the same as the one being created or imported.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: Uuid,
    pub name: String,
    /// Matching emails (contacts).
    pub emails: Vec<String>,
    /// Matching phone keys (contacts).
    pub phone_keys: Vec<String>,
    /// Trigram similarity of the names, 0 to 1.
    pub similarity: f32,
}
