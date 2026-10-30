//! Ready-made follow-ups: common chasers a workspace switches on in one
//! click. Each is a few ordinary rules (and the templates they send),
//! marked with the follow-up they came from, so they can be edited like any
//! other rule and switched off again. Switching off deletes the rules and
//! keeps the templates, which may have been reworded.

use core_domain::{Ctx, Permission};
use core_engines::templates::Kind;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::begin;
use super::rules::{self, Input};
use super::templates::{self, New};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

const DAY: i64 = 24 * 60;

struct Template {
    kind: Kind,
    key: &'static str,
    name: &'static str,
    subject: Option<&'static str>,
    body: &'static str,
}

struct Step {
    /// Appended to the follow-up's key to mark the rule: `invoice_overdue/7`.
    part: &'static str,
    name: &'static str,
    trigger: fn() -> Value,
    conditions: fn() -> Value,
    actions: fn() -> Value,
}

struct Preset {
    key: &'static str,
    name: &'static str,
    description: &'static str,
    /// The workspace module it belongs to; hidden when that is off.
    module: &'static str,
    templates: &'static [Template],
    steps: &'static [Step],
}

const PRESETS: &[Preset] = &[
    Preset {
        key: "enquiry_unanswered",
        name: "Unanswered enquiries",
        description: "Makes a task to reply when an enquiry is still new two days after it came in.",
        module: "bookings",
        templates: &[],
        steps: &[Step {
            part: "2",
            name: "Follow-up: enquiry still new after 2 days",
            trigger: || json!({ "kind": "date_offset", "entity": "enquiry", "field": "created_at", "offset_minutes": 2 * DAY }),
            conditions: || json!({ "all": [{ "field": "enquiry.stage", "op": "eq", "value": "new" }] }),
            actions: || json!([{ "type": "create_task", "title": "Reply to the enquiry: {{ enquiry.title }}", "due_in_minutes": 0 }]),
        }],
    },
    Preset {
        key: "quote_chase",
        name: "Quote chaser",
        description: "Emails the client when a quote has not been opened three days after it was sent.",
        module: "bookings",
        templates: &[Template {
            kind: Kind::Email,
            key: "follow_up.quote",
            name: "Quote chaser",
            subject: Some("Your quote {{ quote.reference }}"),
            body: "<p>Hello,</p>\
                   <p>Just checking our quote {{ quote.reference }} for {{ quote.booking_title }} \
                   ({{ quote.total | money }}) reached you. Happy to answer any questions.</p>\
                   <p>{{ workspace.name }}</p>",
        }],
        steps: &[Step {
            part: "3",
            name: "Follow-up: quote not opened after 3 days",
            trigger: || json!({ "kind": "date_offset", "entity": "quote", "field": "sent_at", "offset_minutes": 3 * DAY }),
            conditions: || json!({ "all": [{ "field": "quote.status", "op": "eq", "value": "sent" }] }),
            actions: || json!([{ "type": "send_email", "template": "follow_up.quote", "to": { "field": "quote.sent_to" } }]),
        }],
    },
    Preset {
        key: "invoice_overdue",
        name: "Overdue invoice reminders",
        description: "Emails whoever the invoice was sent to 1, 7 and 14 days after it was due and still unpaid, and makes a task to call at 14 days.",
        module: "finance",
        templates: &[
            Template {
                kind: Kind::Email,
                key: "follow_up.invoice.first",
                name: "Overdue invoice: first reminder",
                subject: Some("Invoice {{ invoice.reference }} is now due"),
                body: "<p>Hello,</p>\
                       <p>A friendly reminder that invoice {{ invoice.reference }} was due on \
                       {{ invoice.due_date | date }}. {{ invoice.outstanding | money }} is still to pay.</p>\
                       <p>If you have already paid, thank you, and please ignore this.</p>\
                       <p>{{ workspace.name }}</p>",
            },
            Template {
                kind: Kind::Email,
                key: "follow_up.invoice.second",
                name: "Overdue invoice: second reminder",
                subject: Some("Invoice {{ invoice.reference }} is a week overdue"),
                body: "<p>Hello,</p>\
                       <p>Invoice {{ invoice.reference }} was due on {{ invoice.due_date | date }} and \
                       {{ invoice.outstanding | money }} is still to pay. Could you let us know when \
                       to expect payment?</p>\
                       <p>{{ workspace.name }}</p>",
            },
            Template {
                kind: Kind::Email,
                key: "follow_up.invoice.final",
                name: "Overdue invoice: final reminder",
                subject: Some("Invoice {{ invoice.reference }} is two weeks overdue"),
                body: "<p>Hello,</p>\
                       <p>Invoice {{ invoice.reference }} is now two weeks overdue, with \
                       {{ invoice.outstanding | money }} still to pay. Please pay it, or reply to \
                       tell us when you will.</p>\
                       <p>{{ workspace.name }}</p>",
            },
        ],
        steps: &[
            Step {
                part: "1",
                name: "Follow-up: invoice 1 day overdue",
                trigger: || json!({ "kind": "date_offset", "entity": "invoice", "field": "due_date", "offset_minutes": DAY }),
                conditions: || json!({ "all": [] }),
                actions: || json!([{ "type": "send_email", "template": "follow_up.invoice.first", "to": { "field": "invoice.sent_to" } }]),
            },
            Step {
                part: "7",
                name: "Follow-up: invoice 7 days overdue",
                trigger: || json!({ "kind": "date_offset", "entity": "invoice", "field": "due_date", "offset_minutes": 7 * DAY }),
                conditions: || json!({ "all": [] }),
                actions: || json!([{ "type": "send_email", "template": "follow_up.invoice.second", "to": { "field": "invoice.sent_to" } }]),
            },
            Step {
                part: "14",
                name: "Follow-up: invoice 14 days overdue",
                trigger: || json!({ "kind": "date_offset", "entity": "invoice", "field": "due_date", "offset_minutes": 14 * DAY }),
                conditions: || json!({ "all": [] }),
                // The task first: an invoice never emailed still gets the call.
                actions: || {
                    json!([
                        { "type": "create_task", "title": "Call about overdue invoice {{ invoice.reference }}", "due_in_minutes": 0 },
                        { "type": "send_email", "template": "follow_up.invoice.final", "to": { "field": "invoice.sent_to" } }
                    ])
                },
            },
        ],
    },
    Preset {
        key: "booking_reminder_text",
        name: "Booking reminder text",
        description: "Texts the booking's contact the day before it starts, if they agreed to texts. Needs a texting number.",
        module: "messaging",
        templates: &[Template {
            kind: Kind::Sms,
            key: "follow_up.booking.reminder",
            name: "Booking reminder text",
            subject: None,
            body: "Reminder from {{ workspace.name }}: {{ booking.title }} is tomorrow, {{ booking.starts_at | date(none, booking.timezone) }}.",
        }],
        steps: &[Step {
            part: "1",
            name: "Follow-up: text the day before a booking",
            trigger: || json!({ "kind": "date_offset", "entity": "booking", "field": "starts_at", "offset_minutes": -DAY }),
            conditions: || json!({ "all": [{ "field": "booking.status", "op": "eq", "value": "confirmed" }] }),
            actions: || json!([{ "type": "send_sms", "template": "follow_up.booking.reminder" }]),
        }],
    },
];

/// A ready-made follow-up and whether it is on.
#[derive(Debug, Serialize, ToSchema)]
pub struct FollowUp {
    pub key: String,
    pub name: String,
    pub description: String,
    /// The workspace module it belongs to.
    pub module: String,
    /// Whether its rules are installed.
    pub on: bool,
    /// The rules installed for it, to edit them like any other rule.
    pub rule_ids: Vec<Uuid>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetFollowUp {
    pub on: bool,
}

fn preset(key: &str) -> ApiResult<&'static Preset> {
    PRESETS
        .iter()
        .find(|p| p.key == key)
        .ok_or(ApiError::NotFound)
}

fn marker(p: &Preset, s: &Step) -> String {
    format!("{}/{}", p.key, s.part)
}

pub async fn list(state: &AppState, ctx: &Ctx) -> ApiResult<Vec<FollowUp>> {
    ctx.authorize(Permission::WorkspaceRead)?;
    let mut tx = begin(state, ctx).await?;
    let installed = core_db::rules::presets(&mut tx).await?;
    tx.commit().await?;
    Ok(PRESETS
        .iter()
        .map(|p| {
            let rule_ids: Vec<Uuid> = installed
                .iter()
                .filter(|(m, _, _)| m.split('/').next() == Some(p.key))
                .map(|(_, id, _)| *id)
                .collect();
            FollowUp {
                key: p.key.into(),
                name: p.name.into(),
                description: p.description.into(),
                module: p.module.into(),
                on: !rule_ids.is_empty(),
                rule_ids,
            }
        })
        .collect())
}

/// Switches a follow-up on (adding whatever of it is missing) or off.
pub async fn set(state: &AppState, ctx: &Ctx, key: &str, on: bool) -> ApiResult<FollowUp> {
    ctx.authorize(Permission::ConfigWrite)?;
    let p = preset(key)?;
    let mut tx = begin(state, ctx).await?;
    // One switch at a time per workspace, so two cannot both add the rules.
    core_db::rules::lock_presets(&mut tx).await?;
    let installed = core_db::rules::presets(&mut tx).await?;
    let mine = |s: &Step| {
        let m = marker(p, s);
        installed
            .iter()
            .find(|(i, _, _)| *i == m)
            .map(|(_, id, _)| *id)
    };
    if on {
        for t in p.templates {
            // One the workspace already has (perhaps reworded) is kept.
            if core_db::templates::key_exists(&mut tx, t.key).await? {
                continue;
            }
            templates::create_in(
                state,
                &mut tx,
                ctx,
                New {
                    kind: t.kind,
                    key: t.key.into(),
                    locale: None,
                    name: t.name.into(),
                    subject: t.subject.map(Into::into),
                    body: t.body.into(),
                    description: Some(format!("Sent by the \"{}\" follow-up.", p.name)),
                },
            )
            .await?;
        }
        for s in p.steps {
            if mine(s).is_some() {
                continue;
            }
            let rule = rules::create_in(
                &mut tx,
                ctx,
                &Input {
                    name: s.name.into(),
                    enabled: true,
                    dry_run: false,
                    once_per_object: false,
                    trigger: (s.trigger)(),
                    conditions: (s.conditions)(),
                    actions: (s.actions)(),
                },
            )
            .await?;
            core_db::rules::set_preset(&mut tx, rule.id, &marker(p, s)).await?;
        }
    } else {
        for (m, id, _) in &installed {
            if m.split('/').next() == Some(p.key) {
                rules::delete_in(&mut tx, ctx, *id).await?;
            }
        }
    }
    tx.commit().await?;
    list(state, ctx)
        .await?
        .into_iter()
        .find(|f| f.key == key)
        .ok_or(ApiError::NotFound)
}
