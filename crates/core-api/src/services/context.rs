//! The context tree rules and templates see: `workspace`, the object under
//! its entity's name (`contact`, `talent`, `thing`…), its state `before` a
//! change, and the triggering `event`. Documented by [`variables`] so the
//! template editor can list them.

use chrono::Utc;
use core_db::Tx;
use core_engines::fields::FieldType;
use core_engines::templates::Options;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::AppState;
use crate::error::{ApiError, ApiResult};

/// The workspace part of the context, and how to format dates and money for it.
pub(crate) async fn workspace(tx: &mut Tx, state: &AppState) -> ApiResult<(Value, Options)> {
    let id = sqlx::query_scalar!(r#"SELECT app_workspace_id() AS "id!""#)
        .fetch_one(&mut **tx)
        .await?;
    let w = core_db::workspaces::get(tx, core_domain::WorkspaceId(id))
        .await?
        .ok_or(ApiError::NotFound)?;
    let options = Options {
        locale: w.locale.clone(),
        timezone: w.default_timezone.parse().unwrap_or(chrono_tz::Tz::UTC),
    };
    let json = json!({
        "id": w.id,
        "name": w.name,
        "slug": w.slug,
        "country": w.country.trim(),
        "currency": w.default_currency.trim(),
        "timezone": w.default_timezone,
        "locale": w.locale,
        "app_url": state.config.public_base_url.as_str(),
    });
    Ok((json, options))
}

/// Where a record's page is in the web app.
fn app_path(entity: &str, id: &str) -> String {
    match entity {
        "talent" => format!("/crm/talent/{id}"),
        "organisation" => format!("/crm/organisations/{id}"),
        "contact" => format!("/crm/contacts/{id}"),
        "enquiry" => format!("/enquiries/{id}"),
        "booking" => format!("/bookings/{id}"),
        "task" => "/tasks".to_owned(),
        _ => format!("/things/{id}"),
    }
}

/// Adds the object's link in the web app.
pub(crate) fn with_url(state: &AppState, entity: &str, mut object: Value) -> Value {
    // A quote is shown on its booking's page.
    let (entity, key) = match entity {
        "quote" => ("booking", "booking_id"),
        e => (e, "id"),
    };
    if let Some(id) = object.get(key).and_then(Value::as_str) {
        let url = state.config.app_url(&app_path(entity, id), &[]);
        if let Some(obj) = object.as_object_mut() {
            obj.insert("url".into(), Value::String(url));
        }
    }
    object
}

/// The entities rules and templates can be about.
pub(crate) fn is_entity(entity: &str) -> bool {
    entity == super::things::ENTITY || super::records::is_crm(entity) || WORK.contains(&entity)
}

/// The M4 records rules and templates can be about.
pub(crate) const WORK: &[&str] = &["enquiry", "booking", "quote"];

/// A record as rules and templates see it, with its link.
pub(crate) async fn load_object(
    tx: &mut Tx,
    state: &AppState,
    entity: &str,
    id: uuid::Uuid,
) -> ApiResult<Option<Value>> {
    let json = if entity == super::things::ENTITY {
        super::things::load(tx, id)
            .await?
            .map(|v| super::things::to_json(&v))
    } else if entity == super::enquiries::ENTITY {
        super::enquiries::load(tx, id)
            .await?
            .map(|v| super::enquiries::to_json(&v))
    } else if entity == super::bookings::ENTITY {
        super::bookings::load(tx, id)
            .await?
            .map(|v| super::bookings::to_json(&v))
    } else if entity == super::quotes::ENTITY {
        super::quotes::load(tx, id)
            .await?
            .map(|v| super::quotes::to_json(&v))
    } else {
        super::records::load_json(tx, entity, id).await?
    };
    Ok(json.map(|j| with_url(state, entity, j)))
}

/// The permission needed to read an entity's records.
pub(crate) fn read_permission(entity: &str) -> core_domain::Permission {
    use core_domain::Permission as P;
    match entity {
        "talent" => P::TalentRead,
        "organisation" => P::OrganisationRead,
        "contact" => P::ContactRead,
        "enquiry" => P::EnquiryRead,
        "booking" => P::BookingRead,
        "quote" => P::QuoteRead,
        _ => P::ThingRead,
    }
}

pub(crate) fn build(
    workspace: Value,
    entity: &str,
    object: Option<Value>,
    before: Option<Value>,
    event: Option<Value>,
) -> Value {
    let mut tree = Map::new();
    tree.insert("workspace".into(), workspace);
    tree.insert(entity.to_owned(), object.unwrap_or(Value::Null));
    tree.insert("before".into(), before.unwrap_or(Value::Null));
    tree.insert("event".into(), event.unwrap_or(Value::Null));
    tree.insert("now".into(), json!(Utc::now()));
    Value::Object(tree)
}

/// Plausible values for each custom field, for previews without a real record.
async fn sample_custom(tx: &mut Tx, entity: &str) -> ApiResult<Map<String, Value>> {
    let defs = core_db::fields::list(tx, Some(entity), false).await?;
    let mut custom = Map::new();
    for d in &defs {
        let Some(def) = super::fields::to_engine(d) else {
            continue;
        };
        let value = match def.field_type {
            FieldType::Text | FieldType::LongText => json!(format!("Sample {}", def.label)),
            FieldType::Number => json!(42),
            FieldType::Money => json!({
                "minor": 125_000,
                "currency": def.validation.currency.clone().unwrap_or_else(|| "GBP".into())
            }),
            FieldType::Date => json!("2027-03-07"),
            FieldType::Datetime => json!("2027-03-07T19:30:00Z"),
            FieldType::Boolean => json!(true),
            FieldType::Select => json!(def.validation.options.first().map(|o| o.value.clone())),
            FieldType::MultiSelect => {
                json!(
                    def.validation
                        .options
                        .iter()
                        .take(2)
                        .map(|o| o.value.clone())
                        .collect::<Vec<_>>()
                )
            }
            FieldType::Url => json!("https://example.com"),
            FieldType::Email => json!("someone@example.com"),
            FieldType::Phone => json!("+44 20 7946 0000"),
            FieldType::File | FieldType::Relation => json!(core_domain::ids::new_id()),
        };
        custom.insert(def.key.clone(), value);
    }
    Ok(custom)
}

/// A made-up record of the entity, for previews and dry runs without a real one.
pub(crate) async fn sample(tx: &mut Tx, state: &AppState, entity: &str) -> ApiResult<Value> {
    let custom = sample_custom(tx, entity).await?;
    let id = core_domain::ids::new_id();
    let now = Utc::now();
    let base = match entity {
        "talent" => json!({
            "id": id, "kind": "person", "name": "Sample Artist", "legal_name": "Sam Artist",
            "talent_type": "DJ", "base_location": "London",
            "default_fee": { "minor": 150_000, "currency": "GBP" }, "commission_percent": 15.0,
            "status": "active", "tags": ["sample"], "custom": custom, "version": 1,
            "created_at": now, "updated_at": now,
        }),
        "organisation" => json!({
            "id": id, "name": "Sample Venue", "org_type": "venue",
            "addresses": [{ "line1": "1 High Street", "city": "London", "postcode": "N1 1AA", "country": "GB" }],
            "tax_id": "GB123456789", "default_currency": "GBP", "payment_terms_days": 30,
            "tags": ["sample"], "custom": custom, "version": 1, "created_at": now, "updated_at": now,
        }),
        "contact" => json!({
            "id": id, "name": "Sam Sample", "organisation_id": null, "organisation_name": "Sample Venue",
            "email": "sam@example.com", "emails": ["sam@example.com"],
            "phone": "+44 20 7946 0000", "phones": ["+44 20 7946 0000"], "role": "Booker",
            "preferred_channel": "email", "email_opt_in": true, "sms_opt_in": false,
            "whatsapp_opt_in": false, "tags": ["sample"], "custom": custom, "version": 1,
            "created_at": now, "updated_at": now,
        }),
        "enquiry" => json!({
            "id": id, "title": "Summer party", "source": "manual",
            "talent": [{ "id": core_domain::ids::new_id(), "name": "Sample Artist" }],
            "organisation_name": "Sample Venue", "contact_name": "Sam Sample",
            "starts_on": "2027-07-03", "budget": { "minor": 250_000, "currency": "GBP" },
            "stage": "new", "stage_name": "New", "stage_outcome": "open", "stage_entered_at": now,
            "tags": ["sample"], "custom": custom, "version": 1, "created_at": now, "updated_at": now,
        }),
        "booking" => json!({
            "id": id, "title": "Summer party", "status": "hold", "status_changed_at": now,
            "talent": [{ "id": core_domain::ids::new_id(), "name": "Sample Artist" }],
            "organisation_name": "Sample Venue", "contact_name": "Sam Sample", "venue_name": "Sample Venue",
            "timezone": "Europe/London", "starts_at": "2027-07-03T19:00:00Z", "ends_at": "2027-07-03T23:00:00Z",
            "all_day": false, "schedule": [{ "label": "Soundcheck", "at": "2027-07-03T17:00:00Z" }],
            "hold_expires_at": now + chrono::Duration::days(7),
            "fee": { "minor": 250_000, "currency": "GBP" }, "deposit_percent": 50.0,
            "tags": ["sample"], "custom": custom, "version": 1, "created_at": now, "updated_at": now,
        }),
        "quote" => json!({
            "id": id, "booking_id": core_domain::ids::new_id(), "booking_title": "Summer party",
            "number": 7, "revision": 1, "reference": "Q-0007", "status": "sent", "currency": "GBP",
            "line_items": [{ "description": "Fee: Sample Artist", "quantity": 1.0, "unit_minor": 250_000,
                             "amount_minor": 250_000 }],
            "total": { "minor": 250_000, "currency": "GBP" }, "valid_until": "2027-06-01",
            "sent_at": now, "sent_to": "sam@example.com", "version": 1, "created_at": now, "updated_at": now,
        }),
        _ => json!({
            "id": id, "name": "Sample record", "pipeline_id": null, "stage": "new",
            "stage_entered_at": now, "due_at": now + chrono::Duration::days(14),
            "custom": custom, "version": 1, "created_at": now, "updated_at": now,
        }),
    };
    Ok(with_url(state, entity, base))
}

/// One variable a template can use.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Variable {
    pub path: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field_type: Option<String>,
}

fn var(path: &str, description: &str) -> Variable {
    Variable {
        path: path.into(),
        description: description.into(),
        field_type: None,
    }
}

/// Every variable available to templates about `entity`, including its
/// custom fields.
pub(crate) async fn variables(tx: &mut Tx, entity: &str) -> ApiResult<Vec<Variable>> {
    let mut out = vec![
        var("workspace.name", "The workspace's name"),
        var("workspace.slug", "The workspace's short name"),
        var("workspace.country", "ISO country code"),
        var("workspace.currency", "Default currency code"),
        var(
            "workspace.timezone",
            "Default timezone; dates are shown in it",
        ),
        var("workspace.app_url", "Link to the web app"),
        var("now", "When the message is rendered"),
        var(
            "event.topic",
            "The event that triggered the rule, e.g. contact.created",
        ),
        var("event.at", "When the event happened"),
    ];
    let core: &[(&str, &str)] = match entity {
        "thing" => &[
            ("id", "The record's id"),
            ("name", "Name"),
            ("stage", "Current stage key"),
            ("stage_entered_at", "When it entered the current stage"),
            ("due_at", "Due date and time"),
        ],
        "talent" => &[
            ("id", "The record's id"),
            ("name", "Stage or display name"),
            ("kind", "person, act, space or equipment"),
            ("legal_name", "Legal name"),
            ("talent_type", "Type (solo, band, DJ…)"),
            ("bio", "Bio"),
            ("base_location", "Where they are based"),
            ("default_fee", "Default fee (use the money filter)"),
            ("commission_percent", "Commission percentage"),
            ("status", "active or inactive"),
            ("tags", "Tags"),
        ],
        "organisation" => &[
            ("id", "The record's id"),
            ("name", "Name"),
            ("org_type", "Type (venue, promoter…)"),
            ("addresses", "Addresses (use the address filter on one)"),
            ("tax_id", "Tax or VAT number"),
            ("default_currency", "Default currency"),
            ("payment_terms_days", "Payment terms in days"),
            ("tags", "Tags"),
        ],
        "contact" => &[
            ("id", "The record's id"),
            ("name", "Name"),
            ("email", "Primary email"),
            ("emails", "Every email address"),
            ("phone", "Primary phone number"),
            ("role", "Role or job title"),
            ("organisation_name", "Their organisation"),
            ("preferred_channel", "email, phone, sms or whatsapp"),
            ("tags", "Tags"),
        ],
        "enquiry" => &[
            ("id", "The record's id"),
            ("title", "What is asked for"),
            ("source", "manual, email, form or portal"),
            ("talent", "The talent asked for (a list; each has a name)"),
            ("organisation_name", "The organisation"),
            ("contact_name", "The contact"),
            ("starts_on", "First date (use the date filter)"),
            ("budget", "Budget (use the money filter)"),
            ("stage", "Current stage key"),
            ("stage_name", "Current stage"),
            ("tags", "Tags"),
        ],
        "booking" => &[
            ("id", "The record's id"),
            ("title", "Event name"),
            (
                "status",
                "hold, pending, confirmed, cancelled, completed or expired",
            ),
            ("talent", "The talent booked (a list; each has a name)"),
            ("organisation_name", "The organisation"),
            ("contact_name", "The contact"),
            ("venue_name", "The venue"),
            (
                "starts_at",
                "Start (use the datetime filter with booking.timezone)",
            ),
            ("ends_at", "End"),
            ("timezone", "The venue's timezone"),
            ("hold_expires_at", "When the hold lapses"),
            ("fee", "Fee (use the money filter)"),
            ("deposit_percent", "Deposit percentage"),
            ("tags", "Tags"),
        ],
        "quote" => &[
            ("id", "The record's id"),
            ("reference", "Q-0007"),
            ("booking_title", "The booking it prices"),
            (
                "status",
                "draft, sent, viewed, accepted, declined, expired or withdrawn",
            ),
            ("total", "Total (use the money filter)"),
            ("valid_until", "Last day it can be accepted"),
            ("sent_to", "Who it was sent to"),
        ],
        _ => &[],
    };
    if !core.is_empty() {
        for (field, description) in core {
            out.push(var(&format!("{entity}.{field}"), description));
        }
        out.push(var(
            &format!("{entity}.url"),
            "Link to the record in the web app",
        ));
        out.push(var(&format!("{entity}.created_at"), "When it was created"));
        out.push(var(
            "before.name",
            "Name before the change (update rules only)",
        ));
        if entity == super::things::ENTITY || entity == super::enquiries::ENTITY {
            out.push(var(
                "before.stage",
                "Stage before the change (update rules only)",
            ));
        }
        for d in core_db::fields::list(tx, Some(entity), false).await? {
            out.push(Variable {
                path: format!("{entity}.custom.{}", d.key),
                description: d.label,
                field_type: Some(d.field_type),
            });
        }
    }
    Ok(out)
}
