//! Organisations: venues, promoters, brands, festivals, agencies.

use chrono::{DateTime, Utc};
use core_db::Tx;
use core_db::organisations::Organisation;
use core_domain::{Ctx, Permission, UserId};
use core_engines::fields::{FieldDef, Mode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::ToSchema;
use uuid::Uuid;

use super::crm::{Check, Duplicate, ListArgs, ListQuery};
use super::{Change, begin, check_version, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::routes::nullable;

pub const ENTITY: &str = "organisation";
pub const MAX_ADDRESSES: usize = 10;

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
    /// State, county or province.
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

impl From<Organisation> for OrganisationView {
    fn from(o: Organisation) -> Self {
        Self {
            id: o.id,
            name: o.name,
            org_type: o.org_type,
            addresses: serde_json::from_value(o.addresses).unwrap_or_default(),
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
    tx.commit().await?;
    Ok(OrganisationPage {
        items: rows.into_iter().map(Into::into).collect(),
        total,
    })
}

pub(crate) async fn load(tx: &mut Tx, id: Uuid) -> ApiResult<Option<OrganisationView>> {
    Ok(core_db::organisations::get(tx, id).await?.map(Into::into))
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

/// Checks a new record's values against each other and the custom field
/// definitions, collecting every problem.
pub(crate) fn check_new(new: &NewOrganisation, defs: &[FieldDef]) -> (Checked, Check) {
    let mut check = Check::default();
    let checked = Checked {
        name: check.required("name", &new.name, 200),
        org_type: check.optional("org_type", new.org_type.as_deref(), 100),
        addresses: addresses_json(check_addresses(&mut check, &new.addresses)),
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
    let (checked, check) = check_new(&new, &defs);
    check.finish()?;

    let id = core_domain::ids::new_id();
    core_db::organisations::insert(tx, id, checked.values(), ctx.user_id()).await?;
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
    let checked = Checked {
        name: match &p.name {
            Some(n) => check.required("name", n, 200),
            None => current.name.clone(),
        },
        org_type: match &p.org_type {
            Some(v) => check.optional("org_type", v.as_deref(), 100),
            None => current.org_type.clone(),
        },
        addresses: match &p.addresses {
            Some(a) => addresses_json(check_addresses(&mut check, a)),
            None => current.addresses.clone(),
        },
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
        && checked.custom == current.custom;
    if unchanged {
        return Ok(before);
    }

    core_db::organisations::update(tx, id, checked.values(), ctx.user_id()).await?;
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
    delete_in_tx(&mut tx, ctx, id).await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn delete_in_tx(tx: &mut Tx, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
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
            event_extra: None,
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
