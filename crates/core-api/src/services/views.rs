//! Saved list views (columns, sort and filters under a name) and the
//! schema every form and list is drawn from.

use chrono::{DateTime, Utc};
use core_db::views::View;
use core_domain::{BookingStatus, Ctx, Permission, TalentKind};
use core_engines::fields::{FieldType, SelectOption};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use super::search::authorize_read;
use super::{begin, check_version, required_text};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

/// The largest saved view configuration, in bytes of JSON.
const MAX_CONFIG_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SavedView {
    pub id: Uuid,
    pub entity: String,
    pub name: String,
    /// Shared with everyone in the workspace, rather than the caller's own.
    pub shared: bool,
    /// Columns, sort, filters: whatever the list needs to restore itself.
    #[schema(value_type = Object)]
    pub config: Value,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<View> for SavedView {
    fn from(v: View) -> Self {
        Self {
            id: v.id,
            entity: v.entity,
            name: v.name,
            shared: v.owner_id.is_none(),
            config: v.config,
            version: v.version,
            created_at: v.created_at,
            updated_at: v.updated_at,
        }
    }
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SavedViewInput {
    pub entity: String,
    pub name: String,
    /// Share with the whole workspace (needs `config:write`).
    #[serde(default)]
    pub shared: bool,
    #[schema(value_type = Object)]
    pub config: Value,
}

fn check_config(config: &Value) -> ApiResult<()> {
    if !config.is_object() {
        return Err(ApiError::invalid("config", "must be a JSON object"));
    }
    if config.to_string().len() > MAX_CONFIG_BYTES {
        return Err(ApiError::invalid("config", "is too large"));
    }
    Ok(())
}

/// Who may own a new or changed view: the caller for a personal one (people
/// only), nobody for a shared one (admins only).
fn owner(ctx: &Ctx, shared: bool) -> ApiResult<Option<core_domain::UserId>> {
    if shared {
        ctx.authorize(Permission::ConfigWrite)?;
        Ok(None)
    } else {
        ctx.user_id()
            .map(Some)
            .ok_or_else(|| ApiError::invalid("shared", "an API key can only save shared views"))
    }
}

pub async fn list(state: &AppState, ctx: &Ctx, entity: &str) -> ApiResult<Vec<SavedView>> {
    authorize_read(ctx, entity)?;
    let mut tx = begin(state, ctx).await?;
    let rows = core_db::views::list(&mut tx, entity).await?;
    tx.commit().await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

pub async fn create(state: &AppState, ctx: &Ctx, input: SavedViewInput) -> ApiResult<SavedView> {
    authorize_read(ctx, &input.entity)?;
    let name = required_text("name", &input.name, 100)?;
    check_config(&input.config)?;
    let owner = owner(ctx, input.shared)?;
    let mut tx = begin(state, ctx).await?;
    let id = core_domain::ids::new_id();
    core_db::views::insert(
        &mut tx,
        id,
        &input.entity,
        &name,
        owner,
        &input.config,
        ctx.user_id(),
    )
    .await?;
    let view = core_db::views::get_for_update(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(view.into())
}

pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    input: SavedViewInput,
    if_match: Option<i64>,
) -> ApiResult<SavedView> {
    authorize_read(ctx, &input.entity)?;
    let name = required_text("name", &input.name, 100)?;
    check_config(&input.config)?;
    let mut tx = begin(state, ctx).await?;
    let current = core_db::views::get_for_update(&mut tx, id)
        .await?
        .filter(|v| v.entity == input.entity)
        .ok_or(ApiError::NotFound)?;
    check_version(current.version, if_match)?;
    if current.owner_id.is_none() {
        ctx.authorize(Permission::ConfigWrite)?;
    }
    let owner = owner(ctx, input.shared)?;
    core_db::views::update(&mut tx, id, &name, owner, &input.config).await?;
    let view = core_db::views::get_for_update(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(view.into())
}

pub async fn delete(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    let mut tx = begin(state, ctx).await?;
    let current = core_db::views::get_for_update(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    authorize_read(ctx, &current.entity)?;
    if current.owner_id.is_none() {
        ctx.authorize(Permission::ConfigWrite)?;
    }
    core_db::views::soft_delete(&mut tx, id).await?;
    tx.commit().await?;
    Ok(())
}

// Schema ---------------------------------------------------------------------------

/// A field's kind, as forms and lists draw it: the custom field types plus
/// the core fields' own shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SchemaFieldType {
    Text,
    LongText,
    Number,
    Money,
    Date,
    Datetime,
    Boolean,
    Select,
    MultiSelect,
    Url,
    Email,
    Phone,
    File,
    Relation,
    /// 0 to 100.
    Percent,
    /// A list of free-form tags.
    Tags,
    /// A list of email addresses.
    EmailList,
    /// A list of phone numbers.
    PhoneList,
    /// A list of postal addresses.
    AddressList,
    /// An ISO 4217 code.
    Currency,
}

impl From<FieldType> for SchemaFieldType {
    fn from(t: FieldType) -> Self {
        match t {
            FieldType::Text => Self::Text,
            FieldType::LongText => Self::LongText,
            FieldType::Number => Self::Number,
            FieldType::Money => Self::Money,
            FieldType::Date => Self::Date,
            FieldType::Datetime => Self::Datetime,
            FieldType::Boolean => Self::Boolean,
            FieldType::Select => Self::Select,
            FieldType::MultiSelect => Self::MultiSelect,
            FieldType::Url => Self::Url,
            FieldType::Email => Self::Email,
            FieldType::Phone => Self::Phone,
            FieldType::File => Self::File,
            FieldType::Relation => Self::Relation,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SchemaField {
    /// The key in the record: `name`, `emails`, or `custom.<key>`.
    pub key: String,
    pub label: String,
    pub field_type: SchemaFieldType,
    /// Built in, rather than a custom field.
    pub core: bool,
    pub required: bool,
    /// Shown but not edited (counts, links kept elsewhere).
    pub read_only: bool,
    /// Shown as a list column unless a view says otherwise.
    pub in_list: bool,
    /// What to pass as a list's `sort` to sort by it; absent if it cannot be.
    pub sort: Option<String>,
    /// Choices for selects.
    pub options: Vec<SelectOption>,
    /// The record type a relation points at.
    pub relation_entity: Option<String>,
    /// The fixed currency of a money field.
    pub currency: Option<String>,
    /// The form section it belongs in.
    pub group: Option<String>,
    pub help_text: Option<String>,
    /// For custom fields: the definition, as `GET /custom-fields/{id}` gives it.
    pub field_id: Option<Uuid>,
    /// For custom fields: the default for new records.
    #[schema(value_type = Option<Object>)]
    pub default_value: Option<Value>,
    /// For custom fields: limits the server checks (regex, min, max, lengths).
    #[schema(value_type = Option<Object>)]
    pub validation: Option<Value>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EntitySchema {
    pub entity: String,
    pub label: String,
    pub label_plural: String,
    /// The caller may create and change these records.
    pub can_write: bool,
    /// Core fields first, then custom fields in their order.
    pub fields: Vec<SchemaField>,
}

struct Core<'a> {
    key: &'a str,
    label: &'a str,
    t: SchemaFieldType,
    required: bool,
    in_list: bool,
    sortable: bool,
}

const fn core<'a>(key: &'a str, label: &'a str, t: SchemaFieldType) -> Core<'a> {
    Core {
        key,
        label,
        t,
        required: false,
        in_list: false,
        sortable: false,
    }
}

impl Core<'_> {
    const fn required(mut self) -> Self {
        self.required = true;
        self
    }
    const fn listed(mut self) -> Self {
        self.in_list = true;
        self
    }
    const fn sorted(mut self) -> Self {
        self.sortable = true;
        self
    }
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

fn option(value: &str, label: &str) -> SelectOption {
    SelectOption {
        value: value.into(),
        label: label.into(),
    }
}

fn core_fields(entity: &str) -> Vec<(Core<'static>, Vec<SelectOption>, Option<&'static str>)> {
    use SchemaFieldType as T;
    match entity {
        "talent" => vec![
            (
                core("name", "Name", T::Text).required().listed().sorted(),
                vec![],
                None,
            ),
            (
                core("kind", "Kind", T::Select).required().listed().sorted(),
                TalentKind::ALL
                    .iter()
                    .map(|k| {
                        let label = match k {
                            TalentKind::Person => "Person",
                            TalentKind::Act => "Act",
                            TalentKind::Space => "Space",
                            TalentKind::Equipment => "Equipment",
                        };
                        option(k.as_str(), label)
                    })
                    .collect(),
                None,
            ),
            (
                core("talent_type", "Type", T::Text).listed().sorted(),
                vec![],
                None,
            ),
            (
                core("legal_name", "Legal name", T::Text).sorted(),
                vec![],
                None,
            ),
            (
                core("base_location", "Based in", T::Text).listed().sorted(),
                vec![],
                None,
            ),
            (
                core("default_fee", "Default fee", T::Money)
                    .listed()
                    .sorted(),
                vec![],
                None,
            ),
            (
                core("commission_percent", "Commission %", T::Percent).sorted(),
                vec![],
                None,
            ),
            (
                core("status", "Status", T::Select)
                    .required()
                    .listed()
                    .sorted(),
                vec![option("active", "Active"), option("inactive", "Inactive")],
                None,
            ),
            (core("tags", "Tags", T::Tags).listed(), vec![], None),
            (core("bio", "Bio", T::LongText), vec![], None),
        ],
        "organisation" => vec![
            (
                core("name", "Name", T::Text).required().listed().sorted(),
                vec![],
                None,
            ),
            (
                core("org_type", "Type", T::Text).listed().sorted(),
                vec![],
                None,
            ),
            (
                core("contact_count", "Contacts", T::Number)
                    .listed()
                    .sorted(),
                vec![],
                None,
            ),
            (
                core("tax_id", "Tax / VAT number", T::Text).sorted(),
                vec![],
                None,
            ),
            (
                core("default_currency", "Currency", T::Currency).sorted(),
                vec![],
                None,
            ),
            (
                core("payment_terms_days", "Payment terms (days)", T::Number).sorted(),
                vec![],
                None,
            ),
            (core("addresses", "Addresses", T::AddressList), vec![], None),
            (core("tags", "Tags", T::Tags).listed(), vec![], None),
            (core("notes", "Notes", T::LongText), vec![], None),
        ],
        "enquiry" => vec![
            (
                core("title", "Title", T::Text).required().listed().sorted(),
                vec![],
                None,
            ),
            (
                core("starts_on", "Date", T::Date).listed().sorted(),
                vec![],
                None,
            ),
            (
                core("budget", "Budget", T::Money).listed().sorted(),
                vec![],
                None,
            ),
            (
                core("organisation_id", "Organisation", T::Relation).listed(),
                vec![],
                Some("organisation"),
            ),
            (
                core("contact_id", "Contact", T::Relation),
                vec![],
                Some("contact"),
            ),
            (core("tags", "Tags", T::Tags).listed(), vec![], None),
            (core("notes", "Notes", T::LongText), vec![], None),
        ],
        "booking" => vec![
            (
                core("title", "Title", T::Text).required().listed().sorted(),
                vec![],
                None,
            ),
            (
                core("starts_at", "Starts", T::Datetime)
                    .required()
                    .listed()
                    .sorted(),
                vec![],
                None,
            ),
            (
                core("ends_at", "Ends", T::Datetime).required(),
                vec![],
                None,
            ),
            (
                core("status", "Status", T::Select).listed().sorted(),
                BookingStatus::ALL
                    .iter()
                    .map(|s| option(s.as_str(), &capitalise(s.as_str())))
                    .collect(),
                None,
            ),
            (core("fee", "Fee", T::Money).listed().sorted(), vec![], None),
            (
                core("organisation_id", "Organisation", T::Relation).listed(),
                vec![],
                Some("organisation"),
            ),
            (
                core("contact_id", "Contact", T::Relation),
                vec![],
                Some("contact"),
            ),
            (core("tags", "Tags", T::Tags).listed(), vec![], None),
            (core("notes", "Notes", T::LongText), vec![], None),
        ],
        _ => vec![
            (
                core("name", "Name", T::Text).required().listed().sorted(),
                vec![],
                None,
            ),
            (
                core("organisation_id", "Organisation", T::Relation)
                    .listed()
                    .sorted(),
                vec![],
                Some("organisation"),
            ),
            (
                core("role", "Role", T::Text).listed().sorted(),
                vec![],
                None,
            ),
            (
                core("emails", "Email", T::EmailList).listed().sorted(),
                vec![],
                None,
            ),
            (
                core("phones", "Phone", T::PhoneList).listed().sorted(),
                vec![],
                None,
            ),
            (
                core("preferred_channel", "Preferred channel", T::Select).sorted(),
                vec![
                    option("email", "Email"),
                    option("phone", "Phone"),
                    option("sms", "SMS"),
                    option("whatsapp", "WhatsApp"),
                ],
                None,
            ),
            (
                core("email_opt_in", "Email marketing", T::Boolean),
                vec![],
                None,
            ),
            (
                core("sms_opt_in", "SMS marketing", T::Boolean),
                vec![],
                None,
            ),
            (
                core("whatsapp_opt_in", "WhatsApp marketing", T::Boolean),
                vec![],
                None,
            ),
            (core("tags", "Tags", T::Tags).listed(), vec![], None),
            (core("notes", "Notes", T::LongText), vec![], None),
        ],
    }
}

fn write_permission(entity: &str) -> Permission {
    match entity {
        "talent" => Permission::TalentWrite,
        "organisation" => Permission::OrganisationWrite,
        "enquiry" => Permission::EnquiryWrite,
        "booking" => Permission::BookingWrite,
        _ => Permission::ContactWrite,
    }
}

/// An entity's fields, core and custom, as forms and lists draw them:
/// without the ones OnHold's team hid for the workspace, nor commission
/// where it takes none. Hiding a field never drops a required one.
pub async fn schema(state: &AppState, ctx: &Ctx, entity: &str) -> ApiResult<EntitySchema> {
    let mut schema = full_schema(state, ctx, entity).await?;
    let mut tx = begin(state, ctx).await?;
    let w = core_db::workspaces::get(&mut tx, ctx.workspace_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    let commission = w.extras.iter().any(|e| e == "commission");
    schema.fields.retain(|f| {
        let hidden = w.hidden.contains(&format!("field:{entity}.{}", f.key))
            || (f.key == "commission_percent" && !commission);
        f.required || !hidden
    });
    Ok(schema)
}

/// Every field of an entity, hidden or not.
pub async fn full_schema(state: &AppState, ctx: &Ctx, entity: &str) -> ApiResult<EntitySchema> {
    authorize_read(ctx, entity)?;
    let (label, plural) = match entity {
        "talent" => ("Talent", "Talent"),
        "organisation" => ("Organisation", "Organisations"),
        "enquiry" => ("Enquiry", "Enquiries"),
        "booking" => ("Booking", "Bookings"),
        _ => ("Contact", "Contacts"),
    };
    let mut fields: Vec<SchemaField> = core_fields(entity)
        .into_iter()
        .map(|(c, options, relation)| SchemaField {
            key: c.key.into(),
            label: c.label.into(),
            field_type: c.t,
            core: true,
            required: c.required,
            // A booking's status moves through its own actions, not the form.
            read_only: c.key == "contact_count" || (entity == "booking" && c.key == "status"),
            in_list: c.in_list,
            sort: if c.sortable {
                sort_key(entity, c.key)
            } else {
                None
            },
            options,
            relation_entity: relation.map(str::to_owned),
            currency: None,
            group: None,
            help_text: None,
            field_id: None,
            default_value: None,
            validation: None,
        })
        .collect();
    let mut tx = begin(state, ctx).await?;
    let defs = core_db::fields::list(&mut tx, Some(entity), false).await?;
    tx.commit().await?;
    for d in defs {
        let Some(def) = super::fields::to_engine(&d) else {
            continue;
        };
        fields.push(SchemaField {
            key: format!("custom.{}", d.key),
            label: d.label,
            field_type: def.field_type.into(),
            core: false,
            required: d.required,
            read_only: false,
            // A field someone just added is one they want to see.
            in_list: true,
            sort: (!matches!(def.field_type, FieldType::MultiSelect | FieldType::File))
                .then(|| format!("custom.{}", d.key)),
            options: def.validation.options.clone(),
            relation_entity: def.validation.entity.clone(),
            currency: def.validation.currency.clone(),
            group: d.field_group,
            help_text: d.help_text,
            field_id: Some(d.id),
            default_value: d.default_value,
            validation: Some(d.validation),
        });
    }
    Ok(EntitySchema {
        entity: entity.into(),
        label: label.into(),
        label_plural: plural.into(),
        can_write: ctx.permissions.contains(&write_permission(entity)),
        fields,
    })
}

/// The column a schema key sorts by, as the list endpoints spell it.
fn sort_key(entity: &str, key: &str) -> Option<String> {
    if key.starts_with("custom.") {
        return Some(key.to_owned());
    }
    let mapped = match (entity, key) {
        ("talent", "commission_percent") => "commission",
        ("contact", "organisation_id") => "organisation",
        ("contact", "emails") => "email",
        ("contact", "phones") => "phone",
        (_, k) => k,
    };
    let sorts = match entity {
        "talent" => core_db::talent::SORTS,
        "organisation" => core_db::organisations::SORTS,
        "enquiry" => core_db::enquiries::SORTS,
        "booking" => core_db::bookings::SORTS,
        _ => core_db::contacts::SORTS,
    };
    sorts.contains(&mapped).then(|| mapped.to_owned())
}
