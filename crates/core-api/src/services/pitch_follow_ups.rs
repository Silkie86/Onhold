//! Pitching, follow-ups: what is due next for each organisation pitched
//! without an answer, from the workspace's follow-up steps and the pitch
//! log, plus organisations given a follow-up date by hand. Worked out when
//! asked; nothing is stored, and nothing is sent without a person sending
//! it.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use core_domain::{Ctx, Permission};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::begin;
use super::finance::today;
use super::organisations::ItemRef;
use super::pitches::PitchChannel;
use super::pitching::{FollowUpStep, ItemKind, PitchingSettings, settings_in};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

/// Runs that ended longer ago than this are left alone.
const LOOK_BACK_DAYS: i64 = 400;
pub const MAX_DAYS_AHEAD: i64 = 60;

/// Why a follow-up is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FollowUpKind {
    /// One of the workspace's follow-up steps.
    Step,
    /// The steps are done and nothing came back: time to log what came of it.
    NoAnswer,
    /// The follow-up date set on the organisation.
    Date,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DueFollowUp {
    pub organisation_id: Uuid,
    pub organisation_name: String,
    pub kind: FollowUpKind,
    /// The day it is due, in the workspace's timezone.
    pub due_on: NaiveDate,
    /// For a step: which, from 1, and how many there are.
    pub step: Option<i32>,
    pub steps: i32,
    /// For a step: how, what doing it logs, and the template to send.
    pub channel: Option<PitchChannel>,
    pub action: Option<String>,
    pub action_label: Option<String>,
    pub template_key: Option<String>,
    /// Who and what the last action was about.
    pub contact_id: Option<Uuid>,
    pub contact_name: Option<String>,
    pub item: Option<ItemRef>,
    /// When the last action was, for a step or no answer.
    pub last_action_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct FollowUpFilter {
    /// Also those due in the next so many days, 0 to 60; default 0.
    pub days_ahead: Option<i64>,
    /// Only this organisation's, and its next one however far off.
    pub organisation_id: Option<Uuid>,
}

/// What is due for one run, if anything is: the next step, or asking what
/// came of it once the steps are done.
fn next<'a>(
    settings: &'a PitchingSettings,
    run: &core_db::pitches::Run,
    tz: chrono_tz::Tz,
) -> Option<(FollowUpKind, NaiveDate, Option<&'a FollowUpStep>, i32)> {
    if run.answered {
        // Their answer waits to be logged ("What did they say?").
        return None;
    }
    let last = run.last_at.with_timezone(&tz).date_naive();
    let done = usize::try_from(run.actions - 1).unwrap_or(0);
    match settings.follow_ups.get(done) {
        Some(step) => Some((
            FollowUpKind::Step,
            last + Duration::days(i64::from(step.after_days)),
            Some(step),
            i32::try_from(done + 1).unwrap_or(i32::MAX),
        )),
        None if settings.give_up_after_days > 0 => Some((
            FollowUpKind::NoAnswer,
            last + Duration::days(i64::from(settings.give_up_after_days)),
            None,
            0,
        )),
        None => None,
    }
}

/// Follow-ups due by today (or a few days ahead), soonest first; or one
/// organisation's next, however far off.
pub async fn due(state: &AppState, ctx: &Ctx, f: &FollowUpFilter) -> ApiResult<Vec<DueFollowUp>> {
    ctx.authorize(Permission::OrganisationRead)?;
    let days_ahead = f.days_ahead.unwrap_or(0);
    if !(0..=MAX_DAYS_AHEAD).contains(&days_ahead) {
        return Err(ApiError::invalid(
            "days_ahead",
            format!("give 0 to {MAX_DAYS_AHEAD} days"),
        ));
    }
    let mut tx = begin(state, ctx).await?;
    if let Some(id) = f.organisation_id
        && core_db::organisations::get(&mut tx, id).await?.is_none()
    {
        return Err(ApiError::NotFound);
    }
    let (settings, _) = settings_in(&mut tx).await?;
    let tz: chrono_tz::Tz = core_db::workspaces::get(&mut tx, ctx.workspace_id)
        .await?
        .and_then(|w| w.default_timezone.parse().ok())
        .unwrap_or(chrono_tz::UTC);
    let until = today(&mut tx, ctx).await? + Duration::days(days_ahead);
    // For one organisation, its next whenever it is.
    let horizon = if f.organisation_id.is_some() {
        NaiveDate::MAX
    } else {
        until
    };
    let runs = core_db::pitches::runs(
        &mut tx,
        f.organisation_id,
        Utc::now() - Duration::days(LOOK_BACK_DAYS),
    )
    .await?;
    let dated = core_db::pitches::dated(&mut tx, f.organisation_id, horizon).await?;
    tx.commit().await?;

    let steps = i32::try_from(settings.follow_ups.len()).unwrap_or(i32::MAX);
    let mut out = Vec::new();
    for run in runs {
        let Some((kind, due_on, step, n)) = next(&settings, &run, tz) else {
            continue;
        };
        if due_on > horizon {
            continue;
        }
        let item = match (run.talent_id, run.project_id, run.item_name) {
            (Some(id), _, Some(name)) => Some(ItemRef {
                kind: ItemKind::Talent,
                id,
                name,
            }),
            (None, Some(id), Some(name)) => Some(ItemRef {
                kind: ItemKind::Project,
                id,
                name,
            }),
            _ => None,
        };
        out.push(DueFollowUp {
            organisation_id: run.organisation_id,
            organisation_name: run.organisation_name,
            kind,
            due_on,
            step: step.map(|_| n),
            steps,
            channel: step.map(|s| s.channel),
            action: step.map(|s| s.action.clone()),
            action_label: step
                .and_then(|s| settings.action(&s.action))
                .map(|a| a.label.clone()),
            template_key: step.and_then(|s| s.template_key.clone()),
            contact_id: run.contact_id,
            contact_name: run.contact_name,
            item,
            last_action_at: Some(run.last_at),
        });
    }
    out.extend(dated.into_iter().map(|d| DueFollowUp {
        organisation_id: d.organisation_id,
        organisation_name: d.organisation_name,
        kind: FollowUpKind::Date,
        due_on: d.follow_up_on,
        step: None,
        steps,
        channel: None,
        action: None,
        action_label: None,
        template_key: None,
        contact_id: None,
        contact_name: None,
        item: None,
        last_action_at: None,
    }));
    out.sort_by(|a, b| {
        a.due_on
            .cmp(&b.due_on)
            .then_with(|| a.organisation_name.cmp(&b.organisation_name))
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(actions: i64, last_at: &str, answered: bool) -> core_db::pitches::Run {
        core_db::pitches::Run {
            organisation_id: Uuid::nil(),
            organisation_name: "Club".into(),
            actions,
            last_at: last_at.parse().unwrap(),
            answered,
            contact_id: None,
            contact_name: None,
            talent_id: None,
            project_id: None,
            item_name: None,
        }
    }

    #[test]
    fn steps_follow_the_last_action_until_an_answer() {
        let s = PitchingSettings::default();
        let tz = chrono_tz::Europe::London;
        let day = |d: &str| d.parse::<NaiveDate>().unwrap();
        // Pitched on the 1st: the email follow-up a week on.
        let (kind, due, step, n) = next(&s, &run(1, "2026-10-01T23:30:00Z", false), tz).unwrap();
        assert_eq!(kind, FollowUpKind::Step);
        // Half past midnight in London is the 2nd.
        assert_eq!(due, day("2026-10-09"));
        assert_eq!(step.unwrap().template_key.as_deref(), Some("follow_up"));
        assert_eq!(n, 1);
        // Followed up: then the call.
        let (_, due, step, n) = next(&s, &run(2, "2026-10-09T10:00:00Z", false), tz).unwrap();
        assert_eq!(due, day("2026-10-16"));
        assert_eq!(step.unwrap().action, "called");
        assert_eq!(n, 2);
        // Both done: two weeks on, ask what came of it.
        let (kind, due, step, _) = next(&s, &run(3, "2026-10-16T10:00:00Z", false), tz).unwrap();
        assert_eq!(kind, FollowUpKind::NoAnswer);
        assert_eq!(due, day("2026-10-30"));
        assert!(step.is_none());
        // An answer stops them.
        assert!(next(&s, &run(1, "2026-10-01T10:00:00Z", true), tz).is_none());
        // With no asking, nothing after the last step.
        let never = PitchingSettings {
            give_up_after_days: 0,
            ..PitchingSettings::default()
        };
        assert!(next(&never, &run(3, "2026-10-16T10:00:00Z", false), tz).is_none());
    }
}
