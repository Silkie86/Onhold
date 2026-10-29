//! Pitching, sending: a pitch template filled in for a person at an
//! organisation and what is pitched to them, ready to send from OnHold
//! (through the member's own mailbox, so it logs itself), to open in their
//! email app, or to copy; and how each member usually sends one.

use std::collections::BTreeSet;

use chrono::Utc;
use core_db::Tx;
use core_domain::{Ctx, Permission};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::begin;
use super::inbox::{self, Compose};
use super::organisations::ItemInput;
use super::pitches::{self, Changed, Parts, PitchChannel, PitchKind, PitchView};
use super::pitching::{ItemKind, PitchSendWith, PitchTemplate, settings_in};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

/// What a pitch template can say, filled in for each pitch.
pub const PLACEHOLDERS: &[(&str, &str)] = &[
    ("firstName", "The person's first name"),
    ("lastName", "The person's last name"),
    ("fullName", "The person's full name"),
    ("organisation", "The organisation's name"),
    ("city", "The organisation's city"),
    ("country", "The organisation's country"),
    ("item", "What is pitched: the talent or project's name"),
    (
        "link",
        "The first web address among the talent's own fields",
    ),
    ("yourName", "Your name"),
    ("workspace", "The workspace's name"),
];

/// What each placeholder is filled with here, `None` where there is nothing.
struct Values(Vec<(&'static str, Option<String>)>);

impl Values {
    fn get(&self, name: &str) -> Option<Option<&str>> {
        self.0
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.as_deref())
    }
}

/// Fills `{placeholders}` in. One with nothing to fill it is left as it is,
/// and noted in `missing`; braces around anything else are left alone.
fn fill(text: &str, values: &Values, missing: &mut BTreeSet<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let name_len = after
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(after.len());
        let name = &after[..name_len];
        match (after[name_len..].starts_with('}'), values.get(name)) {
            (true, Some(Some(v))) => {
                out.push_str(v);
                rest = &after[name_len + 1..];
            }
            (true, Some(None)) => {
                missing.insert(name.to_owned());
                out.push('{');
                out.push_str(name);
                out.push('}');
                rest = &after[name_len + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The placeholders still in `text` that are known but have no value.
fn unfilled(text: &str) -> BTreeSet<String> {
    let mut missing = BTreeSet::new();
    let none = Values(PLACEHOLDERS.iter().map(|(n, _)| (*n, None)).collect());
    fill(text, &none, &mut missing);
    missing
}

/// The first web address among a talent's own fields.
fn first_link(custom: &serde_json::Value) -> Option<String> {
    let mut found = None;
    if let Some(map) = custom.as_object() {
        for v in map.values() {
            if let Some(s) = v.as_str().map(str::trim)
                && (s.starts_with("https://") || s.starts_with("http://"))
                && !s.contains(char::is_whitespace)
            {
                found = Some(s.to_owned());
                break;
            }
        }
    }
    found
}

/// The organisation, checked: there, and not marked do not contact.
async fn organisation(tx: &mut Tx, id: Uuid) -> ApiResult<core_db::organisations::Organisation> {
    let org = core_db::organisations::get(tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if org.do_not_contact {
        return Err(ApiError::conflict(
            "do_not_contact",
            "this organisation is marked do not contact",
        ));
    }
    Ok(org)
}

/// The person, who must be at the organisation.
async fn person(
    tx: &mut Tx,
    organisation: Uuid,
    id: Uuid,
) -> ApiResult<core_db::contacts::Contact> {
    match core_db::contacts::get(tx, id).await? {
        Some(c) if c.organisation_id == Some(organisation) => Ok(c),
        Some(_) => Err(ApiError::invalid(
            "contact_id",
            "is not at this organisation",
        )),
        None => Err(ApiError::invalid("contact_id", "there is no such contact")),
    }
}

/// The values for a pitch to `contact` at `org` about `item`.
async fn values(
    tx: &mut Tx,
    ctx: &Ctx,
    org: &core_db::organisations::Organisation,
    contact: Option<&core_db::contacts::Contact>,
    item: Option<ItemInput>,
) -> ApiResult<Values> {
    let full = contact.map(|c| c.name.trim().to_owned());
    let (first, last) = match full.as_deref().and_then(|n| n.split_once(' ')) {
        Some((f, l)) => (Some(f.to_owned()), Some(l.trim().to_owned())),
        None => (full.clone(), None),
    };
    let (item_name, link) = match item {
        Some(ItemInput {
            kind: ItemKind::Talent,
            id,
        }) => match core_db::talent::get(tx, id).await? {
            Some(t) => (Some(t.name), first_link(&t.custom)),
            None => return Err(ApiError::invalid("item", "there is no such talent")),
        },
        Some(ItemInput {
            kind: ItemKind::Project,
            id,
        }) => match core_db::projects::get(tx, id).await? {
            Some(p) => (Some(p.name), None),
            None => return Err(ApiError::invalid("item", "there is no such project")),
        },
        None => (None, None),
    };
    let your_name = match ctx.user_id() {
        Some(u) => core_db::users::get(tx, u).await?.map(|u| u.name),
        None => None,
    };
    let workspace = core_db::workspaces::get(tx, ctx.workspace_id)
        .await?
        .map(|w| w.name);
    let country = org
        .country_code
        .as_deref()
        .and_then(core_engines::countries::by_code)
        .map(|c| c.name.to_owned());
    let nonempty = |v: Option<String>| v.filter(|s| !s.trim().is_empty());
    Ok(Values(vec![
        ("firstName", nonempty(first)),
        ("lastName", nonempty(last)),
        ("fullName", nonempty(full)),
        ("organisation", nonempty(Some(org.name.clone()))),
        ("city", nonempty(org.city.clone())),
        ("country", country),
        ("item", nonempty(item_name)),
        ("link", link),
        ("yourName", nonempty(your_name)),
        ("workspace", nonempty(workspace)),
    ]))
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PitchDraftRequest {
    pub template_key: String,
    pub contact_id: Option<Uuid>,
    pub item: Option<ItemInput>,
}

/// A pitch template filled in, to change before it goes.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PitchDraft {
    pub template_key: String,
    /// What sending it logs.
    pub action: String,
    pub subject: String,
    pub body: String,
    /// Placeholders nothing filled in, left in the text to fill by hand.
    pub missing: Vec<String>,
    /// Where it can go: the person's first email address and phone number.
    pub email: Option<String>,
    pub phone: Option<String>,
}

/// Fills a pitch template in for a person at an organisation (both
/// optional) and what is pitched to them.
pub async fn draft(
    state: &AppState,
    ctx: &Ctx,
    organisation_id: Uuid,
    r: PitchDraftRequest,
) -> ApiResult<PitchDraft> {
    ctx.authorize(Permission::OrganisationRead)?;
    let mut tx = begin(state, ctx).await?;
    let org = organisation(&mut tx, organisation_id).await?;
    let (settings, _) = settings_in(&mut tx).await?;
    let template: PitchTemplate = settings
        .template(r.template_key.trim())
        .cloned()
        .ok_or_else(|| {
            ApiError::invalid(
                "template_key",
                "is not one of the workspace's pitch templates",
            )
        })?;
    let contact = match r.contact_id {
        Some(id) => {
            ctx.authorize(Permission::ContactRead)?;
            Some(person(&mut tx, organisation_id, id).await?)
        }
        None => None,
    };
    let values = values(&mut tx, ctx, &org, contact.as_ref(), r.item).await?;
    tx.commit().await?;
    let mut missing = BTreeSet::new();
    let subject = fill(&template.subject, &values, &mut missing);
    let body = fill(&template.body, &values, &mut missing);
    Ok(PitchDraft {
        template_key: template.key,
        action: template.action,
        subject,
        body,
        missing: missing.into_iter().collect(),
        email: contact.as_ref().and_then(|c| c.emails.first().cloned()),
        phone: contact.as_ref().and_then(|c| c.phones.first().cloned()),
    })
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PitchSend {
    pub organisation_id: Uuid,
    pub contact_id: Uuid,
    pub item: Option<ItemInput>,
    /// The pitch template it was written from, if any.
    pub template_key: Option<String>,
    /// What it logs, a key from the workspace's actions; default the
    /// template's, or `pitched`.
    pub type_key: Option<String>,
    /// Default: the person's first email address.
    pub to: Option<String>,
    pub subject: String,
    /// Plain text; the sender's signature goes under it.
    pub body: String,
    /// The mailbox to send from; default the sender's own, if connected.
    pub integration_id: Option<Uuid>,
}

/// Sends a pitch from OnHold, by email from the member's mailbox (or
/// OnHold's sender when they have none), and logs it with its message.
pub async fn send(state: &AppState, ctx: &Ctx, s: PitchSend) -> ApiResult<PitchView> {
    ctx.authorize(Permission::OrganisationWrite)?;
    ctx.authorize(Permission::InboxWrite)?;
    ctx.authorize(Permission::ContactRead)?;
    let mut tx = begin(state, ctx).await?;
    let org = organisation(&mut tx, s.organisation_id)
        .await
        .map_err(|e| match e {
            ApiError::NotFound => {
                ApiError::invalid("organisation_id", "there is no such organisation")
            }
            e => e,
        })?;
    let (settings, _) = settings_in(&mut tx).await?;
    let contact = person(&mut tx, org.id, s.contact_id).await?;
    let template_key = s
        .template_key
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty());
    let type_key = match (s.type_key, &template_key) {
        (Some(t), _) => t.trim().to_owned(),
        (None, Some(k)) => settings
            .template(k)
            .map_or_else(|| "pitched".to_owned(), |t| t.action.clone()),
        (None, None) => "pitched".to_owned(),
    };
    let to = match s.to.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()) {
        Some(t) => t,
        None => contact.emails.first().cloned().ok_or_else(|| {
            ApiError::invalid(
                "to",
                "they have no email address; add one or say where to send it",
            )
        })?,
    };
    let left = unfilled(&format!("{}\n{}", s.subject, s.body));
    if !left.is_empty() {
        let names: Vec<String> = left.iter().map(|n| format!("{{{n}}}")).collect();
        return Err(ApiError::invalid(
            "body",
            format!("fill in {} before sending", names.join(", ")),
        ));
    }
    let subject = s.subject.trim().to_owned();
    let parts = Parts {
        contact_id: Some(contact.id),
        type_key,
        channel: Some(PitchChannel::Email),
        item: s.item,
        note: Some(subject.chars().take(pitches::MAX_NOTE).collect()),
        happened_at: Utc::now(),
        template_key,
    };
    pitches::check(
        &mut tx,
        &settings,
        org.id,
        PitchKind::Action,
        &parts,
        Changed::ALL,
    )
    .await?;
    let (_, message) = inbox::compose_in(
        state,
        &mut tx,
        ctx,
        Compose {
            integration_id: s.integration_id,
            to: vec![to],
            subject: Some(subject),
            body: Some(s.body),
            contact_id: Some(contact.id),
            signature: true,
            ..Compose::default()
        },
    )
    .await?;
    let view = pitches::insert(
        &mut tx,
        ctx,
        &settings,
        org.id,
        PitchKind::Action,
        &parts,
        Some(message.id),
    )
    .await?;
    tx.commit().await?;
    Ok(view)
}

/// How the member sends a pitch, and what they would send it from.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PitchingMe {
    /// What they picked, if anything.
    pub send_with: Option<PitchSendWith>,
    /// The workspace's default, used when they picked nothing.
    pub workspace_send_with: PitchSendWith,
    /// The mailbox Send goes from: their own connected one. Without it,
    /// OnHold sends in the workspace's name with answers going to them.
    pub mailbox: Option<String>,
}

fn not_member() -> ApiError {
    ApiError::forbidden(
        "member_required",
        "how pitches are sent is picked by a member of this workspace, signed in",
    )
}

fn member(ctx: &Ctx) -> ApiResult<core_domain::UserId> {
    ctx.user_id()
        .filter(|_| ctx.role.is_some())
        .ok_or_else(not_member)
}

pub async fn me(state: &AppState, ctx: &Ctx) -> ApiResult<PitchingMe> {
    ctx.authorize(Permission::OrganisationRead)?;
    let user = member(ctx)?;
    let mut tx = begin(state, ctx).await?;
    let (settings, _) = settings_in(&mut tx).await?;
    let send_with = core_db::pitching::send_with(&mut tx, user)
        .await?
        .as_deref()
        .and_then(PitchSendWith::parse);
    let mailbox = if ctx.permissions.contains(&Permission::InboxWrite) {
        inbox::own_mailbox(&mut tx, ctx)
            .await?
            .map(|m| m.mailbox().to_owned())
    } else {
        None
    };
    tx.commit().await?;
    Ok(PitchingMe {
        send_with,
        workspace_send_with: settings.send_with,
        mailbox,
    })
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PitchingMePatch {
    /// `null` goes back to the workspace's default.
    pub send_with: Option<PitchSendWith>,
}

pub async fn set_me(state: &AppState, ctx: &Ctx, p: PitchingMePatch) -> ApiResult<PitchingMe> {
    ctx.authorize(Permission::OrganisationRead)?;
    let user = member(ctx)?;
    let mut tx = begin(state, ctx).await?;
    if !core_db::pitching::set_send_with(&mut tx, user, p.send_with.map(PitchSendWith::as_str))
        .await?
    {
        return Err(not_member());
    }
    tx.commit().await?;
    me(state, ctx).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> Values {
        Values(
            PLACEHOLDERS
                .iter()
                .map(|(n, _)| {
                    let v = match *n {
                        "firstName" => Some("Ana".to_owned()),
                        "item" => Some("The Comets".to_owned()),
                        _ => None,
                    };
                    (*n, v)
                })
                .collect(),
        )
    }

    #[test]
    fn placeholders_fill_in_and_the_rest_stay() {
        let mut missing = BTreeSet::new();
        let out = fill(
            "Hi {firstName}, {item} at {link}. {notOne} {} {item",
            &values(),
            &mut missing,
        );
        assert_eq!(out, "Hi Ana, The Comets at {link}. {notOne} {} {item");
        assert_eq!(missing.into_iter().collect::<Vec<_>>(), vec!["link"]);
        assert_eq!(
            unfilled("Hi {firstName}, {notOne}")
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["firstName"]
        );
        assert!(unfilled("Hi Ana, {{x}} £{5}").is_empty());
    }

    #[test]
    fn a_link_is_the_first_web_address() {
        let custom = serde_json::json!({ "genre": "jazz", "epk": "https://example.com/epk" });
        assert_eq!(
            first_link(&custom).as_deref(),
            Some("https://example.com/epk")
        );
        assert_eq!(first_link(&serde_json::json!({ "x": "not a link" })), None);
    }
}
