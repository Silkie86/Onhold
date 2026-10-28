//! Organisations: venues, promoters, brands, festivals, agencies.

use chrono::{DateTime, NaiveDate, Utc};
use core_db::Tx;
use core_db::organisations::{Item, Organisation};
use core_domain::{Ctx, Permission, UserId};
use core_engines::fields::{FieldDef, Mode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::ToSchema;
use uuid::Uuid;

use super::crm::{Check, Duplicate, ListArgs, ListQuery};
use super::pitching::{ItemKind, PitchingSettings, StatusMeaning};
use super::{Change, begin, check_version, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::routes::nullable;

pub const ENTITY: &str = "organisation";
pub const MAX_ADDRESSES: usize = 10;
/// The most talent or projects on one interested or potential match list.
pub const MAX_ITEMS: usize = 50;
/// The most similar acts named as bought from others.
pub const MAX_BOUGHT: usize = 50;

/// Whether an organisation has worked with the workspace: a client has, a
/// prospect has not yet. It moves by itself as enquiries and confirmed
/// bookings come in, and never back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Relationship {
    Client,
    Prospect,
}

impl Relationship {
    pub fn as_str(self) -> &'static str {
        match self {
            Relationship::Client => "client",
            Relationship::Prospect => "prospect",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "client" | "customer" => Some(Relationship::Client),
            "prospect" | "lead" => Some(Relationship::Prospect),
            _ => None,
        }
    }
}

/// Talent or a project on one of an organisation's lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ItemRef {
    pub kind: ItemKind,
    pub id: Uuid,
    pub name: String,
}

/// Talent or a project to put on one of an organisation's lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
pub struct ItemInput {
    pub kind: ItemKind,
    pub id: Uuid,
}

/// A postal address. Every part is optional, but not all of them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Address {
    /// What the address is for: "Main office", "Stage door".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line1: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line2: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    /// County, state or province (the world region is the organisation's own).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postcode: Option<String>,
    /// A country name or ISO code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
}

impl Address {
    pub fn is_empty(&self) -> bool {
        [
            &self.line1,
            &self.line2,
            &self.city,
            &self.region,
            &self.postcode,
            &self.country,
        ]
        .iter()
        .all(|p| p.is_none())
    }
}

fn check_addresses(check: &mut Check, addresses: &[Address]) -> Vec<Address> {
    if addresses.len() > MAX_ADDRESSES {
        check.issue(
            "addresses",
            format!("give at most {MAX_ADDRESSES} addresses"),
        );
    }
    let mut out = Vec::new();
    for (i, a) in addresses.iter().enumerate() {
        let mut part = |name: &str, v: &Option<String>| {
            check.optional(&format!("addresses[{i}].{name}"), v.as_deref(), 200)
        };
        let clean = Address {
            label: part("label", &a.label),
            line1: part("line1", &a.line1),
            line2: part("line2", &a.line2),
            city: part("city", &a.city),
            region: part("region", &a.region),
            postcode: part("postcode", &a.postcode),
            country: part("country", &a.country),
        };
        if !clean.is_empty() {
            out.push(clean);
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OrganisationView {
    pub id: Uuid,
    pub name: String,
    /// Free text: venue, promoter, brand, festival, agency…
    pub org_type: Option<String>,
    pub addresses: Vec<Address>,
    /// The country's ISO code, worked out from the main address however it
    /// was typed ("UK", "England", "Melbourne").
    pub country_code: Option<String>,
    /// The country's name.
    pub country: Option<String>,
    /// The main address's city.
    pub city: Option<String>,
    /// The key of its world region: the one picked for it, or its country's.
    pub region: Option<String>,
    /// Whether the region was picked by hand rather than taken from the country.
    pub region_picked: bool,
    pub relationship: Option<Relationship>,
    /// A key of the workspace's pitching statuses; absent when not set.
    pub status: Option<String>,
    pub follow_up_on: Option<NaiveDate>,
    /// Never offered for pitching.
    pub do_not_contact: bool,
    /// Similar acts or things they buy from others.
    pub buys_from_others: Vec<String>,
    /// What they asked about.
    pub interested_in: Vec<ItemRef>,
    /// What the workspace thinks would suit them.
    pub potential_matches: Vec<ItemRef>,
    /// VAT or other tax registration number.
    pub tax_id: Option<String>,
    pub default_currency: Option<String>,
    pub payment_terms_days: Option<i32>,
    pub notes: Option<String>,
    pub tags: Vec<String>,
    /// Custom field values by key.
    #[schema(value_type = Object)]
    pub custom: Value,
    /// Contacts at the organisation.
    pub contact_count: i64,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

fn refs(items: &[Item], organisation: Uuid, list: &str) -> Vec<ItemRef> {
    items
        .iter()
        .filter(|i| i.organisation_id == organisation && i.list == list)
        .filter_map(|i| {
            let (kind, id) = match (i.talent_id, i.project_id) {
                (Some(t), _) => (ItemKind::Talent, t),
                (None, Some(p)) => (ItemKind::Project, p),
                (None, None) => return None,
            };
            Some(ItemRef {
                kind,
                id,
                name: i.name.clone(),
            })
        })
        .collect()
}

/// The view of a row, with its region and status read through the
/// workspace's settings (a status no longer listed reads as not set).
pub(crate) fn view(o: Organisation, s: &PitchingSettings, items: &[Item]) -> OrganisationView {
    let region = s.region_of(o.country_code.as_deref(), o.region_key.as_deref());
    let region_picked = region.is_some_and(|r| Some(r.key.as_str()) == o.region_key.as_deref());
    OrganisationView {
        interested_in: refs(items, o.id, "interested"),
        potential_matches: refs(items, o.id, "match"),
        id: o.id,
        name: o.name,
        org_type: o.org_type,
        addresses: serde_json::from_value(o.addresses).unwrap_or_default(),
        country: o
            .country_code
            .as_deref()
            .and_then(core_engines::countries::by_code)
            .map(|c| c.name.to_owned()),
        country_code: o.country_code,
        city: o.city,
        region: region.map(|r| r.key.clone()),
        region_picked,
        relationship: o.relationship.as_deref().and_then(Relationship::parse),
        status: o.status_key.filter(|k| s.status(k).is_some()),
        follow_up_on: o.follow_up_on,
        do_not_contact: o.do_not_contact,
        buys_from_others: o.buys_from_others,
        tax_id: o.tax_id,
        default_currency: o.default_currency,
        payment_terms_days: o.payment_terms_days,
        notes: o.notes,
        tags: o.tags,
        custom: o.custom,
        contact_count: o.contact_count,
        version: o.version,
        created_at: o.created_at,
        updated_at: o.updated_at,
        created_by: o.created_by,
        updated_by: o.updated_by,
    }
}

/// The JSON events, the audit log, rules and templates see. The contact
/// count is left out: it changes without the organisation changing.
pub fn to_json(v: &OrganisationView) -> Value {
    let mut json = serde_json::to_value(v).unwrap_or(Value::Null);
    if let Some(o) = json.as_object_mut() {
        o.remove("contact_count");
    }
    json
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OrganisationPage {
    pub items: Vec<OrganisationView>,
    /// How many records match, across all pages.
    pub total: i64,
}

pub async fn list(state: &AppState, ctx: &Ctx, q: &ListQuery) -> ApiResult<OrganisationPage> {
    ctx.authorize(Permission::OrganisationRead)?;
    let mut tx = begin(state, ctx).await?;
    let args = ListArgs::parse(&mut tx, ENTITY, core_db::organisations::SORTS, q).await?;
    let (rows, total) = core_db::organisations::list(&mut tx, args.params()).await?;
    let (settings, _) = super::pitching::settings_in(&mut tx).await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let items = core_db::organisations::items_of(&mut tx, &ids).await?;
    tx.commit().await?;
    Ok(OrganisationPage {
        items: rows
            .into_iter()
            .map(|r| view(r, &settings, &items))
            .collect(),
        total,
    })
}

pub(crate) async fn load(tx: &mut Tx, id: Uuid) -> ApiResult<Option<OrganisationView>> {
    let Some(row) = core_db::organisations::get(tx, id).await? else {
        return Ok(None);
    };
    let (settings, _) = super::pitching::settings_in(tx).await?;
    let items = core_db::organisations::items_of(tx, &[id]).await?;
    Ok(Some(view(row, &settings, &items)))
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<OrganisationView> {
    ctx.authorize(Permission::OrganisationRead)?;
    let mut tx = begin(state, ctx).await?;
    let v = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(v)
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct NewOrganisation {
    pub name: String,
    pub org_type: Option<String>,
    #[serde(default)]
    pub addresses: Vec<Address>,
    pub tax_id: Option<String>,
    pub default_currency: Option<String>,
    pub payment_terms_days: Option<i32>,
    pub notes: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// A region key, to override the one its country gives.
    pub region: Option<String>,
    pub relationship: Option<Relationship>,
    /// A key of the workspace's pitching statuses.
    pub status: Option<String>,
    pub follow_up_on: Option<NaiveDate>,
    #[serde(default)]
    pub do_not_contact: bool,
    #[serde(default)]
    pub buys_from_others: Vec<String>,
    #[serde(default)]
    pub interested_in: Vec<ItemInput>,
    #[serde(default)]
    pub potential_matches: Vec<ItemInput>,
    /// Custom field values by key. Missing fields take their defaults.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub custom: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct OrganisationPatch {
    pub name: Option<String>,
    /// `null` clears it; likewise for the other optional fields.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub org_type: Option<Option<String>>,
    /// Replaces the addresses.
    pub addresses: Option<Vec<Address>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub tax_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub default_currency: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<i32>)]
    pub payment_terms_days: Option<Option<i32>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub notes: Option<Option<String>>,
    /// Replaces the tags.
    pub tags: Option<Vec<String>>,
    /// `null` goes back to the country's region.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub region: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<Relationship>)]
    pub relationship: Option<Option<Relationship>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub status: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<NaiveDate>)]
    pub follow_up_on: Option<Option<NaiveDate>>,
    pub do_not_contact: Option<bool>,
    /// Replaces the list.
    pub buys_from_others: Option<Vec<String>>,
    /// Replaces the list.
    pub interested_in: Option<Vec<ItemInput>>,
    /// Replaces the list.
    pub potential_matches: Option<Vec<ItemInput>>,
    /// Only the keys sent change; `null` clears a value.
    #[schema(value_type = Option<Object>)]
    pub custom: Option<Map<String, Value>>,
}

pub(crate) struct Checked {
    name: String,
    org_type: Option<String>,
    addresses: Value,
    tax_id: Option<String>,
    default_currency: Option<String>,
    payment_terms_days: Option<i32>,
    notes: Option<String>,
    tags: Vec<String>,
    custom: Value,
    country_code: Option<String>,
    city: Option<String>,
    region_key: Option<String>,
    relationship: Option<Relationship>,
    status_key: Option<String>,
    follow_up_on: Option<NaiveDate>,
    do_not_contact: bool,
    buys_from_others: Vec<String>,
}

impl Checked {
    fn values(&self) -> core_db::organisations::Values<'_> {
        core_db::organisations::Values {
            name: &self.name,
            org_type: self.org_type.as_deref(),
            addresses: &self.addresses,
            tax_id: self.tax_id.as_deref(),
            default_currency: self.default_currency.as_deref(),
            payment_terms_days: self.payment_terms_days,
            notes: self.notes.as_deref(),
            tags: &self.tags,
            custom: &self.custom,
            country_code: self.country_code.as_deref(),
            city: self.city.as_deref(),
            region_key: self.region_key.as_deref(),
            relationship: self.relationship.map(Relationship::as_str),
            status_key: self.status_key.as_deref(),
            follow_up_on: self.follow_up_on,
            do_not_contact: self.do_not_contact,
            buys_from_others: &self.buys_from_others,
        }
    }
}

fn terms(check: &mut Check, days: Option<i32>) -> Option<i32> {
    let d = days?;
    if !(0..=365).contains(&d) {
        check.issue("payment_terms_days", "must be 0 to 365 days");
    }
    Some(d)
}

fn addresses_json(list: Vec<Address>) -> Value {
    serde_json::to_value(list).unwrap_or_else(|_| Value::Array(Vec::new()))
}

/// The country (ISO code) and city of the first address that says where it
/// is. The country is read however it was typed; failing that, from a
/// well-known city.
pub(crate) fn place(addresses: &Value) -> (Option<String>, Option<String>) {
    let list: Vec<Address> = serde_json::from_value(addresses.clone()).unwrap_or_default();
    let Some(a) = list
        .iter()
        .find(|a| a.country.is_some() || a.city.is_some())
    else {
        return (None, None);
    };
    let found = a
        .country
        .as_deref()
        .and_then(core_engines::countries::find)
        // A city is only trusted when it names one place outright, not as
        // a near miss of a country's name.
        .or_else(|| {
            a.city
                .as_deref()
                .and_then(core_engines::countries::find)
                .filter(|f| f.how != core_engines::countries::How::Typo && f.others.is_empty())
        });
    let city = a
        .city
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_owned);
    (found.map(|f| f.country.code.to_owned()), city)
}

fn check_region(check: &mut Check, s: &PitchingSettings, key: Option<&str>) -> Option<String> {
    let key = key.map(str::trim).filter(|k| !k.is_empty())?;
    if s.region(key).is_none() {
        check.issue(
            "region",
            format!("`{key}` is not one of the workspace's regions"),
        );
    }
    Some(key.to_owned())
}

fn check_status(check: &mut Check, s: &PitchingSettings, key: Option<&str>) -> Option<String> {
    let key = key.map(str::trim).filter(|k| !k.is_empty())?;
    if s.status(key).is_none() {
        check.issue(
            "status",
            format!("`{key}` is not one of the workspace's statuses"),
        );
    }
    Some(key.to_owned())
}

/// Names of acts or things bought from others: trimmed, without repeats
/// (whatever their case), each at most 100 characters.
fn check_bought(check: &mut Check, names: &[String]) -> Vec<String> {
    // Far too many is refused before anything is compared.
    if names.len() > 2 * MAX_BOUGHT {
        check.issue("buys_from_others", format!("name at most {MAX_BOUGHT}"));
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for n in names {
        let n = n.split_whitespace().collect::<Vec<_>>().join(" ");
        if n.is_empty() || out.iter().any(|o| o.to_lowercase() == n.to_lowercase()) {
            continue;
        }
        if n.chars().count() > 100 {
            check.issue(
                "buys_from_others",
                format!("`{n}` is longer than 100 characters"),
            );
            continue;
        }
        out.push(n);
    }
    if out.len() > MAX_BOUGHT {
        check.issue("buys_from_others", format!("name at most {MAX_BOUGHT}"));
    }
    out
}

/// A list without repeats, each talent or project checked to exist, as
/// (talent id, project id) pairs.
async fn check_items(
    tx: &mut Tx,
    check: &mut Check,
    field: &str,
    items: &[ItemInput],
) -> ApiResult<Vec<(Option<Uuid>, Option<Uuid>)>> {
    if items.len() > 2 * MAX_ITEMS {
        check.issue(field, format!("name at most {MAX_ITEMS}"));
        return Ok(Vec::new());
    }
    let mut out: Vec<ItemInput> = Vec::new();
    for i in items {
        if !out.contains(i) {
            out.push(*i);
        }
    }
    if out.len() > MAX_ITEMS {
        check.issue(field, format!("name at most {MAX_ITEMS}"));
        return Ok(Vec::new());
    }
    let ids =
        |k: ItemKind| -> Vec<Uuid> { out.iter().filter(|i| i.kind == k).map(|i| i.id).collect() };
    let talent = core_db::organisations::live_talent(tx, &ids(ItemKind::Talent)).await?;
    let projects = core_db::organisations::live_projects(tx, &ids(ItemKind::Project)).await?;
    let mut pairs = Vec::new();
    for i in &out {
        let known = match i.kind {
            ItemKind::Talent => talent.contains(&i.id),
            ItemKind::Project => projects.contains(&i.id),
        };
        if !known {
            check.issue(field, format!("there is no {} {}", i.kind.as_str(), i.id));
            continue;
        }
        pairs.push(match i.kind {
            ItemKind::Talent => (Some(i.id), None),
            ItemKind::Project => (None, Some(i.id)),
        });
    }
    Ok(pairs)
}

/// Checks a new record's values against each other and the custom field
/// definitions, collecting every problem.
pub(crate) fn check_new(
    new: &NewOrganisation,
    defs: &[FieldDef],
    settings: &PitchingSettings,
) -> (Checked, Check) {
    let mut check = Check::default();
    let addresses = addresses_json(check_addresses(&mut check, &new.addresses));
    let (country_code, city) = place(&addresses);
    let checked = Checked {
        country_code,
        city,
        region_key: check_region(&mut check, settings, new.region.as_deref()),
        relationship: new.relationship,
        status_key: check_status(&mut check, settings, new.status.as_deref()),
        follow_up_on: new.follow_up_on,
        do_not_contact: new.do_not_contact,
        buys_from_others: check_bought(&mut check, &new.buys_from_others),
        name: check.required("name", &new.name, 200),
        org_type: check.optional("org_type", new.org_type.as_deref(), 100),
        addresses,
        tax_id: check.optional("tax_id", new.tax_id.as_deref(), 50),
        default_currency: check.currency("default_currency", new.default_currency.as_deref()),
        payment_terms_days: terms(&mut check, new.payment_terms_days),
        notes: check.long("notes", new.notes.as_deref(), 20_000),
        tags: check.tags(&new.tags),
        custom: Value::Object(check.custom(defs, &Map::new(), &new.custom, Mode::Create)),
    };
    (checked, check)
}

pub async fn create(
    state: &AppState,
    ctx: &Ctx,
    new: NewOrganisation,
) -> ApiResult<OrganisationView> {
    ctx.authorize(Permission::OrganisationWrite)?;
    let mut tx = begin(state, ctx).await?;
    let view = create_in_tx(&mut tx, ctx, new, None).await?;
    tx.commit().await?;
    Ok(view)
}

/// The create, inside the caller's transaction (imports use this).
pub(crate) async fn create_in_tx(
    tx: &mut Tx,
    ctx: &Ctx,
    new: NewOrganisation,
    event_extra: Option<Value>,
) -> ApiResult<OrganisationView> {
    let defs = super::fields::engine_defs(tx, ENTITY).await?;
    let (settings, _) = super::pitching::settings_in(tx).await?;
    let (checked, mut check) = check_new(&new, &defs, &settings);
    let interested = check_items(tx, &mut check, "interested_in", &new.interested_in).await?;
    let matches = check_items(tx, &mut check, "potential_matches", &new.potential_matches).await?;
    check.finish()?;

    let id = core_domain::ids::new_id();
    core_db::organisations::insert(tx, id, checked.values(), ctx.user_id()).await?;
    core_db::organisations::set_items(tx, id, "interested", &interested).await?;
    core_db::organisations::set_items(tx, id, "match", &matches).await?;
    let view = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "organisation.created",
            resource: ENTITY,
            object_id: Some(id),
            before: None,
            after: Some(to_json(&view)),
            event_extra,
        },
    )
    .await?;
    Ok(view)
}

pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    patch: OrganisationPatch,
    if_match: Option<i64>,
) -> ApiResult<OrganisationView> {
    ctx.authorize(Permission::OrganisationWrite)?;
    let mut tx = begin(state, ctx).await?;
    let view = update_in_tx(&mut tx, ctx, id, patch, if_match, None).await?;
    tx.commit().await?;
    Ok(view)
}

/// The update, inside the caller's transaction (rules and imports use this).
pub(crate) async fn update_in_tx(
    tx: &mut Tx,
    ctx: &Ctx,
    id: Uuid,
    p: OrganisationPatch,
    if_match: Option<i64>,
    event_extra: Option<Value>,
) -> ApiResult<OrganisationView> {
    let current = core_db::organisations::get_for_update(tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(current.version, if_match)?;
    let before = load(tx, id).await?.ok_or(ApiError::NotFound)?;

    let mut check = Check::default();
    let current_custom = current.custom.as_object().cloned().unwrap_or_default();
    let custom = match &p.custom {
        Some(changes) => {
            let defs = super::fields::engine_defs(tx, ENTITY).await?;
            check.custom(&defs, &current_custom, changes, Mode::Update)
        }
        None => current_custom,
    };
    let (settings, _) = super::pitching::settings_in(tx).await?;
    let addresses = match &p.addresses {
        Some(a) => addresses_json(check_addresses(&mut check, a)),
        None => current.addresses.clone(),
    };
    // Worked out afresh, so a record saved before countries were read gets one.
    let (country_code, city) = place(&addresses);
    let interested = match &p.interested_in {
        Some(list) => Some(check_items(tx, &mut check, "interested_in", list).await?),
        None => None,
    };
    let matches = match &p.potential_matches {
        Some(list) => Some(check_items(tx, &mut check, "potential_matches", list).await?),
        None => None,
    };
    let checked = Checked {
        country_code,
        city,
        region_key: match &p.region {
            Some(v) => check_region(&mut check, &settings, v.as_deref()),
            None => current.region_key.clone(),
        },
        relationship: match p.relationship {
            Some(v) => v,
            None => current
                .relationship
                .as_deref()
                .and_then(Relationship::parse),
        },
        status_key: match &p.status {
            Some(v) => check_status(&mut check, &settings, v.as_deref()),
            None => current.status_key.clone(),
        },
        follow_up_on: p.follow_up_on.unwrap_or(current.follow_up_on),
        do_not_contact: p.do_not_contact.unwrap_or(current.do_not_contact),
        buys_from_others: match &p.buys_from_others {
            Some(b) => check_bought(&mut check, b),
            None => current.buys_from_others.clone(),
        },
        name: match &p.name {
            Some(n) => check.required("name", n, 200),
            None => current.name.clone(),
        },
        org_type: match &p.org_type {
            Some(v) => check.optional("org_type", v.as_deref(), 100),
            None => current.org_type.clone(),
        },
        addresses,
        tax_id: match &p.tax_id {
            Some(v) => check.optional("tax_id", v.as_deref(), 50),
            None => current.tax_id.clone(),
        },
        default_currency: match &p.default_currency {
            Some(v) => check.currency("default_currency", v.as_deref()),
            None => current.default_currency.clone(),
        },
        payment_terms_days: match p.payment_terms_days {
            Some(v) => terms(&mut check, v),
            None => current.payment_terms_days,
        },
        notes: match &p.notes {
            Some(v) => check.long("notes", v.as_deref(), 20_000),
            None => current.notes.clone(),
        },
        tags: match &p.tags {
            Some(t) => check.tags(t),
            None => current.tags.clone(),
        },
        custom: Value::Object(custom),
    };
    check.finish()?;

    let unchanged = checked.name == current.name
        && checked.org_type == current.org_type
        && checked.addresses == current.addresses
        && checked.tax_id == current.tax_id
        && checked.default_currency == current.default_currency
        && checked.payment_terms_days == current.payment_terms_days
        && checked.notes == current.notes
        && checked.tags == current.tags
        && checked.custom == current.custom
        && checked.country_code == current.country_code
        && checked.city == current.city
        && checked.region_key == current.region_key
        && checked.relationship.map(Relationship::as_str) == current.relationship.as_deref()
        && checked.status_key == current.status_key
        && checked.follow_up_on == current.follow_up_on
        && checked.do_not_contact == current.do_not_contact
        && checked.buys_from_others == current.buys_from_others;
    let same_list = |new: &Option<Vec<(Option<Uuid>, Option<Uuid>)>>, now: &[ItemRef]| {
        new.as_ref().is_none_or(|n| {
            n.len() == now.len() && n.iter().zip(now).all(|((t, p), r)| t.or(*p) == Some(r.id))
        })
    };
    if unchanged
        && same_list(&interested, &before.interested_in)
        && same_list(&matches, &before.potential_matches)
    {
        return Ok(before);
    }

    core_db::organisations::update(tx, id, checked.values(), ctx.user_id()).await?;
    if let Some(list) = &interested {
        core_db::organisations::set_items(tx, id, "interested", list).await?;
    }
    if let Some(list) = &matches {
        core_db::organisations::set_items(tx, id, "match", list).await?;
    }
    let after = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "organisation.updated",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra,
        },
    )
    .await?;
    Ok(after)
}

/// Deleting an organisation keeps its contacts, still linked, so their
/// history reads correctly.
pub async fn delete(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::OrganisationWrite)?;
    let mut tx = begin(state, ctx).await?;
    delete_in_tx(&mut tx, ctx, id, None).await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn delete_in_tx(
    tx: &mut Tx,
    ctx: &Ctx,
    id: Uuid,
    event_extra: Option<Value>,
) -> ApiResult<()> {
    let before = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    core_db::organisations::soft_delete(tx, id, ctx.user_id()).await?;
    record(
        tx,
        ctx,
        Change {
            topic: "organisation.deleted",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: None,
            event_extra,
        },
    )
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct OrganisationDuplicateQuery {
    pub name: String,
    pub tax_id: Option<String>,
    /// The record being edited, which is not its own duplicate.
    pub except: Option<Uuid>,
}

/// Organisations that may be the one being entered: similar names or the
/// same tax id.
pub async fn duplicates(
    state: &AppState,
    ctx: &Ctx,
    q: &OrganisationDuplicateQuery,
) -> ApiResult<Vec<Duplicate>> {
    ctx.authorize(Permission::OrganisationRead)?;
    let name = q.name.trim().to_lowercase();
    let tax_id = q.tax_id.as_deref().map(str::trim).filter(|t| !t.is_empty());
    if name.is_empty() && tax_id.is_none() {
        return Ok(Vec::new());
    }
    let mut tx = begin(state, ctx).await?;
    let rows = core_db::organisations::similar(
        &mut tx,
        &name,
        tax_id,
        super::crm::SIMILAR_NAME,
        q.except,
        super::crm::MAX_CANDIDATES,
    )
    .await?;
    tx.commit().await?;
    Ok(rows
        .into_iter()
        .map(|(c, same_tax_id)| {
            let mut reasons = Vec::new();
            if same_tax_id {
                reasons.push("same tax id".to_owned());
            }
            reasons.extend(super::crm::name_reason(c.similarity));
            Duplicate {
                id: c.id,
                name: c.name,
                reasons,
            }
        })
        .collect())
}

/// What came in for an organisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Deal {
    /// An enquiry, or a booking not yet confirmed.
    Enquiry,
    Confirmed,
}

/// Moves an organisation along as deals come in, never back: an enquiry
/// makes it a prospect (unless it is a client) and sets a "talking" status
/// if none is set; a confirmed booking makes it a client and sets a
/// "booked" status unless it has one. Recorded as an ordinary update.
pub(crate) async fn advance_in_tx(tx: &mut Tx, ctx: &Ctx, id: Uuid, deal: Deal) -> ApiResult<()> {
    let Some(before) = load(tx, id).await? else {
        return Ok(());
    };
    let (settings, _) = super::pitching::settings_in(tx).await?;
    let (relationship, status, replaces): (&str, _, Vec<String>) = match deal {
        Deal::Enquiry => (
            "prospect",
            settings.first_meaning(StatusMeaning::Talking),
            Vec::new(),
        ),
        Deal::Confirmed => (
            "client",
            settings.first_meaning(StatusMeaning::Booked),
            // Anything short of booked gives way.
            settings
                .statuses
                .iter()
                .filter(|s| s.meaning != StatusMeaning::Booked)
                .map(|s| s.key.clone())
                .collect(),
        ),
    };
    let listed: Vec<String> = settings.statuses.iter().map(|s| s.key.clone()).collect();
    let changed = core_db::organisations::advance(
        tx,
        id,
        relationship,
        status.map(|s| s.key.as_str()),
        &replaces,
        &listed,
        ctx.user_id(),
    )
    .await?;
    if !changed {
        return Ok(());
    }
    let after = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "organisation.updated",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    Ok(())
}
