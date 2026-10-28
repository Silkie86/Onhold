//! Running rules. The worker calls in here for three kinds of job:
//!
//! - `event.dispatch`: one published event. Runs every enabled rule
//!   triggered by its topic and queues a delivery to every webhook that
//!   subscribes to it.
//! - `rule.scan`: a date-offset rule looking for records whose date (plus
//!   offset) has arrived since its last scan.
//! - `rule.scheduled`: a cron rule's run.
//!
//! A rule runs inside one transaction with the workspace in context, acting
//! as `Actor::Rule`, so its changes are audited as the rule's and emit
//! events like anyone else's. Each run is logged with what it did or why it
//! did not act. Guards: the workspace kill switch, once per object, a depth
//! limit on rules triggering rules, and dry-run mode.

use chrono::{DateTime, Utc};
use core_db::Tx;
use core_db::jobs::NewJob;
use core_db::outbox::Event;
use core_db::rules::Rule;
use core_domain::authz::RequestMeta;
use core_domain::{Actor, Ctx, Permission, WorkspaceId};
use core_engines::conditions::{self, Check};
use core_engines::rules::{Action, MAX_DEPTH, Recipient, RuleDef, Trigger};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{context, things};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

/// The most addresses one send-email action may mail.
pub const MAX_RECIPIENTS: usize = 50;
/// Records a single date-offset scan handles; the next scan continues.
const SCAN_BATCH: i64 = 500;

pub const JOB_DISPATCH: &str = "event.dispatch";
pub const JOB_SCAN: &str = "rule.scan";
pub const JOB_SCHEDULED: &str = "rule.scheduled";
pub const JOB_MAIL: &str = "mail.send";
pub const JOB_WEBHOOK: &str = "webhook.deliver";
/// Platform mail made from an event (invitations, sign-in links); queued by the publisher.
pub const JOB_SYSTEM_MAIL: &str = "mail.system";

fn rule_ctx(workspace: WorkspaceId, rule_id: Uuid, depth: u8) -> Ctx {
    Ctx {
        workspace_id: workspace,
        actor: Actor::Rule { rule_id, depth },
        role: None,
        permissions: Permission::ALL.iter().copied().collect(),
        meta: RequestMeta::default(),
    }
}

/// Dedupe keys are hashed so they stay short whatever goes in.
fn dedupe_key(parts: &[&str]) -> String {
    let digest = Sha256::digest(parts.join("\u{1f}").as_bytes());
    data_encoding::HEXLOWER.encode(&digest)
}

/// What started a rule run.
struct Input {
    event: Option<Event>,
    /// What the object is (`contact`, `thing`…): the key it has in the context.
    entity: String,
    /// The record the rule is about, as JSON (with its link).
    object: Option<Value>,
    object_id: Option<Uuid>,
    before: Option<Value>,
    /// How many rules led to the event (0 when a person or key caused it).
    depth: u8,
    /// Unique per trigger occurrence, for job dedupe keys.
    occurrence: String,
}

/// One action as planned or done, for the run log.
#[derive(Debug, Clone, Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct ActionResult {
    #[serde(rename = "type")]
    pub kind: String,
    #[schema(value_type = Object)]
    #[serde(default)]
    pub detail: Value,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn event_json(e: &Event) -> Value {
    json!({
        "id": e.id,
        "seq": e.seq,
        "topic": e.topic,
        "at": e.created_at,
        "actor": e.payload.get("actor"),
    })
}

async fn log_run(
    tx: &mut Tx,
    rule: &Rule,
    input: &Input,
    outcome: &str,
    reason: Option<&str>,
    actions: &[ActionResult],
) -> ApiResult<()> {
    let actions = serde_json::to_value(actions).map_err(ApiError::internal)?;
    core_db::rules::record_run(
        tx,
        core_db::rules::NewRun {
            rule_id: rule.id,
            event_id: input.event.as_ref().map(|e| e.id),
            object_id: input.object_id,
            outcome,
            reason,
            actions: &actions,
        },
    )
    .await?;
    Ok(())
}

fn explain(checks: &[Check]) -> String {
    let failed: Vec<String> = checks
        .iter()
        .filter(|c| !c.passed)
        .map(Check::to_string)
        .collect();
    if failed.is_empty() {
        "conditions not met".into()
    } else {
        format!("conditions not met: {}", failed.join("; "))
    }
}

async fn recipients(tx: &mut Tx, to: &Recipient, ctx_json: &Value) -> ApiResult<Vec<String>> {
    let list = match to {
        Recipient::Address { address } => vec![address.clone()],
        Recipient::Field { field } => match core_engines::path::lookup(ctx_json, field) {
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            _ => vec![],
        },
        Recipient::Role { role } => core_db::members::list(tx)
            .await?
            .into_iter()
            .filter(|m| m.role == *role)
            .map(|m| m.email)
            .collect(),
    };
    let mut out: Vec<String> = list
        .into_iter()
        .map(|a| a.trim().to_owned())
        .filter(|a| core_engines::fields::is_email(a))
        .collect();
    out.sort();
    out.dedup();
    out.truncate(MAX_RECIPIENTS);
    Ok(out)
}

/// A task title with its template variables filled in. A title that does
/// not render is used as written.
fn task_title(title: &str, ctx_json: &Value) -> String {
    let options = core_engines::templates::Options {
        locale: ctx_json
            .pointer("/workspace/locale")
            .and_then(Value::as_str)
            .unwrap_or("en-GB")
            .to_owned(),
        timezone: ctx_json
            .pointer("/workspace/timezone")
            .and_then(Value::as_str)
            .and_then(|t| t.parse().ok())
            .unwrap_or(chrono_tz::UTC),
    };
    core_engines::templates::render(
        core_engines::templates::Kind::Notification,
        None,
        title,
        Default::default(),
        ctx_json,
        &options,
    )
    .map(|r| r.body.trim().chars().take(300).collect())
    .unwrap_or_else(|_| title.trim().to_owned())
}

/// Works out what an action would do, without doing it.
async fn plan(
    tx: &mut Tx,
    action: &Action,
    entity: &str,
    ctx_json: &Value,
) -> ApiResult<ActionResult> {
    let (kind, detail) = match action {
        Action::GenerateDocument { template } => {
            ("generate_document", json!({ "template": template }))
        }
        Action::CreateInvoice { purpose, issue } => (
            "create_invoice",
            json!({ "purpose": purpose, "issue": issue }),
        ),
        Action::SetBookingStatus { status } => ("set_booking_status", json!({ "status": status })),
        Action::SendSms { template, to } | Action::SendWhatsapp { template, to } => {
            let kind = if matches!(action, Action::SendSms { .. }) {
                "send_sms"
            } else {
                "send_whatsapp"
            };
            let path = to
                .as_deref()
                .or_else(|| core_engines::rules::default_contact_path(entity));
            let contact = path
                .and_then(|p| core_engines::path::lookup(ctx_json, p))
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<Uuid>().ok());
            (kind, json!({ "template": template, "contact_id": contact }))
        }
        Action::SendEmail { template, to } => {
            let to = recipients(tx, to, ctx_json).await?;
            ("send_email", json!({ "template": template, "to": to }))
        }
        Action::SetField { field, value } => {
            ("set_field", json!({ "field": field, "value": value }))
        }
        Action::MoveStage { stage } => ("move_stage", json!({ "stage": stage })),
        Action::CallWebhook { webhook_id } => ("call_webhook", json!({ "webhook_id": webhook_id })),
        Action::CreateTask {
            title,
            due_in_minutes,
            assignee_id,
        } => (
            "create_task",
            json!({
                "title": task_title(title, ctx_json),
                "due_at": due_in_minutes.map(|m| Utc::now() + chrono::Duration::minutes(m)),
                "assignee_id": assignee_id,
            }),
        ),
    };
    Ok(ActionResult {
        kind: kind.into(),
        detail,
        ok: true,
        error: None,
    })
}

#[allow(clippy::too_many_arguments)]
async fn act(
    state: &AppState,
    tx: &mut Tx,
    workspace: WorkspaceId,
    rule: &Rule,
    index: usize,
    action: &Action,
    input: &Input,
    ctx_json: &Value,
    planned: ActionResult,
) -> ApiResult<ActionResult> {
    let failed = |planned: ActionResult, error: String| ActionResult {
        ok: false,
        error: Some(error),
        ..planned
    };
    let rule_id = rule.id.to_string();
    let index = index.to_string();
    // Only expected failures (a check, a conflict) fail the action; anything
    // else fails the run so the job retries.
    let outcome = |planned: ActionResult, r: ApiResult<Value>| match r {
        Ok(detail) => Ok(ActionResult { detail, ..planned }),
        Err(err @ ApiError::Internal(_)) => Err(err),
        Err(err) => Ok(failed(planned, err.to_string())),
    };
    match action {
        Action::GenerateDocument { template } => {
            let Some(object_id) = input.object_id else {
                return Ok(failed(planned, "there is no record to make it from".into()));
            };
            let ctx = rule_ctx(workspace, rule.id, input.depth.saturating_add(1));
            let r =
                super::documents::generate_in(state, tx, &ctx, template, &input.entity, object_id)
                    .await
                    .map(|(id, _)| json!({ "template": template, "document_id": id }));
            outcome(planned, r)
        }
        Action::CreateInvoice { purpose, issue } => {
            let Some(object_id) = input
                .object_id
                .filter(|_| input.entity == super::bookings::ENTITY)
            else {
                return Ok(failed(planned, "there is no booking to invoice".into()));
            };
            let ctx = rule_ctx(workspace, rule.id, input.depth.saturating_add(1));
            let r = async {
                let id =
                    super::invoices::from_booking_in(state, tx, &ctx, object_id, purpose, None)
                        .await?;
                if *issue {
                    super::invoices::issue_in(state, tx, &ctx, id, Default::default(), None)
                        .await?;
                }
                Ok(json!({ "purpose": purpose, "issue": issue, "invoice_id": id }))
            }
            .await;
            outcome(planned, r)
        }
        Action::SetBookingStatus { status } => {
            let Some(object_id) = input
                .object_id
                .filter(|_| input.entity == super::bookings::ENTITY)
            else {
                return Ok(failed(planned, "there is no booking to change".into()));
            };
            let Some(status) = core_domain::BookingStatus::parse(status) else {
                return Ok(failed(
                    planned,
                    format!("`{status}` is not a booking status"),
                ));
            };
            let ctx = rule_ctx(workspace, rule.id, input.depth.saturating_add(1));
            let req = super::bookings::StatusRequest {
                status,
                reason: None,
                override_reason: None,
            };
            let r = super::bookings::change_status_in_tx(tx, &ctx, object_id, req, None)
                .await
                .map(|b| json!({ "status": b.status }));
            outcome(planned, r)
        }
        Action::SendEmail { template, .. } => {
            let to: Vec<String> = planned
                .detail
                .get("to")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            if to.is_empty() {
                return Ok(failed(planned, "no valid recipient".into()));
            }
            // Everything but the workspace (re-read when sending, so branding is current).
            let mut snapshot = ctx_json.clone();
            if let Some(obj) = snapshot.as_object_mut() {
                obj.remove("workspace");
            }
            for address in &to {
                let payload = json!({
                    "template": template,
                    "to": address,
                    "rule_id": rule.id,
                    "event_id": input.event.as_ref().map(|e| e.id),
                    "object_id": input.object_id,
                    "context": snapshot,
                });
                let key = dedupe_key(&[&rule_id, &input.occurrence, &index, address]);
                core_db::jobs::enqueue(
                    tx,
                    NewJob::new(Some(workspace), JOB_MAIL, &payload).dedupe(&key),
                )
                .await?;
            }
            Ok(planned)
        }
        Action::SendSms { template, .. } | Action::SendWhatsapp { template, .. } => {
            let kind = if matches!(action, Action::SendSms { .. }) {
                "sms"
            } else {
                "whatsapp"
            };
            let Some(contact) = planned
                .detail
                .get("contact_id")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<Uuid>().ok())
            else {
                return Ok(failed(planned, "there is no contact to text".into()));
            };
            let ctx = rule_ctx(workspace, rule.id, input.depth.saturating_add(1));
            let r = super::messaging::from_rule(state, tx, &ctx, kind, template, contact, ctx_json)
                .await;
            outcome(planned, r)
        }
        Action::CallWebhook { webhook_id } => {
            let Some(event) = &input.event else {
                return Ok(failed(planned, "there is no event to send".into()));
            };
            match core_db::webhooks::get(tx, *webhook_id).await? {
                Some(endpoint) if endpoint.enabled => {
                    let payload = json!({ "endpoint_id": webhook_id, "event_id": event.id });
                    let key =
                        dedupe_key(&[&rule_id, &event.id.to_string(), &webhook_id.to_string()]);
                    core_db::jobs::enqueue(
                        tx,
                        NewJob::new(Some(workspace), JOB_WEBHOOK, &payload).dedupe(&key),
                    )
                    .await?;
                    Ok(planned)
                }
                Some(_) => Ok(failed(planned, "the webhook is disabled".into())),
                None => Ok(failed(planned, "the webhook no longer exists".into())),
            }
        }
        Action::CreateTask { assignee_id, .. } => {
            let ctx = rule_ctx(workspace, rule.id, input.depth.saturating_add(1));
            let related = input
                .object_id
                .filter(|_| super::tasks::RELATED.contains(&input.entity.as_str()));
            let new = super::tasks::NewTask {
                title: planned
                    .detail
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("Task")
                    .to_owned(),
                notes: None,
                due_at: planned
                    .detail
                    .get("due_at")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse().ok()),
                assignee_id: *assignee_id,
                related_entity: related.map(|_| input.entity.clone()),
                related_id: related,
            };
            match super::tasks::create_in_tx(tx, &ctx, new, Some(rule.id)).await {
                Ok(task) => Ok(ActionResult {
                    detail: json!({ "task_id": task.id, "title": task.title, "due_at": task.due_at }),
                    ..planned
                }),
                Err(err @ ApiError::Internal(_)) => Err(err),
                Err(err) => Ok(failed(planned, err.to_string())),
            }
        }
        Action::SetField { .. } | Action::MoveStage { .. } => {
            let Some(object_id) = input.object_id else {
                return Ok(failed(planned, "there is no record to change".into()));
            };
            // The change is made by the rule, one level deeper in any chain.
            let ctx = rule_ctx(workspace, rule.id, input.depth.saturating_add(1));
            let result = if input.entity == super::enquiries::ENTITY {
                let patch = match enquiry_patch(action) {
                    Ok(p) => p,
                    Err(msg) => return Ok(failed(planned, msg)),
                };
                super::enquiries::update_in_tx(tx, &ctx, object_id, patch, None)
                    .await
                    .map(|_| ())
            } else if input.entity == super::bookings::ENTITY {
                let patch = match booking_patch(action) {
                    Ok(p) => p,
                    Err(msg) => return Ok(failed(planned, msg)),
                };
                super::bookings::update_in_tx(tx, &ctx, object_id, patch, None)
                    .await
                    .map(|_| ())
            } else if super::records::is_crm(&input.entity) {
                let edit = match action {
                    Action::SetField { field, value } => match field.as_str() {
                        "name" => match value.as_str() {
                            Some(name) => super::records::Edit {
                                name: Some(name.to_owned()),
                                ..Default::default()
                            },
                            None => return Ok(failed(planned, "name must be text".into())),
                        },
                        other => match other.strip_prefix("custom.") {
                            Some(key) => {
                                let mut changes = serde_json::Map::new();
                                changes.insert(key.to_owned(), value.clone());
                                super::records::Edit {
                                    custom: Some(changes),
                                    ..Default::default()
                                }
                            }
                            None => {
                                let msg = format!("{} records have no `{other}`", input.entity);
                                return Ok(failed(planned, msg));
                            }
                        },
                    },
                    _ => {
                        let msg = format!("{} records are not in a pipeline", input.entity);
                        return Ok(failed(planned, msg));
                    }
                };
                super::records::edit(tx, &ctx, &input.entity, object_id, edit, None).await
            } else {
                let patch = match thing_patch(action) {
                    Ok(p) => p,
                    Err(msg) => return Ok(failed(planned, msg)),
                };
                things::update_in_tx(tx, &ctx, object_id, patch, None)
                    .await
                    .map(|_| ())
            };
            match result {
                Ok(()) => Ok(planned),
                Err(err @ ApiError::Internal(_)) => Err(err),
                Err(err) => Ok(failed(planned, err.to_string())),
            }
        }
    }
}

fn custom_change(field: &str, value: &Value) -> Option<serde_json::Map<String, Value>> {
    let key = field.strip_prefix("custom.")?;
    let mut changes = serde_json::Map::new();
    changes.insert(key.to_owned(), value.clone());
    Some(changes)
}

/// A set-field or move-stage action as a change to an enquiry.
fn enquiry_patch(action: &Action) -> Result<super::enquiries::EnquiryPatch, String> {
    use super::enquiries::EnquiryPatch;
    Ok(match action {
        Action::MoveStage { stage } => EnquiryPatch {
            stage: Some(stage.clone()),
            ..Default::default()
        },
        Action::SetField { field, value } => match (field.as_str(), value) {
            ("title", Value::String(t)) => EnquiryPatch {
                title: Some(t.clone()),
                ..Default::default()
            },
            (f, v) => match custom_change(f, v) {
                Some(custom) => EnquiryPatch {
                    custom: Some(custom),
                    ..Default::default()
                },
                None => return Err(format!("enquiries have no `{f}` a rule can set")),
            },
        },
        _ => return Err("not a change to the record".into()),
    })
}

/// A set-field action as a change to a booking (bookings have statuses, not stages).
fn booking_patch(action: &Action) -> Result<super::bookings::BookingPatch, String> {
    use super::bookings::BookingPatch;
    Ok(match action {
        Action::SetField { field, value } => match (field.as_str(), value) {
            ("title", Value::String(t)) => BookingPatch {
                title: Some(t.clone()),
                ..Default::default()
            },
            (f, v) => match custom_change(f, v) {
                Some(custom) => BookingPatch {
                    custom: Some(custom),
                    ..Default::default()
                },
                None => return Err(format!("bookings have no `{f}` a rule can set")),
            },
        },
        _ => return Err("bookings are not in a pipeline".into()),
    })
}

/// A set-field or move-stage action as a change to a thing.
fn thing_patch(action: &Action) -> Result<things::Patch, String> {
    Ok(match action {
        Action::MoveStage { stage } => things::Patch {
            stage: Some(stage.clone()),
            ..Default::default()
        },
        Action::SetField { field, value } => match field.as_str() {
            "name" => match value.as_str() {
                Some(name) => things::Patch {
                    name: Some(name.to_owned()),
                    ..Default::default()
                },
                None => return Err("name must be text".into()),
            },
            "due_at" => match value {
                Value::Null => things::Patch {
                    due_at: Some(None),
                    ..Default::default()
                },
                Value::String(s) => match DateTime::parse_from_rfc3339(s) {
                    Ok(at) => things::Patch {
                        due_at: Some(Some(at.with_timezone(&Utc))),
                        ..Default::default()
                    },
                    Err(_) => return Err("due_at must be a date-time".into()),
                },
                _ => return Err("due_at must be a date-time".into()),
            },
            custom => {
                let key = custom.strip_prefix("custom.").unwrap_or(custom);
                let mut changes = serde_json::Map::new();
                changes.insert(key.to_owned(), value.clone());
                things::Patch {
                    custom: Some(changes),
                    ..Default::default()
                }
            }
        },
        _ => return Err("not a change to the record".into()),
    })
}

/// Runs one rule for one trigger occurrence, logging the outcome.
async fn run_rule(
    state: &AppState,
    tx: &mut Tx,
    workspace: WorkspaceId,
    rule: &Rule,
    def: &RuleDef,
    input: Input,
    workspace_json: &Value,
) -> ApiResult<()> {
    if input.depth >= MAX_DEPTH {
        return log_run(
            tx,
            rule,
            &input,
            "skipped",
            Some("stopped: rules were triggering each other (loop guard)"),
            &[],
        )
        .await;
    }
    let ctx_json = context::build(
        workspace_json.clone(),
        &input.entity,
        input.object.clone(),
        input.before.clone(),
        input.event.as_ref().map(event_json),
    );
    let (matched, checks) = conditions::evaluate(&def.conditions, &ctx_json);
    if !matched {
        return log_run(tx, rule, &input, "skipped", Some(&explain(&checks)), &[]).await;
    }
    let once = rule.once_per_object && def.trigger.has_object();

    let mut planned = Vec::with_capacity(def.actions.len());
    for action in &def.actions {
        planned.push(plan(tx, action, &input.entity, &ctx_json).await?);
    }
    if rule.dry_run {
        return log_run(tx, rule, &input, "dry_run", None, &planned).await;
    }
    if once
        && let Some(object_id) = input.object_id
        && !core_db::rules::claim_fire(tx, rule.id, object_id).await?
    {
        return log_run(
            tx,
            rule,
            &input,
            "skipped",
            Some("already ran for this record"),
            &[],
        )
        .await;
    }

    let mut done = Vec::with_capacity(planned.len());
    let mut failure = None;
    for (i, (action, plan)) in def.actions.iter().zip(planned).enumerate() {
        // A failed action leaves nothing behind (a draft invoice that was
        // not issued, say), and cannot leave the transaction aborted.
        sqlx::query("SAVEPOINT rule_action")
            .execute(&mut **tx)
            .await?;
        let result = act(
            state, tx, workspace, rule, i, action, &input, &ctx_json, plan,
        )
        .await?;
        let undo = if result.ok {
            "RELEASE SAVEPOINT rule_action"
        } else {
            "ROLLBACK TO SAVEPOINT rule_action"
        };
        sqlx::query(undo).execute(&mut **tx).await?;
        let ok = result.ok;
        if !ok {
            failure = result.error.clone();
        }
        done.push(result);
        if !ok {
            break;
        }
    }
    match failure {
        None => log_run(tx, rule, &input, "acted", None, &done).await,
        Some(reason) => log_run(tx, rule, &input, "failed", Some(&reason), &done).await,
    }
}

/// The record an event is about, as rules and templates see it: its current
/// state if it still exists, else the event's last snapshot. Also says what
/// kind of record it is.
async fn event_object(
    tx: &mut Tx,
    state: &AppState,
    event: &Event,
) -> ApiResult<(Option<Uuid>, Option<Value>, String)> {
    let object_id = event
        .payload
        .get("object_id")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<Uuid>().ok());
    let resource = event
        .payload
        .get("resource")
        .and_then(Value::as_str)
        .unwrap_or(things::ENTITY)
        .to_owned();
    if context::is_entity(&resource)
        && let Some(id) = object_id
        && let Some(object) = context::load_object(tx, state, &resource, id).await?
    {
        return Ok((object_id, Some(object), resource));
    }
    let snapshot = event
        .payload
        .get("after")
        .filter(|v| !v.is_null())
        .or_else(|| event.payload.get("before"))
        .cloned();
    Ok((object_id, snapshot, resource))
}

/// Records made by an import trigger rules only when the import asked.
fn rules_wanted(event: &Event) -> bool {
    event
        .payload
        .pointer("/import/run_rules")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

fn depth_of(event: &Event) -> u8 {
    event
        .payload
        .pointer("/actor/rule_depth")
        .and_then(Value::as_u64)
        .map_or(0, |d| d.min(u64::from(u8::MAX)) as u8)
}

/// `event.dispatch`: runs the event's rules and queues its webhook deliveries.
pub async fn dispatch_event(
    state: &AppState,
    workspace: WorkspaceId,
    event_id: Uuid,
) -> ApiResult<()> {
    let mut tx = core_db::begin(&state.db, Some(workspace), None).await?;
    let Some(event) = core_db::outbox::get(&mut tx, event_id).await? else {
        // Pruned, or the workspace's data is gone: nothing to do.
        return Ok(());
    };

    // Webhooks first: they don't depend on the kill switch.
    let endpoints = core_db::webhooks::list(&mut tx).await?;
    let ping_target = (event.topic == super::webhooks::PING_TOPIC)
        .then(|| {
            event
                .payload
                .get("object_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .flatten();
    for endpoint in endpoints.iter().filter(|e| e.enabled) {
        let wanted = match &ping_target {
            Some(target) => *target == endpoint.id.to_string(),
            None => endpoint
                .events
                .iter()
                .any(|p| core_engines::webhooks::topic_matches(p, &event.topic)),
        };
        if wanted {
            let payload = json!({ "endpoint_id": endpoint.id, "event_id": event.id });
            let key = dedupe_key(&[&endpoint.id.to_string(), &event.id.to_string()]);
            core_db::jobs::enqueue(
                &mut tx,
                NewJob::new(Some(workspace), JOB_WEBHOOK, &payload).dedupe(&key),
            )
            .await?;
        }
    }

    // Bookings written into members' own calendars.
    super::calendar_push::booking_changed(
        &mut tx,
        workspace,
        &event.topic,
        &event.payload,
        event.id,
    )
    .await?;
    // Invoices and payments sent on to the workspace's books.
    super::accounting::changed(&mut tx, workspace, &event.topic).await?;

    let rules = if rules_wanted(&event) {
        core_db::rules::for_event(&mut tx, &event.topic).await?
    } else {
        Vec::new()
    };
    if !rules.is_empty() {
        let paused = core_db::workspaces::automations_paused(&mut tx).await?;
        let catalog = super::rules::catalog(&mut tx).await?;
        let (workspace_json, _) = context::workspace(&mut tx, state).await?;
        let (object_id, object, entity) = event_object(&mut tx, state, &event).await?;
        let before = event
            .payload
            .get("before")
            .filter(|v| !v.is_null())
            .cloned();
        for rule in &rules {
            let input = Input {
                event: Some(event.clone()),
                entity: entity.clone(),
                object: object.clone(),
                object_id,
                before: before.clone(),
                depth: depth_of(&event),
                occurrence: event.id.to_string(),
            };
            if paused {
                log_run(
                    &mut tx,
                    rule,
                    &input,
                    "skipped",
                    Some("automations are paused"),
                    &[],
                )
                .await?;
                continue;
            }
            match super::rules::parse_stored(rule, &catalog) {
                Ok(def) => {
                    run_rule(
                        state,
                        &mut tx,
                        workspace,
                        rule,
                        &def,
                        input,
                        &workspace_json,
                    )
                    .await?
                }
                Err(reason) => {
                    log_run(
                        &mut tx,
                        rule,
                        &input,
                        "failed",
                        Some(&format!("the rule is invalid: {reason}")),
                        &[],
                    )
                    .await?
                }
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

/// A job the scheduler queues for a time-based rule that has come due.
#[derive(Debug, Clone)]
pub struct Planned {
    pub kind: &'static str,
    pub payload: Value,
    pub dedupe_key: String,
}

/// What to queue for a due rule, and when the rule is next due (`None`
/// stops it being scheduled).
pub fn plan_due(
    due: &core_db::rules::Due,
    now: DateTime<Utc>,
) -> (Option<Planned>, Option<DateTime<Utc>>) {
    let Ok(trigger) = serde_json::from_value::<Trigger>(due.trigger.clone()) else {
        return (None, None);
    };
    let at = due.next_run_at.to_rfc3339();
    let rule = due.id.to_string();
    match &trigger {
        Trigger::Event { .. } => (None, None),
        Trigger::DateOffset { .. } => (
            Some(Planned {
                kind: JOB_SCAN,
                payload: json!({ "rule_id": due.id, "until": now }),
                dedupe_key: dedupe_key(&[JOB_SCAN, &rule, &at]),
            }),
            Some(now + super::rules::SCAN_INTERVAL),
        ),
        Trigger::Schedule { .. } => {
            let tz = due.default_timezone.parse().unwrap_or(chrono_tz::UTC);
            let (next, _) = super::rules::schedule(&trigger, tz, now.max(due.next_run_at));
            (
                Some(Planned {
                    kind: JOB_SCHEDULED,
                    payload: json!({ "rule_id": due.id, "fire_at": due.next_run_at }),
                    dedupe_key: dedupe_key(&[JOB_SCHEDULED, &rule, &at]),
                }),
                next,
            )
        }
    }
}

/// `rule.scan`: a date-offset rule's records whose date arrived by `until`.
pub async fn scan_rule(
    state: &AppState,
    workspace: WorkspaceId,
    rule_id: Uuid,
    until: DateTime<Utc>,
) -> ApiResult<()> {
    let mut tx = core_db::begin(&state.db, Some(workspace), None).await?;
    let Some(rule) = core_db::rules::get(&mut tx, rule_id)
        .await?
        .filter(|r| r.enabled)
    else {
        return Ok(());
    };
    let catalog = super::rules::catalog(&mut tx).await?;
    let def = match super::rules::parse_stored(&rule, &catalog) {
        Ok(def) => def,
        Err(reason) => {
            let input = Input {
                event: None,
                entity: things::ENTITY.into(),
                object: None,
                object_id: None,
                before: None,
                depth: 0,
                occurrence: until.to_rfc3339(),
            };
            log_run(
                &mut tx,
                &rule,
                &input,
                "failed",
                Some(&format!("the rule is invalid: {reason}")),
                &[],
            )
            .await?;
            tx.commit().await?;
            return Ok(());
        }
    };
    let Trigger::DateOffset {
        entity,
        field,
        offset_minutes,
    } = &def.trigger
    else {
        return Ok(());
    };
    let from = rule.scanned_until.unwrap_or(rule.created_at);
    if until <= from {
        return Ok(());
    }
    let (workspace_json, options) = context::workspace(&mut tx, state).await?;
    let is_booking = entity == super::bookings::ENTITY;
    let mut reached: Vec<(Uuid, DateTime<Utc>)> = if entity == super::enquiries::ENTITY {
        core_db::enquiries::date_between(
            &mut tx,
            field,
            options.timezone.name(),
            *offset_minutes,
            from,
            until,
            SCAN_BATCH,
        )
        .await?
        .into_iter()
        .map(|r| (r.id, r.at))
        .collect()
    } else if entity == super::quotes::ENTITY {
        core_db::quotes::date_between(
            &mut tx,
            field,
            options.timezone.name(),
            *offset_minutes,
            from,
            until,
            SCAN_BATCH,
        )
        .await?
        .into_iter()
        .map(|r| (r.id, r.at))
        .collect()
    } else if entity == super::invoices::ENTITY {
        core_db::invoices::date_between(
            &mut tx,
            field,
            options.timezone.name(),
            *offset_minutes,
            from,
            until,
            SCAN_BATCH,
        )
        .await?
        .into_iter()
        .map(|r| (r.id, r.at))
        .collect()
    } else if entity == super::documents::ENTITY {
        core_db::documents::expiring_between(
            &mut tx,
            options.timezone.name(),
            *offset_minutes,
            from,
            until,
            SCAN_BATCH,
        )
        .await?
    } else {
        match (is_booking, field.strip_prefix("custom.")) {
            (false, None) => {
                core_db::things::due_between(&mut tx, *offset_minutes, from, until, SCAN_BATCH)
                    .await?
                    .into_iter()
                    .map(|r| (r.id, r.at))
                    .collect()
            }
            (false, Some(key)) => core_db::things::custom_date_between(
                &mut tx,
                key,
                options.timezone.name(),
                *offset_minutes,
                from,
                until,
                SCAN_BATCH,
            )
            .await?
            .into_iter()
            .map(|r| (r.id, r.at))
            .collect(),
            (true, None) => core_db::bookings::date_between(
                &mut tx,
                field,
                *offset_minutes,
                from,
                until,
                SCAN_BATCH,
            )
            .await?
            .into_iter()
            .map(|r| (r.id, r.at))
            .collect(),
            (true, Some(key)) => core_db::bookings::custom_date_between(
                &mut tx,
                key,
                *offset_minutes,
                from,
                until,
                SCAN_BATCH,
            )
            .await?
            .into_iter()
            .map(|r| (r.id, r.at))
            .collect(),
        }
    };
    let scanned_until = match (reached.first(), reached.last()) {
        (Some(&(_, first)), Some(&(_, last))) if reached.len() as i64 == SCAN_BATCH => {
            // A full batch may stop part way through records reaching the
            // same moment (invoices due the same day): leave those for the
            // next scan, unless the whole batch is that one moment.
            if first < last {
                reached.retain(|(_, at)| *at < last);
            }
            reached.last().map_or(last, |&(_, at)| at)
        }
        _ => until,
    };
    let paused = core_db::workspaces::automations_paused(&mut tx).await?;
    if !paused {
        for (id, at) in &reached {
            let Some(object) = context::load_object(&mut tx, state, entity, *id).await? else {
                continue;
            };
            let input = Input {
                event: None,
                entity: entity.clone(),
                object: Some(object),
                object_id: Some(*id),
                before: None,
                depth: 0,
                occurrence: format!("{id}@{}", at.to_rfc3339()),
            };
            run_rule(
                state,
                &mut tx,
                workspace,
                &rule,
                &def,
                input,
                &workspace_json,
            )
            .await?;
        }
    }
    core_db::rules::set_scanned_until(&mut tx, rule.id, scanned_until).await?;
    tx.commit().await?;
    Ok(())
}

/// `rule.scheduled`: one run of a cron rule.
pub async fn run_scheduled(
    state: &AppState,
    workspace: WorkspaceId,
    rule_id: Uuid,
    fire_at: DateTime<Utc>,
) -> ApiResult<()> {
    let mut tx = core_db::begin(&state.db, Some(workspace), None).await?;
    let Some(rule) = core_db::rules::get(&mut tx, rule_id)
        .await?
        .filter(|r| r.enabled)
    else {
        return Ok(());
    };
    let input = Input {
        event: None,
        entity: things::ENTITY.into(),
        object: None,
        object_id: None,
        before: None,
        depth: 0,
        occurrence: fire_at.to_rfc3339(),
    };
    if core_db::workspaces::automations_paused(&mut tx).await? {
        log_run(
            &mut tx,
            &rule,
            &input,
            "skipped",
            Some("automations are paused"),
            &[],
        )
        .await?;
    } else {
        let catalog = super::rules::catalog(&mut tx).await?;
        let (workspace_json, _) = context::workspace(&mut tx, state).await?;
        match super::rules::parse_stored(&rule, &catalog) {
            Ok(def) => {
                run_rule(
                    state,
                    &mut tx,
                    workspace,
                    &rule,
                    &def,
                    input,
                    &workspace_json,
                )
                .await?
            }
            Err(reason) => {
                log_run(
                    &mut tx,
                    &rule,
                    &input,
                    "failed",
                    Some(&format!("the rule is invalid: {reason}")),
                    &[],
                )
                .await?
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

/// The answer to "what would this rule do for this record?".
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct DryRun {
    pub matched: bool,
    /// Each comparison made, with the value found.
    pub checks: Vec<Check>,
    /// What the rule would do, if it matched.
    pub actions: Vec<ActionResult>,
    /// Why it would not act even though it matched, if so.
    pub note: Option<String>,
}

/// Evaluates a rule against a record (or sample data) without acting or
/// writing anything.
pub async fn dry_run(
    state: &AppState,
    ctx: &Ctx,
    rule_id: Uuid,
    record_id: Option<Uuid>,
) -> ApiResult<DryRun> {
    ctx.authorize(Permission::WorkspaceRead)?;
    let mut tx = super::begin(state, ctx).await?;
    let rule = core_db::rules::get(&mut tx, rule_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let catalog = super::rules::catalog(&mut tx).await?;
    let def = super::rules::parse_stored(&rule, &catalog)
        .map_err(|r| ApiError::unprocessable("rule_invalid", r))?;
    let (workspace_json, _) = context::workspace(&mut tx, state).await?;
    let entity = super::rules::entity_of(&def.trigger);
    let object = match record_id {
        Some(id) => {
            ctx.authorize(context::read_permission(&entity))?;
            context::load_object(&mut tx, state, &entity, id)
                .await?
                .ok_or(ApiError::NotFound)?
        }
        None => context::sample(&mut tx, state, &entity).await?,
    };
    let topic = match &def.trigger {
        Trigger::Event { topic } => topic.clone(),
        other => format!("rule.{}", other.kind()),
    };
    let event = json!({ "id": core_domain::ids::new_id(), "topic": topic, "at": Utc::now() });
    let ctx_json = context::build(
        workspace_json,
        &entity,
        Some(object.clone()),
        Some(object),
        Some(event),
    );
    let (matched, checks) = conditions::evaluate(&def.conditions, &ctx_json);
    let mut actions = Vec::new();
    for action in &def.actions {
        actions.push(plan(&mut tx, action, &entity, &ctx_json).await?);
    }
    let note = if core_db::workspaces::automations_paused(&mut tx).await? {
        Some("automations are paused in this workspace".to_owned())
    } else if !rule.enabled {
        Some("the rule is turned off".to_owned())
    } else if rule.dry_run {
        Some("the rule is in dry-run mode: it logs instead of acting".to_owned())
    } else {
        None
    };
    // Nothing above writes; roll back to be sure.
    tx.rollback().await?;
    Ok(DryRun {
        matched,
        checks,
        actions,
        note,
    })
}
