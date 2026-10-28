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
use serde_json::json;
use utoipa::ToSchema;

use super::crm::Check;
use super::{Change, begin, check_version, record};
use crate::AppState;
use crate::error::ApiResult;

/// The module's key in a workspace's enabled modules.
pub const MODULE: &str = "pitching";

pub const MAX_REGIONS: usize = 30;
pub const MAX_STATUSES: usize = 12;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PitchingSettings {
    #[serde(default = "default_regions")]
    pub regions: Vec<Region>,
    #[serde(default = "default_statuses")]
    pub statuses: Vec<Status>,
    #[serde(default = "default_items")]
    pub items: Vec<ItemKind>,
}

impl Default for PitchingSettings {
    fn default() -> Self {
        Self {
            regions: default_regions(),
            statuses: default_statuses(),
            items: default_items(),
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
}

fn is_key(k: &str) -> bool {
    (1..=40).contains(&k.len())
        && k.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
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
        let key = r.key.trim().to_owned();
        if !is_key(&key) {
            check.issue(
                format!("regions[{i}].key"),
                "use 1 to 40 lower case letters, digits or underscores",
            );
        } else if regions.iter().any(|x| x.key == key) {
            check.issue(format!("regions[{i}].key"), "is used twice");
        }
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
        let key = s.key.trim().to_owned();
        if !is_key(&key) {
            check.issue(
                format!("statuses[{i}].key"),
                "use 1 to 40 lower case letters, digits or underscores",
            );
        } else if statuses.iter().any(|x| x.key == key) {
            check.issue(format!("statuses[{i}].key"), "is used twice");
        }
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
    check.finish()?;
    Ok(PitchingSettings {
        regions,
        statuses,
        items,
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
    input: PitchingSettings,
    if_match: Option<i64>,
) -> ApiResult<PitchingSettingsView> {
    ctx.authorize(Permission::ConfigWrite)?;
    let checked = check_settings(input)?;
    let mut tx = begin(state, ctx).await?;
    core_db::pitching::lock(&mut tx).await?;
    let (before, version) = settings_in(&mut tx).await?;
    check_version(version, if_match)?;
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
