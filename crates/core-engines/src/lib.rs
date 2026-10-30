//! Generic engines: custom fields, pipelines, automation rules and their
//! conditions, templates, schedules and webhook signing. None of them know
//! about bookings or talent, and none do I/O: the API and the worker load
//! definitions from the database and hand them here.

pub mod conditions;
pub mod countries;
pub mod documents;
pub mod fields;
pub mod import;
pub mod import_sources;
pub mod path;
pub mod pipeline;
pub mod rules;
pub mod schedule;
pub mod templates;
pub mod vcard;
pub mod webhooks;
pub mod workbook;

/// A problem with one input field, for validation errors that name the
/// field at fault (`custom.budget`, `stages[2].key`, `actions[0].template`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct Issue {
    pub field: String,
    pub message: String,
}

impl Issue {
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}
