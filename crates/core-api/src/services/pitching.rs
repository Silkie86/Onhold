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
}

impl PitchingSettingsInput {
    fn over(self, stored: PitchingSettings) -> PitchingSettings {
        PitchingSettings {
            regions: self.regions.unwrap_or(stored.regions),
            statuses: self.statuses.unwrap_or(stored.statuses),
            items: self.items.unwrap_or(stored.items),
            outcomes: self.outcomes.unwrap_or(stored.outcomes),
            actions: self.actions.unwrap_or(stored.actions),
            templates: self.templates.unwrap_or(stored.templates),
            send_with: self.send_with.unwrap_or(stored.send_with),
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
    let o = |key: &str, label: &str, tag| PitchOutcome {
        key: key.into(),
        label: label.into(),
        tag,
    };
    vec![
        o("interested", "Interested or yes", OutcomeTag::Good),
        o("not_now", "Not now", OutcomeTag::Wait),
        o("no", "No", OutcomeTag::No),
        o("not_a_fit", "Not a fit", OutcomeTag::Never),
        o("no_answer", "No answer", OutcomeTag::Wait),
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
        outcomes.push(PitchOutcome {
            key,
            label,
            tag: o.tag,
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
    check.finish()?;
    Ok(PitchingSettings {
        regions,
        statuses,
        items,
        outcomes,
        actions,
        templates,
        send_with: input.send_with,
    })
}

/// The workspace's settings, or OnHold's defaults where it has none. A
/// stored value that no longer reads (from a later release, say) falls
/// back to the defaults rather than failing every page that needs it.
pub(crate) async fn settings_in(tx: &mut Tx) -> ApiResult<(PitchingSettings, i64)> {
    Ok(match core_db::pitching::settings(tx).await? {
        Some(row) => (
            serde_json::from_value(row.settings).unwrap_or_default(),
            row.version,
        ),
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
