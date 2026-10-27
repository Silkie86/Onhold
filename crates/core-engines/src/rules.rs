//! Automation rules: `WHEN <trigger> IF <conditions> THEN <actions>`, stored
//! as JSON. This module parses and checks rule definitions; running them
//! (loading objects, sending mail, changing records) belongs to the API's
//! automation service and the worker.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::conditions::{self, Condition};
use crate::{Issue, path};

/// How many rule runs may chain (a rule's action triggering another rule)
/// before the chain is cut. Stops two rules from feeding each other forever.
pub const MAX_DEPTH: u8 = 3;
pub const MAX_ACTIONS: usize = 10;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    /// A domain event, e.g. `thing.created` or `thing.stage_changed`.
    Event { topic: String },
    /// A time relative to a date field on each object, e.g. 7 days
    /// (`-10080` minutes) before `thing.due_at`.
    DateOffset {
        entity: String,
        /// `due_at` or `custom.<date or datetime field>`.
        field: String,
        offset_minutes: i64,
    },
    /// A cron schedule (`0 9 * * MON`) in the workspace's timezone unless one is given.
    Schedule {
        cron: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timezone: Option<String>,
    },
}

impl Trigger {
    pub fn kind(&self) -> &'static str {
        match self {
            Trigger::Event { .. } => "event",
            Trigger::DateOffset { .. } => "date_offset",
            Trigger::Schedule { .. } => "schedule",
        }
    }

    /// Whether the rule runs against one object (and so may change it).
    pub fn has_object(&self) -> bool {
        !matches!(self, Trigger::Schedule { .. })
    }
}

/// Who an email goes to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum Recipient {
    /// A fixed address.
    Address { address: String },
    /// An email field on the object, e.g. `thing.custom.client_email`.
    Field { field: String },
    /// Every member with this role.
    Role { role: core_domain::Role },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// Send an email from a template, through the queue.
    SendEmail { template: String, to: Recipient },
    /// Set a field on the object: `name` (or `title`), `due_at` or `custom.<key>`.
    SetField {
        field: String,
        #[schema(value_type = Object)]
        value: Value,
    },
    /// Move the object to a stage of its pipeline (by stage key).
    MoveStage { stage: String },
    /// Deliver the triggering event to one webhook endpoint.
    CallWebhook { webhook_id: Uuid },
    /// Make a task about the object (if it is one tasks can be about). The
    /// title may use template variables: `Chase deposit for {{ booking.title }}`.
    CreateTask {
        title: String,
        /// Due this long after the rule runs; none when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        due_in_minutes: Option<i64>,
        /// Who it is for; nobody in particular when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assignee_id: Option<Uuid>,
    },
}

impl Action {
    pub fn needs_object(&self) -> bool {
        match self {
            Action::SetField { .. } | Action::MoveStage { .. } => true,
            Action::SendEmail { to, .. } => matches!(to, Recipient::Field { .. }),
            Action::CallWebhook { .. } | Action::CreateTask { .. } => false,
        }
    }
}

/// A parsed, checked rule.
#[derive(Debug, Clone)]
pub struct RuleDef {
    pub trigger: Trigger,
    pub conditions: Condition,
    pub actions: Vec<Action>,
}

/// What the caller knows about the workspace's configuration, so references
/// in a rule can be checked when it is saved.
pub trait Catalog {
    /// Entities that support date-offset triggers, with their date fields.
    fn date_fields(&self, entity: &str) -> Option<Vec<String>>;
    fn template_exists(&self, key: &str) -> bool;
    fn is_valid_timezone(&self, tz: &str) -> bool;
}

fn is_topic(s: &str) -> bool {
    s.len() <= 100
        && s.split('.').count() >= 2
        && s.split('.')
            .all(|seg| !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
}

/// Parses and checks a rule's stored JSON parts.
pub fn parse(
    trigger: &Value,
    conditions: &Value,
    actions: &Value,
    catalog: &dyn Catalog,
) -> Result<RuleDef, Issue> {
    let trigger: Trigger = serde_json::from_value(trigger.clone()).map_err(|e| {
        Issue::new(
            "trigger",
            format!("must be an event, date_offset or schedule trigger ({e})"),
        )
    })?;
    match &trigger {
        Trigger::Event { topic } => {
            if !is_topic(topic) {
                return Err(Issue::new(
                    "trigger.topic",
                    "name an event such as `thing.created`",
                ));
            }
        }
        Trigger::DateOffset {
            entity,
            field,
            offset_minutes,
        } => {
            let fields = catalog.date_fields(entity).ok_or_else(|| {
                Issue::new(
                    "trigger.entity",
                    format!("`{entity}` has no dates to schedule from"),
                )
            })?;
            if !fields.contains(field) {
                return Err(Issue::new(
                    "trigger.field",
                    format!("`{field}` is not a date field of {entity}"),
                ));
            }
            // A year either way is plenty and keeps the scan window bounded.
            if offset_minutes.abs() > 366 * 24 * 60 {
                return Err(Issue::new(
                    "trigger.offset_minutes",
                    "must be within a year either way",
                ));
            }
        }
        Trigger::Schedule { cron, timezone } => {
            crate::schedule::parse_cron(cron).map_err(|m| Issue::new("trigger.cron", m))?;
            if let Some(tz) = timezone
                && !catalog.is_valid_timezone(tz)
            {
                return Err(Issue::new(
                    "trigger.timezone",
                    format!("`{tz}` is not an IANA timezone"),
                ));
            }
        }
    }

    let conditions = conditions::parse(conditions)?;

    let actions: Vec<Action> = serde_json::from_value(actions.clone()).map_err(|e| {
        Issue::new(
            "actions",
            format!(
                "must be a list of send_email, set_field, move_stage, call_webhook or create_task actions ({e})"
            ),
        )
    })?;
    if actions.is_empty() || actions.len() > MAX_ACTIONS {
        return Err(Issue::new(
            "actions",
            format!("give 1 to {MAX_ACTIONS} actions"),
        ));
    }
    for (i, action) in actions.iter().enumerate() {
        let at = |f: &str| format!("actions[{i}].{f}");
        if action.needs_object() && !trigger.has_object() {
            return Err(Issue::new(
                format!("actions[{i}]"),
                "a scheduled rule has no object to act on",
            ));
        }
        if matches!(action, Action::CallWebhook { .. }) && !matches!(trigger, Trigger::Event { .. })
        {
            return Err(Issue::new(
                format!("actions[{i}]"),
                "call_webhook sends the triggering event, so it needs an event trigger",
            ));
        }
        match action {
            Action::SendEmail { template, to } => {
                if !catalog.template_exists(template) {
                    return Err(Issue::new(
                        at("template"),
                        format!("there is no email template `{template}`"),
                    ));
                }
                match to {
                    Recipient::Address { address } if !crate::fields::is_email(address) => {
                        return Err(Issue::new(at("to.address"), "must be an email address"));
                    }
                    Recipient::Field { field } if !path::is_valid(field) => {
                        return Err(Issue::new(
                            at("to.field"),
                            "use a dotted path such as `thing.custom.client_email`",
                        ));
                    }
                    _ => {}
                }
            }
            Action::SetField { field, .. } => {
                let ok = field == "name"
                    || field == "title"
                    || field == "due_at"
                    || field
                        .strip_prefix("custom.")
                        .is_some_and(crate::fields::is_valid_key);
                if !ok {
                    return Err(Issue::new(
                        at("field"),
                        "use `name`, `title`, `due_at` or `custom.<field key>`",
                    ));
                }
            }
            Action::MoveStage { stage } => {
                if !crate::fields::is_valid_key(stage) {
                    return Err(Issue::new(at("stage"), "name a stage key"));
                }
            }
            Action::CallWebhook { .. } => {}
            Action::CreateTask {
                title,
                due_in_minutes,
                ..
            } => {
                let len = title.trim().chars().count();
                if len == 0 || len > 300 {
                    return Err(Issue::new(at("title"), "must be 1 to 300 characters"));
                }
                if due_in_minutes.is_some_and(|m| !(0..=366 * 24 * 60).contains(&m)) {
                    return Err(Issue::new(at("due_in_minutes"), "must be from 0 to a year"));
                }
            }
        }
    }

    Ok(RuleDef {
        trigger,
        conditions,
        actions,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    struct Fixed;

    impl Catalog for Fixed {
        fn date_fields(&self, entity: &str) -> Option<Vec<String>> {
            (entity == "thing").then(|| vec!["due_at".into(), "custom.show_date".into()])
        }
        fn template_exists(&self, key: &str) -> bool {
            key == "welcome"
        }
        fn is_valid_timezone(&self, tz: &str) -> bool {
            tz.parse::<chrono_tz::Tz>().is_ok()
        }
    }

    fn parse_rule(trigger: Value, actions: Value) -> Result<RuleDef, Issue> {
        parse(&trigger, &json!({"all": []}), &actions, &Fixed)
    }

    #[test]
    fn valid_rules_parse() {
        let email = json!([{ "type": "send_email", "template": "welcome", "to": { "field": "thing.custom.email" } }]);
        let r = parse_rule(
            json!({"kind": "event", "topic": "thing.created"}),
            email.clone(),
        )
        .expect("valid");
        assert_eq!(r.trigger.kind(), "event");
        assert!(matches!(
            &r.actions[0],
            Action::SendEmail {
                to: Recipient::Field { .. },
                ..
            }
        ));
        parse_rule(
            json!({"kind": "date_offset", "entity": "thing", "field": "due_at", "offset_minutes": -1440}),
            json!([{ "type": "move_stage", "stage": "chase" }, { "type": "set_field", "field": "custom.flag", "value": true }]),
        )
        .expect("valid");
        parse_rule(
            json!({"kind": "schedule", "cron": "0 9 * * MON", "timezone": "Europe/London"}),
            json!([{ "type": "send_email", "template": "welcome", "to": { "role": "admin" } }]),
        )
        .expect("valid");
        let r = parse_rule(
            json!({"kind": "schedule", "cron": "0 9 * * MON"}),
            json!([{ "type": "create_task", "title": "Weekly review", "due_in_minutes": 480 }]),
        )
        .expect("a task needs no object");
        assert!(matches!(
            &r.actions[0],
            Action::CreateTask {
                due_in_minutes: Some(480),
                ..
            }
        ));
    }

    #[test]
    fn invalid_rules_name_the_problem() {
        let email =
            json!([{ "type": "send_email", "template": "welcome", "to": { "address": "a@b.co" } }]);
        let cases = [
            (
                json!({"kind": "event", "topic": "Thing Created"}),
                email.clone(),
                "trigger.topic",
            ),
            (json!({"kind": "sometimes"}), email.clone(), "trigger"),
            (
                json!({"kind": "date_offset", "entity": "thing", "field": "name", "offset_minutes": 0}),
                email.clone(),
                "trigger.field",
            ),
            (
                json!({"kind": "date_offset", "entity": "booking", "field": "due_at", "offset_minutes": 0}),
                email.clone(),
                "trigger.entity",
            ),
            (
                json!({"kind": "schedule", "cron": "every day"}),
                email.clone(),
                "trigger.cron",
            ),
            (
                json!({"kind": "schedule", "cron": "0 9 * * *", "timezone": "Mars/Base"}),
                email.clone(),
                "trigger.timezone",
            ),
            (
                json!({"kind": "event", "topic": "thing.created"}),
                json!([]),
                "actions",
            ),
            (
                json!({"kind": "event", "topic": "thing.created"}),
                json!([{ "type": "launch" }]),
                "actions",
            ),
            (
                json!({"kind": "event", "topic": "thing.created"}),
                json!([{ "type": "send_email", "template": "nope", "to": { "address": "a@b.co" } }]),
                "actions[0].template",
            ),
            (
                json!({"kind": "event", "topic": "thing.created"}),
                json!([{ "type": "send_email", "template": "welcome", "to": { "address": "nobody" } }]),
                "actions[0].to.address",
            ),
            (
                json!({"kind": "event", "topic": "thing.created"}),
                json!([{ "type": "set_field", "field": "id", "value": 1 }]),
                "actions[0].field",
            ),
            (
                json!({"kind": "schedule", "cron": "0 9 * * *"}),
                json!([{ "type": "move_stage", "stage": "x" }]),
                "actions[0]",
            ),
            (
                json!({"kind": "event", "topic": "booking.confirmed"}),
                json!([{ "type": "create_task", "title": "  " }]),
                "actions[0].title",
            ),
            (
                json!({"kind": "event", "topic": "booking.confirmed"}),
                json!([{ "type": "create_task", "title": "Chase", "due_in_minutes": -5 }]),
                "actions[0].due_in_minutes",
            ),
            (
                json!({"kind": "date_offset", "entity": "thing", "field": "due_at", "offset_minutes": 0}),
                json!([{ "type": "call_webhook", "webhook_id": "0190d5f0-0000-7000-8000-000000000000" }]),
                "actions[0]",
            ),
        ];
        for (trigger, actions, field) in cases {
            let err = parse_rule(trigger.clone(), actions).expect_err("invalid");
            assert_eq!(err.field, field, "{trigger}: {}", err.message);
        }
    }
}
