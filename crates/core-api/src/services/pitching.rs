//! Pitching: working through a long list of people to sell to, knowing who
//! to approach next, why, and when to follow up. A module the setup form
//! switches on, so a one-person setup never sees it unless wanted.
//!
//! This part holds the workspace's own settings for it: how it splits the
//! world into regions, the statuses (with colours) an organisation can
//! have, and what it pitches. Each is a setting, not code, so every kind of
//! business shapes it to how it works.

use chrono::{DateTime, Utc};
use core_db::Tx;
use core_domain::{Ctx, Permission, UserId};
use core_engines::countries;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;

use super::crm::Check;
use super::pitches::PitchChannel;
use super::{Change, begin, check_version, record};
use crate::AppState;
use crate::error::ApiResult;

/// The module's key in a workspace's enabled modules.
pub const MODULE: &str = "pitching";

pub const MAX_REGIONS: usize = 30;
pub const MAX_STATUSES: usize = 12;
pub const MAX_OUTCOMES: usize = 20;
pub const MAX_ACTIONS: usize = 20;
pub const MAX_TEMPLATES: usize = 30;
pub const MAX_TEMPLATE_SUBJECT: usize = 300;
pub const MAX_TEMPLATE_BODY: usize = 5000;
pub const MAX_FOLLOW_UPS: usize = 10;
pub const MAX_FOLLOW_UP_DAYS: i32 = 365;
pub const MAX_REST_DAYS: i32 = 3650;
pub const MAX_OWN_REASONS: usize = 30;
pub const MAX_POINTS: i32 = 100;

/// A part of the world, by the countries in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Region {
    /// Lower case letters, digits and underscores: `europe`.
    pub key: String,
    pub name: String,
    /// ISO country codes, upper case. Each country is in one region at most.
    pub countries: Vec<String>,
}

/// What a status says about where things stand, so OnHold can move it by
/// itself: an enquiry sets a `talking` one, a confirmed booking a `booked`
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StatusMeaning {
    Booked,
    Talking,
    Inactive,
    Other,
}

/// The colours a status dot can take, drawn from the theme so they read in
/// light and dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Colour {
    Green,
    Amber,
    Red,
    Grey,
    Blue,
    Purple,
}

impl Colour {
    pub fn as_str(self) -> &'static str {
        match self {
            Colour::Green => "green",
            Colour::Amber => "amber",
            Colour::Red => "red",
            Colour::Grey => "grey",
            Colour::Blue => "blue",
            Colour::Purple => "purple",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Status {
    pub key: String,
    pub label: String,
    pub colour: Colour,
    pub meaning: StatusMeaning,
}

/// What a workspace pitches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// Its talent (artists, acts, rooms, kit: whatever it calls them).
    Talent,
    /// Its tours and other projects.
    Project,
}

impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemKind::Talent => "talent",
            ItemKind::Project => "project",
        }
    }
}

/// What an answer means for pitching them again: good news, wait a while,
/// no for now, or never.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeTag {
    Good,
    Wait,
    No,
    Never,
}

/// An answer to a pitch: "Interested", "Not now", "No answer"…
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PitchOutcome {
    pub key: String,
    pub label: String,
    pub tag: OutcomeTag,
    /// Days to leave them be after this answer before pitching the same
    /// thing again, 0 for no wait; left out, the tag's usual wait. An answer
    /// tagged never is never followed by another pitch of the same thing.
    #[serde(default)]
    pub rest_days: Option<i32>,
}

/// The answer logged when nothing came back.
pub const NO_ANSWER: &str = "no_answer";

impl PitchOutcome {
    /// Days to rest after this answer; `None` for never again.
    pub fn rest(&self) -> Option<i32> {
        match self.tag {
            OutcomeTag::Never => None,
            OutcomeTag::Good => Some(self.rest_days.unwrap_or(0)),
            // Saved before rests existed, "No answer" waits as it does now.
            OutcomeTag::Wait if self.key == NO_ANSWER => Some(self.rest_days.unwrap_or(60)),
            OutcomeTag::Wait => Some(self.rest_days.unwrap_or(90)),
            OutcomeTag::No => Some(self.rest_days.unwrap_or(180)),
        }
    }
}

/// A reason one organisation is better to pitch than another, worth so
/// many points (less than 0 counts against).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RankReason {
    /// The item is on their "interested in" list.
    AskedAbout,
    /// The item is on their "potential matches" list.
    PotentialMatch,
    /// They booked the item before.
    BookedBefore,
    /// They are a client: they have booked through the workspace.
    Client,
    /// One of the item's tags is among theirs, or the acts they book.
    SameTaste,
    /// The item has a date in their city soon.
    Nearby,
    /// Their last answer was a good one.
    SaidYes,
    /// The last pitch went unanswered.
    NoAnswer,
    /// Their last answer was a no, now rested.
    SaidNo,
    /// Nobody there has an email address or a phone number.
    NoWayToReach,
}

impl RankReason {
    pub const ALL: [RankReason; 10] = [
        RankReason::AskedAbout,
        RankReason::PotentialMatch,
        RankReason::BookedBefore,
        RankReason::Client,
        RankReason::SameTaste,
        RankReason::Nearby,
        RankReason::SaidYes,
        RankReason::NoAnswer,
        RankReason::SaidNo,
        RankReason::NoWayToReach,
    ];

    pub fn default_points(self) -> i32 {
        match self {
            RankReason::AskedAbout => 40,
            RankReason::PotentialMatch => 25,
            RankReason::BookedBefore => 30,
            RankReason::Client => 10,
            RankReason::SameTaste => 15,
            RankReason::Nearby => 20,
            RankReason::SaidYes => 20,
            RankReason::NoAnswer => -10,
            RankReason::SaidNo => -20,
            RankReason::NoWayToReach => -30,
        }
    }
}

/// What a reason is worth; 0 switches it off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RankWeight {
    pub reason: RankReason,
    /// -100 to 100.
    pub points: i32,
}

/// A reason of the workspace's own: organisations with this tag are worth
/// so many points more (or fewer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OwnReason {
    pub tag: String,
    /// -100 to 100.
    pub points: i32,
}

/// Something done to win them over: pitched, followed up, called…
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PitchAction {
    pub key: String,
    pub label: String,
}

/// Wording to pitch with. `{firstName}`, `{item}`, `{link}`, `{yourName}`
/// and the rest of [`super::pitch_sending::PLACEHOLDERS`] are filled in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PitchTemplate {
    pub key: String,
    pub name: String,
    /// What sending it logs, a key from the workspace's actions.
    pub action: String,
    pub subject: String,
    pub body: String,
}

/// One follow-up after a pitch nobody has answered: so many days after the
/// last thing done, done this way. With a template it is a message to send;
/// without, a reminder (to ring them, say).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FollowUpStep {
    /// Days after the last action, 1 to 365.
    pub after_days: i32,
    pub channel: PitchChannel,
    /// What doing it logs, a key from the workspace's actions.
    pub action: String,
    /// The pitch template to send, if it is a message.
    pub template_key: Option<String>,
}

/// How a pitch is usually sent: from OnHold through the member's mailbox
/// (so it and the answers are logged by themselves), by opening it in
/// their own email app, or by copying it to paste anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PitchSendWith {
    Onhold,
    MailApp,
    Copy,
}

impl PitchSendWith {
    pub fn as_str(self) -> &'static str {
        match self {
            PitchSendWith::Onhold => "onhold",
            PitchSendWith::MailApp => "mail_app",
            PitchSendWith::Copy => "copy",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(Value::String(s.to_owned())).ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PitchingSettings {
    #[serde(default = "default_regions")]
    pub regions: Vec<Region>,
    #[serde(default = "default_statuses")]
    pub statuses: Vec<Status>,
    #[serde(default = "default_items")]
    pub items: Vec<ItemKind>,
    /// The answers a pitch can get.
    #[serde(default = "default_outcomes")]
    pub outcomes: Vec<PitchOutcome>,
    /// The kinds of thing logged as done.
    #[serde(default = "default_actions")]
    pub actions: Vec<PitchAction>,
    /// The wording members pitch with.
    #[serde(default = "default_templates")]
    pub templates: Vec<PitchTemplate>,
    /// How members send a pitch unless they pick otherwise.
    #[serde(default = "default_send_with")]
    pub send_with: PitchSendWith,
    /// The follow-ups after a pitch with no answer, in order. An answer
    /// stops them.
    #[serde(default = "default_follow_ups")]
    pub follow_ups: Vec<FollowUpStep>,
    /// Days after the last follow-up before asking to log what came of it;
    /// 0 never asks.
    #[serde(default = "default_give_up_days")]
    pub give_up_after_days: i32,
    /// What each reason in the "best to pitch first" ranking is worth.
    #[serde(default = "default_ranking")]
    pub ranking: Vec<RankWeight>,
    /// The workspace's own reasons, from tags.
    #[serde(default)]
    pub own_reasons: Vec<OwnReason>,
}

/// New settings. A list left out keeps what is stored, so a client from
/// before a list existed does not reset it.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct PitchingSettingsInput {
    pub regions: Option<Vec<Region>>,
    pub statuses: Option<Vec<Status>>,
    pub items: Option<Vec<ItemKind>>,
    pub outcomes: Option<Vec<PitchOutcome>>,
    pub actions: Option<Vec<PitchAction>>,
    pub templates: Option<Vec<PitchTemplate>>,
    pub send_with: Option<PitchSendWith>,
    pub follow_ups: Option<Vec<FollowUpStep>>,
    pub give_up_after_days: Option<i32>,
    pub ranking: Option<Vec<RankWeight>>,
    pub own_reasons: Option<Vec<OwnReason>>,
}

impl PitchingSettingsInput {
    fn over(self, stored: PitchingSettings) -> PitchingSettings {
        let actions = self.actions.unwrap_or(stored.actions);
        // Stored templates kept go with any action no longer listed.
        let templates = self.templates.unwrap_or_else(|| {
            let mut kept = stored.templates;
            kept.retain(|t| actions.iter().any(|a| a.key == t.action));
            kept
        });
        // And stored follow-ups with any action or template no longer there.
        let follow_ups = self.follow_ups.unwrap_or_else(|| {
            let mut kept = stored.follow_ups;
            settle_follow_ups(&mut kept, &actions, &templates);
            kept
        });
        PitchingSettings {
            regions: self.regions.unwrap_or(stored.regions),
            statuses: self.statuses.unwrap_or(stored.statuses),
            items: self.items.unwrap_or(stored.items),
            outcomes: self.outcomes.unwrap_or(stored.outcomes),
            actions,
            templates,
            send_with: self.send_with.unwrap_or(stored.send_with),
            follow_ups,
            give_up_after_days: self.give_up_after_days.unwrap_or(stored.give_up_after_days),
            ranking: self.ranking.unwrap_or(stored.ranking),
            own_reasons: self.own_reasons.unwrap_or(stored.own_reasons),
        }
    }
}

impl Default for PitchingSettings {
    fn default() -> Self {
        Self {
            regions: default_regions(),
            statuses: default_statuses(),
            items: default_items(),
            outcomes: default_outcomes(),
            actions: default_actions(),
            templates: default_templates(),
            send_with: default_send_with(),
            follow_ups: default_follow_ups(),
            give_up_after_days: default_give_up_days(),
            ranking: default_ranking(),
            own_reasons: Vec::new(),
        }
    }
}

/// OnHold's world split: Europe (incl. UK), North & Central America, South
/// America, Oceania, Asia and Africa, every country in one.
fn default_regions() -> Vec<Region> {
    countries::DEFAULT_REGIONS
        .iter()
        .map(|(key, name)| Region {
            key: (*key).into(),
            name: (*name).into(),
            countries: countries::all()
                .iter()
                .filter(|c| c.region == *key)
                .map(|c| c.code.into())
                .collect(),
        })
        .collect()
}

fn default_statuses() -> Vec<Status> {
    let s = |key: &str, label: &str, colour, meaning| Status {
        key: key.into(),
        label: label.into(),
        colour,
        meaning,
    };
    vec![
        s(
            "booked",
            "Booked or worked with",
            Colour::Green,
            StatusMeaning::Booked,
        ),
        s(
            "talking",
            "Talking or interested",
            Colour::Amber,
            StatusMeaning::Talking,
        ),
        s(
            "inactive",
            "Inactive",
            Colour::Grey,
            StatusMeaning::Inactive,
        ),
    ]
}

fn default_items() -> Vec<ItemKind> {
    vec![ItemKind::Talent]
}

fn default_outcomes() -> Vec<PitchOutcome> {
    let o = |key: &str, label: &str, tag, rest_days| PitchOutcome {
        key: key.into(),
        label: label.into(),
        tag,
        rest_days,
    };
    vec![
        o("interested", "Interested or yes", OutcomeTag::Good, Some(0)),
        o("not_now", "Not now", OutcomeTag::Wait, Some(90)),
        o("no", "No", OutcomeTag::No, Some(180)),
        o("not_a_fit", "Not a fit", OutcomeTag::Never, None),
        o(NO_ANSWER, "No answer", OutcomeTag::Wait, Some(60)),
    ]
}

fn default_actions() -> Vec<PitchAction> {
    let a = |key: &str, label: &str| PitchAction {
        key: key.into(),
        label: label.into(),
    };
    vec![
        a("pitched", "Pitched"),
        a("followed_up", "Followed up"),
        a("called", "Called"),
        a("met", "Met"),
        a("messaged", "Messaged on social"),
    ]
}

fn default_templates() -> Vec<PitchTemplate> {
    vec![
        PitchTemplate {
            key: "first_pitch".into(),
            name: "First pitch".into(),
            action: "pitched".into(),
            subject: "{item} for {organisation}".into(),
            body: "Hi {firstName},\n\nI look after {item} and think they would be a great fit for \
                   {organisation}. You can see more here: {link}\n\nWould you be open to talking \
                   about dates?\n\nBest wishes,\n{yourName}"
                .into(),
        },
        PitchTemplate {
            key: "follow_up".into(),
            name: "Follow-up".into(),
            action: "followed_up".into(),
            subject: "Following up: {item} for {organisation}".into(),
            body: "Hi {firstName},\n\nJust following up on my note about {item}. Is there a good \
                   time to talk?\n\nBest wishes,\n{yourName}"
                .into(),
        },
    ]
}

fn default_send_with() -> PitchSendWith {
    PitchSendWith::Onhold
}

/// An email a week on, then a call a week after that.
fn default_follow_ups() -> Vec<FollowUpStep> {
    vec![
        FollowUpStep {
            after_days: 7,
            channel: PitchChannel::Email,
            action: "followed_up".into(),
            template_key: Some("follow_up".into()),
        },
        FollowUpStep {
            after_days: 7,
            channel: PitchChannel::Phone,
            action: "called".into(),
            template_key: None,
        },
    ]
}

fn default_give_up_days() -> i32 {
    14
}

fn default_ranking() -> Vec<RankWeight> {
    RankReason::ALL
        .iter()
        .map(|&reason| RankWeight {
            reason,
            points: reason.default_points(),
        })
        .collect()
}

/// One weight per reason, in OnHold's order: any missing (a reason added
/// in a later release) at its usual points, and any repeat dropped.
fn settle_ranking(weights: &mut Vec<RankWeight>) {
    *weights = RankReason::ALL
        .iter()
        .map(|&reason| {
            weights
                .iter()
                .find(|w| w.reason == reason)
                .copied()
                .unwrap_or(RankWeight {
                    reason,
                    points: reason.default_points(),
                })
        })
        .collect();
}

/// Drops follow-ups logging an action no longer listed, and lets one whose
/// template is gone become a reminder.
fn settle_follow_ups(
    steps: &mut Vec<FollowUpStep>,
    actions: &[PitchAction],
    templates: &[PitchTemplate],
) {
    steps.retain(|s| actions.iter().any(|a| a.key == s.action));
    for s in steps {
        if s.template_key
            .as_ref()
            .is_some_and(|k| !templates.iter().any(|t| &t.key == k))
        {
            s.template_key = None;
        }
    }
}

impl PitchingSettings {
    /// The region an organisation is in: the one picked for it if it still
    /// exists, otherwise its country's.
    pub fn region_of(&self, country: Option<&str>, picked: Option<&str>) -> Option<&Region> {
        if let Some(r) = picked.and_then(|k| self.regions.iter().find(|r| r.key == k)) {
            return Some(r);
        }
        let country = country?;
        self.regions
            .iter()
            .find(|r| r.countries.iter().any(|c| c == country))
    }

    pub fn status(&self, key: &str) -> Option<&Status> {
        self.statuses.iter().find(|s| s.key == key)
    }

    /// The first status that means this, if the workspace has one.
    pub fn first_meaning(&self, meaning: StatusMeaning) -> Option<&Status> {
        self.statuses.iter().find(|s| s.meaning == meaning)
    }

    pub fn region(&self, key: &str) -> Option<&Region> {
        self.regions.iter().find(|r| r.key == key)
    }

    pub fn outcome(&self, key: &str) -> Option<&PitchOutcome> {
        self.outcomes.iter().find(|o| o.key == key)
    }

    pub fn action(&self, key: &str) -> Option<&PitchAction> {
        self.actions.iter().find(|a| a.key == key)
    }

    pub fn template(&self, key: &str) -> Option<&PitchTemplate> {
        self.templates.iter().find(|t| t.key == key)
    }

    /// What a ranking reason is worth.
    pub fn points(&self, reason: RankReason) -> i32 {
        self.ranking
            .iter()
            .find(|w| w.reason == reason)
            .map_or_else(|| reason.default_points(), |w| w.points)
    }
}

fn is_key(k: &str) -> bool {
    (1..=40).contains(&k.len())
        && k.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A key, trimmed, noting a problem when it is malformed or `taken`.
fn checked_key(check: &mut Check, field: &str, key: &str, taken: impl Fn(&str) -> bool) -> String {
    let key = key.trim().to_owned();
    if !is_key(&key) {
        check.issue(
            field,
            "use 1 to 40 lower case letters, digits or underscores",
        );
    } else if taken(&key) {
        check.issue(field, "is used twice");
    }
    key
}

/// Checks and tidies settings: keys unique and well formed, names given,
/// countries known and each in one region at most.
fn check_settings(input: PitchingSettings) -> ApiResult<PitchingSettings> {
    let mut check = Check::default();
    if input.regions.is_empty() || input.regions.len() > MAX_REGIONS {
        check.issue("regions", format!("give 1 to {MAX_REGIONS} regions"));
    }
    let mut regions: Vec<Region> = Vec::new();
    let mut seen_countries: Vec<String> = Vec::new();
    for (i, r) in input.regions.iter().enumerate() {
        let key = checked_key(&mut check, &format!("regions[{i}].key"), &r.key, |k| {
            regions.iter().any(|x| x.key == k)
        });
        let name = check.required(&format!("regions[{i}].name"), &r.name, 60);
        let mut codes = Vec::new();
        for c in &r.countries {
            match countries::by_code(c) {
                None => check.issue(
                    format!("regions[{i}].countries"),
                    format!("`{c}` is not a country code"),
                ),
                Some(country) => {
                    let code = country.code.to_owned();
                    if seen_countries.contains(&code) {
                        check.issue(
                            format!("regions[{i}].countries"),
                            format!("{} is listed twice", country.name),
                        );
                    } else {
                        seen_countries.push(code.clone());
                        codes.push(code);
                    }
                }
            }
        }
        regions.push(Region {
            key,
            name,
            countries: codes,
        });
    }
    if input.statuses.len() > MAX_STATUSES {
        check.issue("statuses", format!("give at most {MAX_STATUSES} statuses"));
    }
    let mut statuses: Vec<Status> = Vec::new();
    for (i, s) in input.statuses.iter().enumerate() {
        let key = checked_key(&mut check, &format!("statuses[{i}].key"), &s.key, |k| {
            statuses.iter().any(|x| x.key == k)
        });
        let label = check.required(&format!("statuses[{i}].label"), &s.label, 60);
        statuses.push(Status {
            key,
            label,
            colour: s.colour,
            meaning: s.meaning,
        });
    }
    let mut items: Vec<ItemKind> = Vec::new();
    for k in &input.items {
        if !items.contains(k) {
            items.push(*k);
        }
    }
    if items.is_empty() {
        check.issue("items", "choose at least one thing to pitch");
    }
    if input.outcomes.is_empty() || input.outcomes.len() > MAX_OUTCOMES {
        check.issue("outcomes", format!("give 1 to {MAX_OUTCOMES} answers"));
    }
    let mut outcomes: Vec<PitchOutcome> = Vec::new();
    for (i, o) in input.outcomes.iter().enumerate() {
        let key = checked_key(&mut check, &format!("outcomes[{i}].key"), &o.key, |k| {
            outcomes.iter().any(|x| x.key == k)
        });
        let label = check.required(&format!("outcomes[{i}].label"), &o.label, 60);
        if o.rest_days
            .is_some_and(|d| !(0..=MAX_REST_DAYS).contains(&d))
        {
            check.issue(
                format!("outcomes[{i}].rest_days"),
                format!("give 0 to {MAX_REST_DAYS} days"),
            );
        }
        outcomes.push(PitchOutcome {
            key,
            label,
            tag: o.tag,
            rest_days: o.rest_days,
        });
    }
    if input.actions.is_empty() || input.actions.len() > MAX_ACTIONS {
        check.issue(
            "actions",
            format!("give 1 to {MAX_ACTIONS} kinds of action"),
        );
    }
    let mut actions: Vec<PitchAction> = Vec::new();
    for (i, a) in input.actions.iter().enumerate() {
        let key = checked_key(&mut check, &format!("actions[{i}].key"), &a.key, |k| {
            actions.iter().any(|x| x.key == k)
        });
        let label = check.required(&format!("actions[{i}].label"), &a.label, 60);
        actions.push(PitchAction { key, label });
    }
    if input.templates.len() > MAX_TEMPLATES {
        check.issue(
            "templates",
            format!("give at most {MAX_TEMPLATES} templates"),
        );
    }
    let mut templates: Vec<PitchTemplate> = Vec::new();
    for (i, t) in input.templates.iter().enumerate() {
        let key = checked_key(&mut check, &format!("templates[{i}].key"), &t.key, |k| {
            templates.iter().any(|x| x.key == k)
        });
        let name = check.required(&format!("templates[{i}].name"), &t.name, 80);
        let action = t.action.trim().to_owned();
        if !actions.iter().any(|a| a.key == action) {
            check.issue(
                format!("templates[{i}].action"),
                "is not one of the workspace's actions",
            );
        }
        let subject = check.required(
            &format!("templates[{i}].subject"),
            &t.subject,
            MAX_TEMPLATE_SUBJECT,
        );
        let body = check
            .long(
                &format!("templates[{i}].body"),
                Some(&t.body),
                MAX_TEMPLATE_BODY,
            )
            .unwrap_or_default();
        if body.is_empty() {
            check.issue(format!("templates[{i}].body"), "write what it says");
        }
        templates.push(PitchTemplate {
            key,
            name,
            action,
            subject,
            body,
        });
    }
    if input.follow_ups.len() > MAX_FOLLOW_UPS {
        check.issue(
            "follow_ups",
            format!("give at most {MAX_FOLLOW_UPS} follow-ups"),
        );
    }
    let mut follow_ups: Vec<FollowUpStep> = Vec::new();
    for (i, f) in input.follow_ups.iter().enumerate() {
        if !(1..=MAX_FOLLOW_UP_DAYS).contains(&f.after_days) {
            check.issue(
                format!("follow_ups[{i}].after_days"),
                format!("give 1 to {MAX_FOLLOW_UP_DAYS} days"),
            );
        }
        let action = f.action.trim().to_owned();
        if !actions.iter().any(|a| a.key == action) {
            check.issue(
                format!("follow_ups[{i}].action"),
                "is not one of the workspace's actions",
            );
        }
        let template_key = f
            .template_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(str::to_owned);
        if template_key
            .as_ref()
            .is_some_and(|k| !templates.iter().any(|t| &t.key == k))
        {
            check.issue(
                format!("follow_ups[{i}].template_key"),
                "is not one of the workspace's pitch templates",
            );
        }
        follow_ups.push(FollowUpStep {
            after_days: f.after_days,
            channel: f.channel,
            action,
            template_key,
        });
    }
    if !(0..=MAX_FOLLOW_UP_DAYS).contains(&input.give_up_after_days) {
        check.issue(
            "give_up_after_days",
            format!("give 0 to {MAX_FOLLOW_UP_DAYS} days"),
        );
    }
    let mut ranking = input.ranking.clone();
    for (i, w) in ranking.iter().enumerate() {
        if !(-MAX_POINTS..=MAX_POINTS).contains(&w.points) {
            check.issue(
                format!("ranking[{i}].points"),
                format!("give -{MAX_POINTS} to {MAX_POINTS} points"),
            );
        }
    }
    settle_ranking(&mut ranking);
    if input.own_reasons.len() > MAX_OWN_REASONS {
        check.issue(
            "own_reasons",
            format!("give at most {MAX_OWN_REASONS} reasons of your own"),
        );
    }
    let mut own_reasons: Vec<OwnReason> = Vec::new();
    for (i, r) in input.own_reasons.iter().enumerate() {
        let tag = check.required(&format!("own_reasons[{i}].tag"), &r.tag, 60);
        if own_reasons.iter().any(|x| x.tag.eq_ignore_ascii_case(&tag)) {
            check.issue(format!("own_reasons[{i}].tag"), "is used twice");
        }
        if !(-MAX_POINTS..=MAX_POINTS).contains(&r.points) {
            check.issue(
                format!("own_reasons[{i}].points"),
                format!("give -{MAX_POINTS} to {MAX_POINTS} points"),
            );
        }
        own_reasons.push(OwnReason {
            tag,
            points: r.points,
        });
    }
    check.finish()?;
    Ok(PitchingSettings {
        regions,
        statuses,
        items,
        outcomes,
        actions,
        templates,
        send_with: input.send_with,
        follow_ups,
        give_up_after_days: input.give_up_after_days,
        ranking,
        own_reasons,
    })
}

/// The workspace's settings, or OnHold's defaults where it has none. A
/// stored value that no longer reads (from a later release, say) falls
/// back to the defaults rather than failing every page that needs it.
pub(crate) async fn settings_in(tx: &mut Tx) -> ApiResult<(PitchingSettings, i64)> {
    Ok(match core_db::pitching::settings(tx).await? {
        Some(row) => {
            let mut settings: PitchingSettings =
                serde_json::from_value(row.settings).unwrap_or_default();
            // Settings saved before templates existed get OnHold's, which
            // log actions the workspace may have removed; keep only those
            // that log one it lists.
            let actions = &settings.actions;
            settings
                .templates
                .retain(|t| actions.iter().any(|a| a.key == t.action));
            settle_follow_ups(
                &mut settings.follow_ups,
                &settings.actions,
                &settings.templates,
            );
            settle_ranking(&mut settings.ranking);
            (settings, row.version)
        }
        None => (PitchingSettings::default(), 0),
    })
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CountryView {
    pub code: String,
    pub name: String,
    /// The region OnHold's own split puts it in.
    pub default_region: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PitchingSettingsView {
    #[serde(flatten)]
    pub settings: PitchingSettings,
    /// 0 until the workspace first saves its own.
    pub version: i64,
    pub updated_at: Option<DateTime<Utc>>,
    pub updated_by: Option<UserId>,
    /// Every country, for choosing which region each is in.
    pub countries: Vec<CountryView>,
    /// What a pitch template can say, filled in for each pitch.
    pub placeholders: Vec<Placeholder>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Placeholder {
    /// Written in a template as `{name}`.
    pub name: String,
    pub means: String,
}

fn view(
    settings: PitchingSettings,
    version: i64,
    row: Option<core_db::pitching::Row>,
) -> PitchingSettingsView {
    PitchingSettingsView {
        settings,
        version,
        updated_at: row.as_ref().map(|r| r.updated_at),
        updated_by: row.and_then(|r| r.updated_by),
        countries: countries::all()
            .iter()
            .map(|c| CountryView {
                code: c.code.into(),
                name: c.name.into(),
                default_region: c.region.into(),
            })
            .collect(),
        placeholders: super::pitch_sending::PLACEHOLDERS
            .iter()
            .map(|(name, means)| Placeholder {
                name: (*name).into(),
                means: (*means).into(),
            })
            .collect(),
    }
}

pub async fn get(state: &AppState, ctx: &Ctx) -> ApiResult<PitchingSettingsView> {
    ctx.authorize(Permission::OrganisationRead)?;
    let mut tx = begin(state, ctx).await?;
    let row = core_db::pitching::settings(&mut tx).await?;
    let (settings, version) = settings_in(&mut tx).await?;
    tx.commit().await?;
    Ok(view(settings, version, row))
}

/// Replaces the settings. Organisations keep a region or status that is no
/// longer listed, but show as not set until it is listed again.
pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    input: PitchingSettingsInput,
    if_match: Option<i64>,
) -> ApiResult<PitchingSettingsView> {
    ctx.authorize(Permission::ConfigWrite)?;
    let mut tx = begin(state, ctx).await?;
    core_db::pitching::lock(&mut tx).await?;
    let (before, version) = settings_in(&mut tx).await?;
    check_version(version, if_match)?;
    let checked = check_settings(input.over(before.clone()))?;
    let value = serde_json::to_value(&checked).map_err(crate::error::ApiError::internal)?;
    core_db::pitching::save(&mut tx, &value, ctx.user_id()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "pitching.settings_updated",
            resource: "pitching_settings",
            object_id: None,
            before: Some(json!(before)),
            after: Some(value),
            event_extra: None,
        },
    )
    .await?;
    let row = core_db::pitching::settings(&mut tx).await?;
    let (settings, version) = settings_in(&mut tx).await?;
    tx.commit().await?;
    Ok(view(settings, version, row))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_put_every_country_in_one_region() {
        let s = PitchingSettings::default();
        assert_eq!(s.regions.len(), 6);
        let total: usize = s.regions.iter().map(|r| r.countries.len()).sum();
        assert_eq!(total, countries::all().len());
        assert_eq!(s.region_of(Some("GB"), None).expect("known").key, "europe");
        assert_eq!(s.region_of(Some("AU"), None).expect("known").key, "oceania");
        // A picked region wins; one that no longer exists is ignored.
        assert_eq!(
            s.region_of(Some("GB"), Some("asia")).expect("known").key,
            "asia"
        );
        assert_eq!(
            s.region_of(Some("GB"), Some("gone")).expect("known").key,
            "europe"
        );
        assert!(s.region_of(None, None).is_none());
        assert!(check_settings(s).is_ok());
    }

    #[test]
    fn settings_are_checked_and_tidied() {
        let mut s = PitchingSettings::default();
        s.regions[0].countries.push("gb".into());
        s.regions.push(Region {
            key: "Bad Key".into(),
            name: " ".into(),
            countries: vec!["ZZ".into()],
        });
        s.statuses.push(s.statuses[0].clone());
        s.items.clear();
        s.outcomes.push(s.outcomes[0].clone());
        s.actions = vec![];
        let Err(crate::error::ApiError::Invalid(issues)) = check_settings(s) else {
            panic!("expected problems");
        };
        let fields: Vec<&str> = issues.iter().map(|i| i.field.as_str()).collect();
        for want in [
            "regions[0].countries",
            "regions[6].key",
            "regions[6].name",
            "regions[6].countries",
            "statuses[3].key",
            "items",
            "outcomes[5].key",
            "actions",
        ] {
            assert!(fields.contains(&want), "{want} missing from {fields:?}");
        }

        let mut s = PitchingSettings::default();
        s.regions[0].countries = vec!["gb".into(), "ie".into()];
        s.items = vec![ItemKind::Project, ItemKind::Project, ItemKind::Talent];
        let tidy = check_settings(s).expect("known");
        assert_eq!(tidy.regions[0].countries, ["GB", "IE"]);
        assert_eq!(tidy.items, [ItemKind::Project, ItemKind::Talent]);
    }
}
