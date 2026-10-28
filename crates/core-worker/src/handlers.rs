//! What each kind of job does. Tenant work goes through the API's services,
//! which open their own transactions with the job's workspace in context.

use chrono::{DateTime, Utc};
use core_api::error::ApiError;
use core_api::services::automation::{
    self, JOB_DISPATCH, JOB_MAIL, JOB_SCAN, JOB_SCHEDULED, JOB_SYSTEM_MAIL, JOB_WEBHOOK,
};
use core_api::services::bookings::{self, JOB_COMPLETE, JOB_EXPIRE_HOLD};
use core_api::services::quotes::{self, JOB_EXPIRE as JOB_EXPIRE_QUOTE};
use core_api::services::{
    calendar_push, documents, files, imports, inbox, integrations, invoices, mail, sender_domains,
    stripe, talent_calendars, webhooks,
};
use core_db::jobs::{Claimed, LogLine};
use core_domain::WorkspaceId;
use core_integrations::http::DeliveryError;
use core_integrations::mail::MailError;
use serde_json::Value;
use uuid::Uuid;

use crate::Worker;

/// Why a job failed, and whether trying again could help.
#[derive(Debug, Clone)]
pub struct JobError {
    pub message: String,
    pub permanent: bool,
}

impl JobError {
    pub fn retry(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            permanent: false,
        }
    }

    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            permanent: true,
        }
    }
}

impl From<ApiError> for JobError {
    fn from(err: ApiError) -> Self {
        let message = match &err {
            ApiError::Internal(detail) => format!("internal error: {detail}"),
            ApiError::Invalid(issues) => issues
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
            other => other.to_string(),
        };
        // Bad input stays bad; database and other internal trouble may pass.
        let permanent = !matches!(
            err,
            ApiError::Internal(_)
                | ApiError::Conflict { .. }
                | ApiError::PreconditionFailed { .. }
                | ApiError::Unavailable { .. }
        );
        Self { message, permanent }
    }
}

fn note(log: &mut Vec<LogLine>, message: impl Into<String>) {
    log.push(LogLine {
        at: Utc::now(),
        message: message.into(),
    });
}

fn uuid_field(payload: &Value, key: &str) -> Result<Uuid, JobError> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| JobError::permanent(format!("the job has no `{key}`")))
}

fn time_field(payload: &Value, key: &str) -> Result<DateTime<Utc>, JobError> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| JobError::permanent(format!("the job has no `{key}`")))
}

fn workspace(job: &Claimed) -> Result<WorkspaceId, JobError> {
    job.workspace_id
        .ok_or_else(|| JobError::permanent("the job has no workspace"))
}

impl Worker {
    pub(crate) async fn handle(
        &self,
        job: &Claimed,
        log: &mut Vec<LogLine>,
    ) -> Result<(), JobError> {
        match job.kind.as_str() {
            JOB_DISPATCH => {
                let event_id = uuid_field(&job.payload, "event_id")?;
                automation::dispatch_event(&self.app, workspace(job)?, event_id).await?;
                Ok(())
            }
            JOB_SCAN => {
                let rule_id = uuid_field(&job.payload, "rule_id")?;
                let until = time_field(&job.payload, "until")?;
                automation::scan_rule(&self.app, workspace(job)?, rule_id, until).await?;
                Ok(())
            }
            JOB_SCHEDULED => {
                let rule_id = uuid_field(&job.payload, "rule_id")?;
                let fire_at = time_field(&job.payload, "fire_at")?;
                automation::run_scheduled(&self.app, workspace(job)?, rule_id, fire_at).await?;
                Ok(())
            }
            JOB_EXPIRE_HOLD => {
                let id = uuid_field(&job.payload, "booking_id")?;
                bookings::expire_hold(&self.app, workspace(job)?, id).await?;
                Ok(())
            }
            JOB_COMPLETE => {
                let id = uuid_field(&job.payload, "booking_id")?;
                bookings::complete(&self.app, workspace(job)?, id).await?;
                Ok(())
            }
            JOB_EXPIRE_QUOTE => {
                let id = uuid_field(&job.payload, "quote_id")?;
                quotes::expire(&self.app, workspace(job)?, id).await?;
                Ok(())
            }
            JOB_SYSTEM_MAIL => {
                let out = mail::prepare_system(&self.app, job).await?;
                self.send_mail(job, out, log).await
            }
            JOB_MAIL => {
                let out = mail::prepare_rule_mail(&self.app, job).await?;
                self.send_mail(job, out, log).await
            }
            files::JOB_READ => {
                let id = uuid_field(&job.payload, "file_id")?;
                files::read(&self.app, workspace(job)?, id).await?;
                Ok(())
            }
            documents::JOB_RENDER => {
                let id = uuid_field(&job.payload, "document_id")?;
                let number = job
                    .payload
                    .get("version")
                    .and_then(Value::as_i64)
                    .and_then(|n| i32::try_from(n).ok())
                    .ok_or_else(|| JobError::permanent("the job has no `version`"))?;
                documents::render_version(&self.app, workspace(job)?, id, number).await?;
                Ok(())
            }
            documents::JOB_SEAL => {
                let id = uuid_field(&job.payload, "document_id")?;
                let round = uuid_field(&job.payload, "round_id")?;
                documents::seal_round(&self.app, workspace(job)?, id, round).await?;
                Ok(())
            }
            invoices::JOB_OVERDUE => {
                let id = uuid_field(&job.payload, "invoice_id")?;
                invoices::mark_overdue(&self.app, workspace(job)?, id).await?;
                Ok(())
            }
            stripe::JOB_EVENT => {
                let id = job
                    .payload
                    .get("event_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| JobError::permanent("the job has no `event_id`"))?;
                let outcome = stripe::process_event(&self.app, workspace(job)?, id).await?;
                note(log, format!("Stripe event {id}: {outcome}"));
                Ok(())
            }
            integrations::JOB_POLL => {
                let id = uuid_field(&job.payload, "integration_id")?;
                let outcome = integrations::poll(&self.app, workspace(job)?, id).await?;
                note(log, outcome.to_string());
                Ok(())
            }
            inbox::JOB_SEND => {
                let id = uuid_field(&job.payload, "message_id")?;
                let ws = workspace(job)?;
                let last = job.attempt >= job.max_attempts;
                match inbox::send(&self.app, ws, id, self.mailer.as_ref(), last).await {
                    Ok(outcome) => {
                        note(log, outcome.to_string());
                        Ok(())
                    }
                    Err(e) => {
                        let err = JobError::from(e);
                        // Not tried again: the message shows as failed
                        // rather than as sending for good.
                        if (err.permanent || last)
                            && let Err(e) = inbox::abandon(&self.app, ws, id, &err.message).await
                        {
                            tracing::error!(message_id = %id, error = %e, "could not mark a message failed");
                        }
                        Err(err)
                    }
                }
            }
            inbox::JOB_WAKE => {
                let id = uuid_field(&job.payload, "thread_id")?;
                let at = time_field(&job.payload, "at")?;
                inbox::wake(&self.app, workspace(job)?, id, at).await?;
                Ok(())
            }
            sender_domains::JOB_CHECK => {
                let id = uuid_field(&job.payload, "domain_id")?;
                let outcome = sender_domains::check(&self.app, workspace(job)?, id).await?;
                note(log, outcome.to_string());
                Ok(())
            }
            sender_domains::JOB_FORGET => {
                let id = job
                    .payload
                    .get("provider_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| JobError::permanent("the job has no `provider_id`"))?;
                sender_domains::forget(&self.app, id).await?;
                Ok(())
            }
            talent_calendars::JOB_READ => {
                let id = uuid_field(&job.payload, "calendar_id")?;
                let outcome = talent_calendars::read(&self.app, workspace(job)?, id).await?;
                note(log, outcome.to_string());
                Ok(())
            }
            talent_calendars::JOB_REVOKE => {
                let id = uuid_field(&job.payload, "calendar_id")?;
                let outcome = talent_calendars::revoke(&self.app, workspace(job)?, id).await?;
                note(log, outcome);
                Ok(())
            }
            calendar_push::JOB_WRITE => {
                let id = uuid_field(&job.payload, "push_id")?;
                let booking = match job.payload.get("booking_id") {
                    Some(_) => Some(uuid_field(&job.payload, "booking_id")?),
                    None => None,
                };
                let outcome = calendar_push::write(&self.app, workspace(job)?, id, booking).await?;
                note(log, outcome.to_string());
                Ok(())
            }
            calendar_push::JOB_REVOKE => {
                let id = uuid_field(&job.payload, "push_id")?;
                let outcome = calendar_push::revoke(&self.app, workspace(job)?, id).await?;
                note(log, outcome);
                Ok(())
            }
            JOB_WEBHOOK => self.deliver_webhook(job, log).await,
            imports::JOB_IMPORT => self.run_import(job, log).await,
            other => Err(JobError::permanent(format!(
                "no handler for job kind `{other}`"
            ))),
        }
    }

    /// Cleans up after a job that gave up for good without its handler
    /// finishing: it timed out on its last attempt, or its worker stopped.
    pub(crate) async fn gave_up(
        &self,
        workspace: Option<WorkspaceId>,
        kind: &str,
        payload: &Value,
        error: &str,
    ) {
        if kind == inbox::JOB_SEND {
            let (Some(workspace), Ok(id)) = (workspace, uuid_field(payload, "message_id")) else {
                return;
            };
            if let Err(e) = inbox::abandon(&self.app, workspace, id, error).await {
                tracing::error!(message_id = %id, error = %e, "could not mark a message failed");
            }
            return;
        }
        if kind != imports::JOB_IMPORT {
            return;
        }
        let (Some(workspace), Ok(id)) = (workspace, uuid_field(payload, "import_id")) else {
            return;
        };
        let message = format!("the import stopped: {error}");
        if let Err(e) = imports::abandon(&self.app, workspace, id, &message).await {
            tracing::error!(import_id = %id, error = %e, "could not mark an import failed");
        }
    }

    async fn run_import(&self, job: &Claimed, log: &mut Vec<LogLine>) -> Result<(), JobError> {
        let workspace = workspace(job)?;
        let id = uuid_field(&job.payload, "import_id")?;
        let result = imports::execute(&self.app, workspace, id, self.settings.import_slice).await;
        let Err(err) = result else {
            return Ok(());
        };
        let err = JobError::from(err);
        if err.permanent || job.attempt >= job.max_attempts {
            note(log, "giving up; marking the import failed");
            let message = format!("the import stopped: {}", err.message);
            if let Err(e) = imports::abandon(&self.app, workspace, id, &message).await {
                tracing::error!(job_id = %job.id, error = %e, "could not mark an import failed");
            }
        }
        Err(err)
    }

    async fn send_mail(
        &self,
        job: &Claimed,
        out: mail::Outgoing,
        log: &mut Vec<LogLine>,
    ) -> Result<(), JobError> {
        note(
            log,
            format!("sending `{}` to {}", out.template_key, out.email.to),
        );
        match self.mailer.send(&out.email).await {
            Ok(sent) => {
                note(log, "accepted by the mail server");
                // Sent but not logged is better than sent twice: don't fail the job.
                if let Err(err) = mail::record(&self.app, &out, job.id, Ok(&sent)).await {
                    tracing::error!(job_id = %job.id, error = %err, "email sent but not logged");
                    note(log, "sent, but writing the message log failed");
                }
                Ok(())
            }
            Err(err) => {
                let permanent = matches!(err, MailError::Permanent(_));
                note(log, format!("the mail server refused: {err}"));
                // Log a failed message once, when it will not be tried again.
                if (permanent || job.attempt >= job.max_attempts)
                    && let Err(e) = mail::record(&self.app, &out, job.id, Err(&err)).await
                {
                    tracing::error!(job_id = %job.id, error = %e, "could not log a failed email");
                }
                Err(JobError {
                    message: err.to_string(),
                    permanent,
                })
            }
        }
    }

    async fn deliver_webhook(&self, job: &Claimed, log: &mut Vec<LogLine>) -> Result<(), JobError> {
        let ws = workspace(job)?;
        let endpoint_id = uuid_field(&job.payload, "endpoint_id")?;
        let event_id = uuid_field(&job.payload, "event_id")?;
        let Some(prepared) =
            webhooks::prepare_delivery(&self.app, ws, endpoint_id, event_id).await?
        else {
            note(
                log,
                "the endpoint was deleted or disabled, or the event is gone; nothing sent",
            );
            return Ok(());
        };
        let timestamp = Utc::now().timestamp();
        let headers = [
            (
                "OnHold-Signature",
                core_engines::webhooks::sign(&prepared.secret, timestamp, &prepared.body),
            ),
            ("OnHold-Event-Id", event_id.to_string()),
            ("OnHold-Delivery-Attempt", job.attempt.to_string()),
            (
                "User-Agent",
                format!("OnHold-Webhooks/{}", env!("CARGO_PKG_VERSION")),
            ),
        ];
        note(log, format!("POST {}", prepared.url));
        let result = self
            .webhooks
            .post(&prepared.url, &headers, prepared.body)
            .await;
        let (outcome, error) = match &result {
            Ok(resp) => {
                let ok = (200..300).contains(&resp.status);
                note(
                    log,
                    format!(
                        "answered {} in {} ms",
                        resp.status,
                        resp.duration.as_millis()
                    ),
                );
                let error = (!ok)
                    .then(|| JobError::retry(format!("the endpoint answered {}", resp.status)));
                (
                    webhooks::Outcome {
                        succeeded: ok,
                        status_code: Some(i32::from(resp.status)),
                        error: None,
                        response_excerpt: Some(resp.excerpt.as_str()),
                        duration_ms: millis(resp.duration),
                    },
                    error,
                )
            }
            Err(DeliveryError::Blocked(why)) => {
                note(log, format!("not sent: {why}"));
                (
                    webhooks::Outcome {
                        succeeded: false,
                        status_code: None,
                        error: Some(why.as_str()),
                        response_excerpt: None,
                        duration_ms: 0,
                    },
                    Some(JobError::permanent(why.clone())),
                )
            }
            Err(DeliveryError::Failed { message, duration }) => {
                note(log, format!("failed: {message}"));
                (
                    webhooks::Outcome {
                        succeeded: false,
                        status_code: None,
                        error: Some(message.as_str()),
                        response_excerpt: None,
                        duration_ms: millis(*duration),
                    },
                    Some(JobError::retry(message.clone())),
                )
            }
        };
        webhooks::record_delivery(
            &self.app,
            ws,
            endpoint_id,
            event_id,
            job.id,
            job.attempt,
            outcome,
        )
        .await?;
        match error {
            None => Ok(()),
            Some(err) => Err(err),
        }
    }
}

fn millis(d: std::time::Duration) -> i32 {
    i32::try_from(d.as_millis()).unwrap_or(i32::MAX)
}
