//! Contacts: people, at an organisation or on their own.

use chrono::{DateTime, Utc};
use core_db::Tx;
use core_db::contacts::Contact;
use core_domain::crm::{email_key, phone_key};
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

pub const ENTITY: &str = "contact";
pub const MAX_EMAILS: usize = 10;
pub const MAX_PHONES: usize = 10;
pub const MAX_LINKS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Email,
    Phone,
    Sms,
    Whatsapp,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Email => "email",
            Channel::Phone => "phone",
            Channel::Sms => "sms",
            Channel::Whatsapp => "whatsapp",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "email" | "e-mail" => Some(Channel::Email),
            "phone" | "call" | "telephone" => Some(Channel::Phone),
            "sms" | "text" => Some(Channel::Sms),
            "whatsapp" => Some(Channel::Whatsapp),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ContactView {
    pub id: Uuid,
    pub name: String,
    pub organisation_id: Option<Uuid>,
    pub organisation_name: Option<String>,
    /// Lower-cased; the first is the primary address.
    pub emails: Vec<String>,
    /// The primary email, if any.
    pub email: Option<String>,
    /// As entered; the first is the primary number.
    pub phones: Vec<String>,
    /// The primary phone number, if any.
    pub phone: Option<String>,
    /// The numbers with their country code (+44…), where it can be told
    /// from the number or the organisation's country; as entered otherwise.
    pub phones_international: Vec<String>,
    /// Web pages and social profiles, as full addresses.
    pub links: Vec<String>,
    /// Job title or role at the organisation.
    pub role: Option<String>,
    pub preferred_channel: Option<Channel>,
    /// Consent to marketing by email, SMS and WhatsApp.
    pub email_opt_in: bool,
    pub sms_opt_in: bool,
    pub whatsapp_opt_in: bool,
    pub notes: Option<String>,
    pub tags: Vec<String>,
    /// Custom field values by key.
    #[schema(value_type = Object)]
    pub custom: Value,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

impl From<Contact> for ContactView {
    fn from(c: Contact) -> Self {
        Self {
            id: c.id,
            name: c.name,
            organisation_id: c.organisation_id,
            organisation_name: c.organisation_name,
            email: c.emails.first().cloned(),
            emails: c.emails,
            phone: c.phones.first().cloned(),
            phones_international: c
                .phones
                .iter()
                .map(|p| {
                    core_engines::countries::international_phone(
                        p,
                        c.organisation_country.as_deref(),
                    )
                    .unwrap_or_else(|| p.clone())
                })
                .collect(),
            phones: c.phones,
            links: c.links,
            role: c.role,
            preferred_channel: c.preferred_channel.as_deref().and_then(Channel::parse),
            email_opt_in: c.email_opt_in,
            sms_opt_in: c.sms_opt_in,
            whatsapp_opt_in: c.whatsapp_opt_in,
            notes: c.notes,
            tags: c.tags,
            custom: c.custom,
            version: c.version,
            created_at: c.created_at,
            updated_at: c.updated_at,
            created_by: c.created_by,
            updated_by: c.updated_by,
        }
    }
}

/// The JSON events, the audit log, rules and templates see. The
/// international numbers are left out: they change with the organisation's
/// country without the contact changing.
pub fn to_json(v: &ContactView) -> Value {
    let mut json = serde_json::to_value(v).unwrap_or(Value::Null);
    if let Some(o) = json.as_object_mut() {
        o.remove("phones_international");
    }
    json
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ContactPage {
    pub items: Vec<ContactView>,
    /// How many records match, across all pages.
    pub total: i64,
}

pub async fn list(
    state: &AppState,
    ctx: &Ctx,
    q: &ListQuery,
    organisation_id: Option<Uuid>,
) -> ApiResult<ContactPage> {
    ctx.authorize(Permission::ContactRead)?;
    let mut tx = begin(state, ctx).await?;
    let args = ListArgs::parse(&mut tx, ENTITY, core_db::contacts::SORTS, q).await?;
    let (rows, total) = core_db::contacts::list(
        &mut tx,
        core_db::contacts::Filter {
            list: args.params(),
            organisation_id,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(ContactPage {
        items: rows.into_iter().map(Into::into).collect(),
        total,
    })
}

pub(crate) async fn load(tx: &mut Tx, id: Uuid) -> ApiResult<Option<ContactView>> {
    Ok(core_db::contacts::get(tx, id).await?.map(Into::into))
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<ContactView> {
    ctx.authorize(Permission::ContactRead)?;
    let mut tx = begin(state, ctx).await?;
    let v = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(v)
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct NewContact {
    pub name: String,
    pub organisation_id: Option<Uuid>,
    #[serde(default)]
    pub emails: Vec<String>,
    #[serde(default)]
    pub phones: Vec<String>,
    pub role: Option<String>,
    pub preferred_channel: Option<Channel>,
    #[serde(default)]
    pub email_opt_in: bool,
    #[serde(default)]
    pub sms_opt_in: bool,
    #[serde(default)]
    pub whatsapp_opt_in: bool,
    pub notes: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Web pages and social profiles. A bare address gets `https://`.
    #[serde(default)]
    pub links: Vec<String>,
    /// Custom field values by key. Missing fields take their defaults.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub custom: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct ContactPatch {
    pub name: Option<String>,
    /// `null` clears it; likewise for the other optional fields.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<Uuid>)]
    pub organisation_id: Option<Option<Uuid>>,
    /// Replaces the addresses.
    pub emails: Option<Vec<String>>,
    /// Replaces the numbers.
    pub phones: Option<Vec<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub role: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<Channel>)]
    pub preferred_channel: Option<Option<Channel>>,
    pub email_opt_in: Option<bool>,
    pub sms_opt_in: Option<bool>,
    pub whatsapp_opt_in: Option<bool>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub notes: Option<Option<String>>,
    /// Replaces the tags.
    pub tags: Option<Vec<String>>,
    /// Replaces the links.
    pub links: Option<Vec<String>>,
    /// Only the keys sent change; `null` clears a value.
    #[schema(value_type = Option<Object>)]
    pub custom: Option<Map<String, Value>>,
}

pub(crate) struct Checked {
    organisation_id: Option<Uuid>,
    name: String,
    emails: Vec<String>,
    phones: Vec<String>,
    phone_keys: Vec<String>,
    role: Option<String>,
    preferred_channel: Option<Channel>,
    email_opt_in: bool,
    sms_opt_in: bool,
    whatsapp_opt_in: bool,
    notes: Option<String>,
    tags: Vec<String>,
    custom: Value,
    links: Vec<String>,
}

impl Checked {
    fn values(&self) -> core_db::contacts::Values<'_> {
        core_db::contacts::Values {
            organisation_id: self.organisation_id,
            name: &self.name,
            emails: &self.emails,
            phones: &self.phones,
            phone_keys: &self.phone_keys,
            role: self.role.as_deref(),
            preferred_channel: self.preferred_channel.map(Channel::as_str),
            email_opt_in: self.email_opt_in,
            sms_opt_in: self.sms_opt_in,
            whatsapp_opt_in: self.whatsapp_opt_in,
            notes: self.notes.as_deref(),
            tags: &self.tags,
            custom: &self.custom,
            links: &self.links,
        }
    }
}

/// Lower-cased, without blanks or repeats, each a plausible address.
pub(crate) fn check_emails(check: &mut Check, emails: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in emails {
        let e = email_key(e);
        if e.is_empty() || out.contains(&e) {
            continue;
        }
        if e.len() > 320 || !core_engines::fields::is_email(&e) {
            check.issue("emails", format!("`{e}` is not an email address"));
            continue;
        }
        out.push(e);
    }
    if out.len() > MAX_EMAILS {
        check.issue("emails", format!("give at most {MAX_EMAILS} addresses"));
    }
    out
}

/// Trimmed, without blanks or repeats, each with 5 to 20 digits.
pub(crate) fn check_phones(check: &mut Check, phones: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in phones {
        let p = p.split_whitespace().collect::<Vec<_>>().join(" ");
        if p.is_empty() || out.contains(&p) {
            continue;
        }
        let digits = p.chars().filter(char::is_ascii_digit).count();
        let shape_ok = p
            .chars()
            .all(|c| c.is_ascii_digit() || " +-().".contains(c));
        if !shape_ok || !(5..=20).contains(&digits) || p.len() > 50 {
            check.issue("phones", format!("`{p}` is not a phone number"));
            continue;
        }
        out.push(p);
    }
    if out.len() > MAX_PHONES {
        check.issue("phones", format!("give at most {MAX_PHONES} numbers"));
    }
    out
}

/// Full web addresses without repeats: `instagram.com/x` becomes
/// `https://instagram.com/x`; anything that is not a web page is refused.
pub(crate) fn check_links(check: &mut Check, links: &[String]) -> Vec<String> {
    // Far too many is refused before anything is parsed or compared.
    if links.len() > 2 * MAX_LINKS {
        check.issue("links", format!("give at most {MAX_LINKS} links"));
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for l in links {
        let l = l.trim();
        if l.is_empty() {
            continue;
        }
        let full = if l.contains("://") {
            l.to_owned()
        } else {
            format!("https://{}", l.trim_start_matches('/'))
        };
        let ok = full.len() <= 500
            && url::Url::parse(&full).is_ok_and(|u| {
                matches!(u.scheme(), "http" | "https")
                    && u.host_str().is_some_and(|h| h.contains('.'))
            });
        if !ok {
            check.issue("links", format!("`{l}` is not a web address"));
            continue;
        }
        if !out.contains(&full) {
            out.push(full);
        }
    }
    if out.len() > MAX_LINKS {
        check.issue("links", format!("give at most {MAX_LINKS} links"));
    }
    out
}

pub(crate) fn phone_keys(phones: &[String]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for k in phones.iter().filter_map(|p| phone_key(p)) {
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    keys
}

async fn check_organisation(tx: &mut Tx, check: &mut Check, id: Option<Uuid>) -> ApiResult<()> {
    if let Some(id) = id
        && core_db::organisations::get(tx, id).await?.is_none()
    {
        check.issue("organisation_id", "there is no such organisation");
    }
    Ok(())
}

/// Checks a new record's values against each other and the custom field
/// definitions, collecting every problem. Whether the organisation exists
/// is checked separately.
pub(crate) fn check_new(new: &NewContact, defs: &[FieldDef]) -> (Checked, Check) {
    let mut check = Check::default();
    let emails = check_emails(&mut check, &new.emails);
    let phones = check_phones(&mut check, &new.phones);
    let checked = Checked {
        organisation_id: new.organisation_id,
        name: check.required("name", &new.name, 200),
        phone_keys: phone_keys(&phones),
        emails,
        phones,
        role: check.optional("role", new.role.as_deref(), 200),
        preferred_channel: new.preferred_channel,
        email_opt_in: new.email_opt_in,
        sms_opt_in: new.sms_opt_in,
        whatsapp_opt_in: new.whatsapp_opt_in,
        notes: check.long("notes", new.notes.as_deref(), 20_000),
        tags: check.tags(&new.tags),
        custom: Value::Object(check.custom(defs, &Map::new(), &new.custom, Mode::Create)),
        links: check_links(&mut check, &new.links),
    };
    (checked, check)
}

pub async fn create(state: &AppState, ctx: &Ctx, new: NewContact) -> ApiResult<ContactView> {
    ctx.authorize(Permission::ContactWrite)?;
    let mut tx = begin(state, ctx).await?;
    let view = create_in_tx(&mut tx, ctx, new, None).await?;
    tx.commit().await?;
    Ok(view)
}

/// The create, inside the caller's transaction (imports use this).
pub(crate) async fn create_in_tx(
    tx: &mut Tx,
    ctx: &Ctx,
    new: NewContact,
    event_extra: Option<Value>,
) -> ApiResult<ContactView> {
    let defs = super::fields::engine_defs(tx, ENTITY).await?;
    let (checked, mut check) = check_new(&new, &defs);
    check_organisation(tx, &mut check, new.organisation_id).await?;
    check.finish()?;

    let id = core_domain::ids::new_id();
    core_db::contacts::insert(tx, id, checked.values(), ctx.user_id()).await?;
    let view = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "contact.created",
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
    patch: ContactPatch,
    if_match: Option<i64>,
) -> ApiResult<ContactView> {
    ctx.authorize(Permission::ContactWrite)?;
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
    p: ContactPatch,
    if_match: Option<i64>,
    event_extra: Option<Value>,
) -> ApiResult<ContactView> {
    let current = core_db::contacts::get_for_update(tx, id)
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
    let emails = match &p.emails {
        Some(e) => check_emails(&mut check, e),
        None => current.emails.clone(),
    };
    let phones = match &p.phones {
        Some(ph) => check_phones(&mut check, ph),
        None => current.phones.clone(),
    };
    let checked = Checked {
        organisation_id: p.organisation_id.unwrap_or(current.organisation_id),
        name: match &p.name {
            Some(n) => check.required("name", n, 200),
            None => current.name.clone(),
        },
        phone_keys: phone_keys(&phones),
        emails,
        phones,
        role: match &p.role {
            Some(v) => check.optional("role", v.as_deref(), 200),
            None => current.role.clone(),
        },
        preferred_channel: p.preferred_channel.unwrap_or(before.preferred_channel),
        email_opt_in: p.email_opt_in.unwrap_or(current.email_opt_in),
        sms_opt_in: p.sms_opt_in.unwrap_or(current.sms_opt_in),
        whatsapp_opt_in: p.whatsapp_opt_in.unwrap_or(current.whatsapp_opt_in),
        notes: match &p.notes {
            Some(v) => check.long("notes", v.as_deref(), 20_000),
            None => current.notes.clone(),
        },
        tags: match &p.tags {
            Some(t) => check.tags(t),
            None => current.tags.clone(),
        },
        custom: Value::Object(custom),
        links: match &p.links {
            Some(l) => check_links(&mut check, l),
            None => current.links.clone(),
        },
    };
    if checked.organisation_id != current.organisation_id {
        check_organisation(tx, &mut check, checked.organisation_id).await?;
    }
    check.finish()?;

    let unchanged = checked.organisation_id == current.organisation_id
        && checked.name == current.name
        && checked.emails == current.emails
        && checked.phones == current.phones
        && checked.role == current.role
        && checked.preferred_channel == before.preferred_channel
        && checked.email_opt_in == current.email_opt_in
        && checked.sms_opt_in == current.sms_opt_in
        && checked.whatsapp_opt_in == current.whatsapp_opt_in
        && checked.notes == current.notes
        && checked.tags == current.tags
        && checked.links == current.links
        && checked.custom == current.custom;
    if unchanged {
        return Ok(before);
    }

    core_db::contacts::update(tx, id, checked.values(), ctx.user_id()).await?;
    let after = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "contact.updated",
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

pub async fn delete(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::ContactWrite)?;
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
    core_db::contacts::soft_delete(tx, id, ctx.user_id()).await?;
    record(
        tx,
        ctx,
        Change {
            topic: "contact.deleted",
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
pub struct ContactDuplicateQuery {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub emails: Vec<String>,
    #[serde(default)]
    pub phones: Vec<String>,
    /// The record being edited, which is not its own duplicate.
    pub except: Option<Uuid>,
}

/// Contacts that may be the one being entered: the same email or phone
/// number, or a similar name.
pub async fn duplicates(
    state: &AppState,
    ctx: &Ctx,
    q: &ContactDuplicateQuery,
) -> ApiResult<Vec<Duplicate>> {
    ctx.authorize(Permission::ContactRead)?;
    let name = q.name.trim().to_lowercase();
    let emails: Vec<String> = q
        .emails
        .iter()
        .map(|e| email_key(e))
        .filter(|e| !e.is_empty())
        .collect();
    let keys = phone_keys(&q.phones);
    if name.is_empty() && emails.is_empty() && keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut tx = begin(state, ctx).await?;
    let rows = core_db::contacts::similar(
        &mut tx,
        &name,
        &emails,
        &keys,
        super::crm::SIMILAR_NAME,
        q.except,
        super::crm::MAX_CANDIDATES,
    )
    .await?;
    tx.commit().await?;
    Ok(rows
        .into_iter()
        .map(|c| {
            let mut reasons: Vec<String> =
                c.emails.iter().map(|e| format!("same email {e}")).collect();
            reasons.extend(c.phone_keys.iter().map(|k| {
                format!(
                    "same phone number (ending {})",
                    &k[k.len().saturating_sub(4)..]
                )
            }));
            reasons.extend(super::crm::name_reason(c.similarity));
            Duplicate {
                id: c.id,
                name: c.name,
                reasons,
            }
        })
        .collect())
}
