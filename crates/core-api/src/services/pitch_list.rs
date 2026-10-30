//! Pitching, the list: organisations to pitch something to, with where each
//! stands with it and how good a bet it is. The ranking is plain points
//! from the workspace's own weights, every point shown as a reason, with
//! the strongest reason against as a "but…"; answers rest an organisation
//! for as long as the workspace says. Worked out when asked; nothing is
//! stored.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use core_domain::{Ctx, Permission};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::begin;
use super::organisations::ItemRef;
use super::pitching::{ItemKind, NO_ANSWER, OutcomeTag, PitchingSettings, RankReason, settings_in};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

/// Organisations looked at in one go; a longer list asks to narrow it.
pub const MAX_CANDIDATES: i64 = 5000;
pub const MAX_PAGE: i64 = 200;
/// Dates of the item this far ahead count as nearby.
const NEARBY_DAYS: i64 = 60;

/// Where an organisation stands with what is pitched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PitchStatus {
    /// They have booked it.
    Booked,
    /// Pitched, and the answer is still to come.
    Awaiting,
    /// They asked about it, or said yes.
    Interested,
    /// Pitched, and they said not now, no or never.
    Tried,
    /// Pitched before, and nothing was logged back.
    Pitched,
    /// The workspace thinks it would suit them.
    Match,
    NotPitched,
}

/// How to order the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PitchSort {
    /// Best to pitch first, resting ones last.
    #[default]
    Rank,
    /// A to Z.
    Name,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct PitchListFilter {
    /// What is pitched: `talent` or `project`, with `item_id`.
    pub item_kind: Option<ItemKind>,
    pub item_id: Option<Uuid>,
    /// Words in the name, type or tags.
    pub q: Option<String>,
    pub tag: Option<String>,
    /// A region key from the pitching settings.
    pub region: Option<String>,
    /// An ISO country code.
    pub country: Option<String>,
    pub city: Option<String>,
    /// A status key from the pitching settings.
    pub status: Option<String>,
    /// `client` or `prospect`.
    pub relationship: Option<String>,
    /// Only those standing this way with what is pitched.
    pub pitch_status: Option<PitchStatus>,
    /// Leave out organisations resting after an answer, or waiting for one.
    pub hide_resting: Option<bool>,
    #[param(inline)]
    pub sort: Option<PitchSort>,
    pub offset: Option<i64>,
    /// 1 to 200; default 50.
    pub limit: Option<i64>,
}

/// One reason in the ranking, and what it is worth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct RankLine {
    /// Which reason; none for one of the workspace's own (from a tag).
    pub reason: Option<RankReason>,
    pub label: String,
    pub points: i32,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PitchListRow {
    pub organisation_id: Uuid,
    pub name: String,
    pub city: Option<String>,
    pub country_code: Option<String>,
    /// The region it is in, picked or from its country.
    pub region: Option<String>,
    /// Its status, a key from the pitching settings.
    pub status: Option<String>,
    pub relationship: Option<String>,
    pub follow_up_on: Option<NaiveDate>,
    pub pitch_status: PitchStatus,
    pub score: i32,
    /// What counts for and against it, best first, but for the `but`.
    pub reasons: Vec<RankLine>,
    /// The strongest reason against, if any.
    pub but: Option<RankLine>,
    /// Resting after an answer or waiting for one: shown greyed and last.
    pub resting: bool,
    /// Until when (none: not again, after an answer tagged never; or until
    /// an answer that came back is logged).
    pub resting_until: Option<NaiveDate>,
    pub rest_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PitchList {
    pub items: Vec<PitchListRow>,
    /// How many match in all.
    pub total: i64,
    /// More organisations matched than OnHold looks at in one go; narrow
    /// the list with a filter to see them all.
    pub capped: bool,
    /// What is pitched, when one is picked.
    pub item: Option<ItemRef>,
}

/// What the ranking knows about the day and the item.
struct Scene<'a> {
    settings: &'a PitchingSettings,
    today: NaiveDate,
    tz: chrono_tz::Tz,
    /// The item's name and tags (lower case), when one is picked.
    item: Option<(&'a str, &'a [String])>,
}

impl Scene<'_> {
    fn day(&self, at: DateTime<Utc>) -> NaiveDate {
        at.with_timezone(&self.tz).date_naive()
    }

    /// Days from the last of `actions` unanswered ones (the pitch and
    /// the follow-ups done since) that it waits for an answer: the
    /// follow-ups still to do, then the wait before asking what came of it.
    fn waiting_days(&self, actions: i64) -> i64 {
        let done = usize::try_from(actions - 1).unwrap_or(0);
        let steps: i64 = self
            .settings
            .follow_ups
            .iter()
            .skip(done)
            .map(|s| i64::from(s.after_days))
            .sum();
        steps + i64::from(self.settings.give_up_after_days.max(7))
    }
}

fn short(day: NaiveDate) -> String {
    day.format("%-d %b %Y").to_string()
}

/// Where one organisation stands and how it ranks.
fn rank(scene: &Scene<'_>, c: &core_db::pitch_list::Candidate) -> PitchListRow {
    let s = scene.settings;
    let item = scene.item.map_or("it", |(name, _)| name);
    let mut lines: Vec<RankLine> = Vec::new();
    let mut add = |reason: RankReason, label: String| {
        let points = s.points(reason);
        if points != 0 {
            lines.push(RankLine {
                reason: Some(reason),
                label,
                points,
            });
        }
    };

    let outcome = c
        .outcome_key
        .as_deref()
        .and_then(|k| s.outcome(k))
        .zip(c.outcome_at);
    let outcome_label = |o: &super::pitching::PitchOutcome, at: DateTime<Utc>| {
        format!("Said “{}” on {}", o.label, short(scene.day(at)))
    };

    // Resting after an answer, or waiting for one.
    let mut resting = false;
    let mut resting_until = None;
    let mut rest_reason = None;
    let open = c.actions_since > 0 && c.action_at.is_some();
    let waiting_until = c
        .action_at
        .filter(|_| open && !c.answered_since)
        .map(|at| scene.day(at) + Duration::days(scene.waiting_days(c.actions_since)));
    if let Some((o, at)) = outcome {
        match o.rest() {
            None => {
                resting = true;
                rest_reason = Some(format!(
                    "{}: not pitched {item} again",
                    outcome_label(o, at)
                ));
            }
            Some(days) if days > 0 => {
                let until = scene.day(at) + Duration::days(i64::from(days));
                if until > scene.today && !open {
                    resting = true;
                    resting_until = Some(until);
                    rest_reason = Some(format!(
                        "{}: resting until {}",
                        outcome_label(o, at),
                        short(until)
                    ));
                }
            }
            Some(_) => {}
        }
    }
    if !resting && open && c.answered_since {
        // They answered; what they said is still to be logged.
        resting = true;
        rest_reason = Some("They answered: log what they said".into());
    } else if !resting
        && let (Some(until), Some(at)) = (waiting_until, c.action_at)
        && until > scene.today
    {
        resting = true;
        resting_until = Some(until);
        rest_reason = Some(format!(
            "Pitched on {}: waiting for an answer",
            short(scene.day(at))
        ));
    }

    // What counts for them.
    if c.interested {
        add(RankReason::AskedAbout, format!("Asked about {item}"));
    }
    if c.matched {
        add(RankReason::PotentialMatch, format!("Could suit {item}"));
    }
    if let Some(at) = c.booked_last {
        add(
            RankReason::BookedBefore,
            format!(
                "Booked {item} before ({}): worth going back",
                short(scene.day(at))
            ),
        );
    }
    if c.relationship.as_deref() == Some("client") {
        add(RankReason::Client, "Books from you already".into());
    }
    if let Some((_, tags)) = scene.item {
        let theirs = c.tags.iter().chain(&c.buys_from_others);
        if let Some(t) = theirs
            .map(|t| t.trim().to_lowercase())
            .find(|t| tags.contains(t))
        {
            add(RankReason::SameTaste, format!("Into {t}"));
        }
    }
    if let (Some(at), Some(city)) = (c.nearby_at, c.city.as_deref()) {
        add(
            RankReason::Nearby,
            format!("{item} is in {} on {}", city.trim(), short(scene.day(at))),
        );
    }
    if let Some((o, at)) = outcome {
        match o.tag {
            OutcomeTag::Good => add(RankReason::SaidYes, outcome_label(o, at)),
            OutcomeTag::No if !resting => add(RankReason::SaidNo, outcome_label(o, at)),
            _ => {}
        }
    }
    if let (Some(until), Some(at)) = (waiting_until, c.action_at)
        && until <= scene.today
    {
        add(
            RankReason::NoAnswer,
            format!("No answer to the pitch on {}", short(scene.day(at))),
        );
    } else if !open
        && let Some((o, at)) = outcome
        && o.key == NO_ANSWER
    {
        // Logging that nothing came back counts the same as not logging it.
        add(
            RankReason::NoAnswer,
            format!("No answer to the pitch (logged {})", short(scene.day(at))),
        );
    }
    if !c.reachable {
        add(
            RankReason::NoWayToReach,
            "No email or phone for anyone there".into(),
        );
    }
    for own in &s.own_reasons {
        if own.points != 0 && c.tags.iter().any(|t| t.eq_ignore_ascii_case(&own.tag)) {
            lines.push(RankLine {
                reason: None,
                label: format!("Tagged {}", own.tag),
                points: own.points,
            });
        }
    }

    let score = lines.iter().map(|l| l.points).sum();
    // Best first; the strongest against is the "but", any other with it.
    let mut reasons = lines;
    reasons.sort_by_key(|l| std::cmp::Reverse(l.points));
    let but = reasons
        .last()
        .is_some_and(|l| l.points < 0)
        .then(|| reasons.pop())
        .flatten();

    let pitch_status = if c.booked > 0 {
        PitchStatus::Booked
    } else if open && (c.answered_since || waiting_until.is_some_and(|u| u > scene.today)) {
        PitchStatus::Awaiting
    } else if let Some((o, _)) = outcome.filter(|_| !open) {
        match o.tag {
            OutcomeTag::Good => PitchStatus::Interested,
            _ => PitchStatus::Tried,
        }
    } else if c.interested {
        PitchStatus::Interested
    } else if c.action_at.is_some() {
        PitchStatus::Pitched
    } else if c.matched {
        PitchStatus::Match
    } else {
        PitchStatus::NotPitched
    };

    PitchListRow {
        organisation_id: c.id,
        name: c.name.clone(),
        city: c.city.clone(),
        country_code: c.country_code.clone(),
        region: s
            .region_of(c.country_code.as_deref(), c.region_key.as_deref())
            .map(|r| r.key.clone()),
        status: c.status_key.clone(),
        relationship: c.relationship.clone(),
        follow_up_on: c.follow_up_on,
        pitch_status,
        score,
        reasons,
        but,
        resting,
        resting_until,
        rest_reason,
    }
}

/// The pitching list, for one item or none: filtered, ranked (or A to Z)
/// and a page of it.
pub async fn list(state: &AppState, ctx: &Ctx, f: &PitchListFilter) -> ApiResult<PitchList> {
    ctx.authorize(Permission::OrganisationRead)?;
    let limit = f.limit.unwrap_or(50);
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(ApiError::invalid("limit", format!("give 1 to {MAX_PAGE}")));
    }
    let offset = f.offset.unwrap_or(0).max(0);
    let (talent, project) = match (f.item_kind, f.item_id) {
        // The item's bookings count: where it plays, and who booked it.
        (Some(ItemKind::Talent), Some(id)) => {
            ctx.authorize(Permission::TalentRead)?;
            ctx.authorize(Permission::BookingRead)?;
            (Some(id), None)
        }
        (Some(ItemKind::Project), Some(id)) => {
            ctx.authorize(Permission::ProjectRead)?;
            ctx.authorize(Permission::BookingRead)?;
            (None, Some(id))
        }
        (None, None) => (None, None),
        _ => {
            return Err(ApiError::invalid(
                "item_id",
                "give both what is pitched and which",
            ));
        }
    };
    let blank = |s: &Option<String>| {
        s.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let country = blank(&f.country).map(|c| c.to_uppercase());
    let (q, tag, city, status_key, relationship, region) = (
        blank(&f.q),
        blank(&f.tag),
        blank(&f.city),
        blank(&f.status),
        blank(&f.relationship),
        blank(&f.region),
    );

    let mut tx = begin(state, ctx).await?;
    let (settings, _) = settings_in(&mut tx).await?;
    let found = core_db::pitch_list::item(&mut tx, talent, project).await?;
    if (talent.is_some() || project.is_some()) && found.is_none() {
        return Err(ApiError::NotFound);
    }
    let tz: chrono_tz::Tz = core_db::workspaces::get(&mut tx, ctx.workspace_id)
        .await?
        .and_then(|w| w.default_timezone.parse().ok())
        .unwrap_or(chrono_tz::UTC);
    let region_keys: Vec<String> = settings.regions.iter().map(|r| r.key.clone()).collect();
    let picked_region = region.as_deref().map(|key| {
        (
            key,
            settings
                .region(key)
                .map_or(&[][..], |r| r.countries.as_slice()),
        )
    });
    let candidates = core_db::pitch_list::candidates(
        &mut tx,
        &core_db::pitch_list::Filter {
            talent,
            project,
            q: q.as_deref(),
            tag: tag.as_deref(),
            country: country.as_deref(),
            city: city.as_deref(),
            status_key: status_key.as_deref(),
            relationship: relationship.as_deref(),
            region: picked_region,
            region_keys: &region_keys,
            nearby_until: Utc::now() + Duration::days(NEARBY_DAYS),
            limit: MAX_CANDIDATES,
        },
    )
    .await?;
    tx.commit().await?;

    let capped = candidates.first().is_some_and(|c| c.total > MAX_CANDIDATES);
    let item_tags: Vec<String> = found
        .as_ref()
        .map(|(_, tags)| tags.iter().map(|t| t.trim().to_lowercase()).collect())
        .unwrap_or_default();
    let scene = Scene {
        settings: &settings,
        today: Utc::now().with_timezone(&tz).date_naive(),
        tz,
        item: found
            .as_ref()
            .map(|(name, _)| (name.as_str(), item_tags.as_slice())),
    };
    let mut rows: Vec<PitchListRow> = candidates
        .iter()
        .map(|c| rank(&scene, c))
        .filter(|r| region.is_none() || r.region == region)
        .filter(|r| f.pitch_status.is_none_or(|s| r.pitch_status == s))
        .filter(|r| !(f.hide_resting.unwrap_or(false) && r.resting))
        .collect();
    if f.sort.unwrap_or_default() == PitchSort::Rank {
        // Already A to Z, which settles ties.
        rows.sort_by(|a, b| a.resting.cmp(&b.resting).then(b.score.cmp(&a.score)));
    }
    let total = i64::try_from(rows.len()).unwrap_or(i64::MAX);
    let items = rows
        .into_iter()
        .skip(usize::try_from(offset).unwrap_or(usize::MAX))
        .take(usize::try_from(limit).unwrap_or(0))
        .collect();
    Ok(PitchList {
        items,
        total,
        capped,
        item: match (f.item_kind, f.item_id, found) {
            (Some(kind), Some(id), Some((name, _))) => Some(ItemRef { kind, id, name }),
            _ => None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_db::pitch_list::Candidate;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().expect("a time")
    }

    fn club() -> Candidate {
        Candidate {
            id: Uuid::nil(),
            name: "Club".into(),
            city: Some("Leeds".into()),
            country_code: Some("GB".into()),
            region_key: None,
            relationship: Some("prospect".into()),
            status_key: None,
            follow_up_on: None,
            tags: vec!["Techno".into(), "festival".into()],
            buys_from_others: vec![],
            reachable: true,
            interested: false,
            matched: false,
            booked: 0,
            booked_last: None,
            nearby_at: None,
            outcome_key: None,
            outcome_at: None,
            action_at: None,
            actions_since: 0,
            answered_since: false,
            total: 1,
        }
    }

    fn scene<'a>(s: &'a PitchingSettings, tags: &'a [String]) -> Scene<'a> {
        Scene {
            settings: s,
            today: "2026-10-29".parse().expect("a day"),
            tz: chrono_tz::Europe::London,
            item: Some(("Ana", tags)),
        }
    }

    #[test]
    fn reasons_add_up_and_the_worst_is_the_but() {
        let mut s = PitchingSettings::default();
        s.own_reasons.push(super::super::pitching::OwnReason {
            tag: "festival".into(),
            points: 10,
        });
        let tags = vec!["techno".to_owned()];
        let c = Candidate {
            interested: true,
            reachable: false,
            nearby_at: Some(at("2026-11-14T20:00:00Z")),
            ..club()
        };
        let row = rank(&scene(&s, &tags), &c);
        let labels: Vec<&str> = row.reasons.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Asked about Ana",
                "Ana is in Leeds on 14 Nov 2026",
                "Into techno",
                "Tagged festival"
            ]
        );
        assert_eq!(
            row.but.as_ref().map(|b| b.label.as_str()),
            Some("No email or phone for anyone there")
        );
        assert_eq!(row.score, 40 + 20 + 15 + 10 - 30);
        assert_eq!(row.pitch_status, PitchStatus::Interested);
        assert!(!row.resting);

        // A reason switched off counts for nothing and isn't shown.
        for w in &mut s.ranking {
            if w.reason == RankReason::NoWayToReach {
                w.points = 0;
            }
        }
        let row = rank(&scene(&s, &tags), &c);
        assert!(row.but.is_none());
    }

    #[test]
    fn answers_rest_them_and_pitches_wait_for_an_answer() {
        let s = PitchingSettings::default();
        let tags = vec![];
        // "Not now" a month ago: resting for 90 days.
        let c = Candidate {
            outcome_key: Some("not_now".into()),
            outcome_at: Some(at("2026-09-29T10:00:00Z")),
            action_at: Some(at("2026-09-20T10:00:00Z")),
            ..club()
        };
        let row = rank(&scene(&s, &tags), &c);
        assert!(row.resting);
        assert_eq!(row.resting_until, "2026-12-28".parse().ok());
        assert_eq!(row.pitch_status, PitchStatus::Tried);
        assert_eq!(
            row.rest_reason.as_deref(),
            Some("Said “Not now” on 29 Sep 2026: resting until 28 Dec 2026")
        );
        // "Not a fit": never again.
        let row = rank(
            &scene(&s, &tags),
            &Candidate {
                outcome_key: Some("not_a_fit".into()),
                ..c.clone()
            },
        );
        assert!(row.resting);
        assert!(row.resting_until.is_none());
        // "No" long ago, rested: it counts against them.
        let row = rank(
            &scene(&s, &tags),
            &Candidate {
                outcome_key: Some("no".into()),
                outcome_at: Some(at("2025-01-10T10:00:00Z")),
                ..c
            },
        );
        assert!(!row.resting);
        assert_eq!(row.but.map(|b| b.reason), Some(Some(RankReason::SaidNo)));
        // Pitched last week: waiting for an answer.
        let pitched = Candidate {
            action_at: Some(at("2026-10-22T10:00:00Z")),
            actions_since: 1,
            ..club()
        };
        let row = rank(&scene(&s, &tags), &pitched);
        assert!(row.resting);
        assert_eq!(row.pitch_status, PitchStatus::Awaiting);
        // Two months on, nothing back: no longer waiting, and it counts
        // against them.
        let row = rank(
            &scene(&s, &tags),
            &Candidate {
                action_at: Some(at("2026-08-20T10:00:00Z")),
                ..pitched
            },
        );
        assert!(!row.resting);
        assert_eq!(row.pitch_status, PitchStatus::Pitched);
        assert_eq!(row.but.map(|b| b.reason), Some(Some(RankReason::NoAnswer)));
    }

    #[test]
    fn follow_ups_done_shorten_the_wait_and_answers_wait_to_be_logged() {
        let s = PitchingSettings::default();
        let tags = vec![];
        let steps: i64 = s.follow_ups.iter().map(|f| i64::from(f.after_days)).sum();
        let give_up = i64::from(s.give_up_after_days.max(7));
        let sc = scene(&s, &tags);
        assert_eq!(sc.waiting_days(1), steps + give_up);
        let last = i64::try_from(s.follow_ups.len()).expect("a few");
        assert_eq!(sc.waiting_days(last + 1), give_up);
        assert_eq!(sc.waiting_days(last + 5), give_up);

        // An answer came back: waiting for it to be logged, not pitched again.
        let row = rank(
            &sc,
            &Candidate {
                action_at: Some(at("2026-08-20T10:00:00Z")),
                actions_since: 1,
                answered_since: true,
                ..club()
            },
        );
        assert!(row.resting);
        assert_eq!(row.pitch_status, PitchStatus::Awaiting);
        assert!(
            row.reasons
                .iter()
                .all(|r| r.reason != Some(RankReason::NoAnswer))
        );
    }

    #[test]
    fn every_reason_against_is_shown_and_a_logged_no_answer_still_counts() {
        let s = PitchingSettings::default();
        let tags = vec![];
        let row = rank(
            &scene(&s, &tags),
            &Candidate {
                interested: true,
                reachable: false,
                outcome_key: Some(NO_ANSWER.into()),
                outcome_at: Some(at("2026-01-10T10:00:00Z")),
                action_at: Some(at("2025-12-01T10:00:00Z")),
                ..club()
            },
        );
        assert!(!row.resting);
        let shown: i32 = row.reasons.iter().chain(&row.but).map(|l| l.points).sum();
        assert_eq!(shown, row.score);
        assert_eq!(
            row.but.map(|b| b.reason),
            Some(Some(RankReason::NoWayToReach))
        );
        assert!(
            row.reasons
                .iter()
                .any(|r| r.reason == Some(RankReason::NoAnswer))
        );

        // Booked for a date still to come: booked, but not "before".
        let row = rank(
            &scene(&s, &tags),
            &Candidate {
                booked: 1,
                booked_last: None,
                ..club()
            },
        );
        assert_eq!(row.pitch_status, PitchStatus::Booked);
        assert!(row.reasons.is_empty());
    }
}
