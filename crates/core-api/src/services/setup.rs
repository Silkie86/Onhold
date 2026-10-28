//! The setup questionnaire: what a workspace's people say they will use
//! OnHold for decides its presets, modules, extras and words. It can be sent
//! again at any time; switching something off only hides it. Comments go to
//! OnHold's team, and a price list (OnHold's to set) gives a live estimate.

use std::collections::BTreeMap;

use core_domain::workspace as ws;
use core_domain::{Ctx, Permission};
use core_modules::setup::{self, Answers, Plan, Question};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::extract::Session;
use crate::services::{Change, admin, record};

pub const PRICE_LIST: &str = "price_list";
const COMMENT_MAX: usize = 4000;
/// Comments a workspace can have waiting for OnHold's team at once.
const OPEN_REQUESTS_MAX: i64 = 20;

/// What OnHold charges, in minor units a month: a base price, plus each
/// ticked feature, plus the team size. Shown as an estimate only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PriceList {
    /// ISO 4217, e.g. `GBP`.
    pub currency: String,
    /// Shown as example prices while pricing is not final.
    #[serde(default)]
    pub placeholder: bool,
    pub base: u32,
    /// Per feature key of the setup form.
    pub features: BTreeMap<String, u32>,
    /// Per team size (`just_me`, `small`, `large`).
    pub team: BTreeMap<String, u32>,
}

pub fn default_price_list() -> PriceList {
    let map = |pairs: &[(&str, u32)]| pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect();
    PriceList {
        currency: "GBP".into(),
        placeholder: true,
        base: 1500,
        features: map(&[
            ("bookings", 0),
            ("calendar", 0),
            ("email", 500),
            ("invoicing", 800),
            ("documents", 500),
            ("events", 500),
            ("portal", 1000),
            ("commission", 500),
            ("custom", 1000),
        ]),
        team: map(&[("just_me", 0), ("small", 2000), ("large", 6000)]),
    }
}

const MAX_AMOUNT: u32 = 10_000_000;

/// Checks a price list: a known currency, and amounts for known features
/// and team sizes only, none absurdly large.
pub fn validate_price_list(p: &PriceList) -> ApiResult<()> {
    let bad = |m: String| ApiError::invalid("price_list", m);
    ws::validate_currency(&p.currency).map_err(|e| bad(e.message))?;
    if p.base > MAX_AMOUNT {
        return Err(bad("base is too large".into()));
    }
    let questions = setup::questions();
    for (group, prices) in [("features", &p.features), ("team", &p.team)] {
        let known: Vec<&str> = questions
            .iter()
            .find(|q| q.key == group)
            .map(|q| q.choices.iter().map(|c| c.key.as_str()).collect())
            .unwrap_or_default();
        for (k, amount) in prices {
            if !known.contains(&k.as_str()) {
                return Err(bad(format!("unknown {group} key `{k}`")));
            }
            if *amount > MAX_AMOUNT {
                return Err(bad(format!("{group}.{k} is too large")));
            }
        }
    }
    Ok(())
}

/// The stored list, or the example one; a stored list from an older
/// release that no longer reads falls back too.
fn read_price_list(stored: Option<Value>) -> PriceList {
    stored
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_else(default_price_list)
}

pub struct SetupState {
    pub questions: Vec<Question>,
    pub answers: Answers,
    /// When the form was last sent; `None` if never.
    pub done_at: Option<chrono::DateTime<chrono::Utc>>,
    pub current: Plan,
    /// The workspace's own comments; only for those who can change the setup.
    pub requests: Vec<core_db::setup_requests::SetupRequest>,
    pub price_list: PriceList,
    pub version: i64,
}

fn current_plan(w: &core_db::workspaces::Workspace) -> Plan {
    Plan {
        presets: w.presets.clone(),
        modules: w.enabled_modules.clone(),
        extras: w.extras.clone(),
        perspective: Some(w.perspective),
        vocabulary: serde_json::from_value(w.vocabulary.clone()).unwrap_or_default(),
    }
}

/// The form, the answers so far (or what the workspace's presets suggest),
/// what is set up now, recent comments and the price list.
pub async fn get(state: &AppState, ctx: &Ctx) -> ApiResult<SetupState> {
    ctx.authorize(Permission::WorkspaceRead)?;
    let mut tx = core_db::begin(&state.db, Some(ctx.workspace_id), ctx.user_id()).await?;
    let w = core_db::workspaces::get(&mut tx, ctx.workspace_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let requests = if ctx.permissions.contains(&Permission::WorkspaceUpdate) {
        core_db::setup_requests::recent(&mut tx, 10).await?
    } else {
        Vec::new()
    };
    let price_list = read_price_list(core_db::platform_settings::get(&mut tx, PRICE_LIST).await?);
    tx.commit().await?;
    let answers = w
        .setup
        .clone()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_else(|| {
            if w.presets.is_empty() {
                setup::answers_from(&w.enabled_modules, &w.extras)
            } else {
                setup::answers_for(&w.presets)
            }
        });
    Ok(SetupState {
        questions: setup::questions(),
        answers,
        done_at: w.setup_at,
        current: current_plan(&w),
        requests,
        price_list,
        version: w.version,
    })
}

/// What the answers would set up, without saving.
pub fn preview(ctx: &Ctx, answers: &Answers) -> ApiResult<Plan> {
    ctx.authorize(Permission::WorkspaceRead)?;
    setup::plan(answers).map_err(|e| ApiError::invalid(e.field, e.to_string()))
}

/// Saves the answers and sets the workspace up from them. A comment is kept
/// for OnHold's team.
pub async fn save(
    state: &AppState,
    ctx: &Ctx,
    mut answers: Answers,
    comment: Option<String>,
    if_match: Option<i64>,
) -> ApiResult<core_db::workspaces::Workspace> {
    ctx.authorize(Permission::WorkspaceUpdate)?;
    answers.tidy();
    let plan = setup::plan(&answers).map_err(|e| ApiError::invalid(e.field, e.to_string()))?;
    let comment = comment
        .map(|c| c.trim().to_owned())
        .filter(|c| !c.is_empty());
    if comment
        .as_ref()
        .is_some_and(|c| c.chars().count() > COMMENT_MAX)
    {
        return Err(ApiError::invalid(
            "comment",
            format!("must be at most {COMMENT_MAX} characters"),
        ));
    }

    let mut tx = core_db::begin(&state.db, Some(ctx.workspace_id), ctx.user_id()).await?;
    let before = core_db::workspaces::get(&mut tx, ctx.workspace_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let expected = if_match.unwrap_or(before.version);
    if expected != before.version {
        return Err(ApiError::version_conflict());
    }
    let vocabulary = serde_json::to_value(&plan.vocabulary).map_err(ApiError::internal)?;
    let answers_json = serde_json::to_value(&answers).map_err(ApiError::internal)?;
    let perspective = plan.perspective.unwrap_or(before.perspective);
    let unchanged = before.setup_at.is_some()
        && before.setup.as_ref() == Some(&answers_json)
        && before.perspective == perspective
        && before.presets == plan.presets
        && before.enabled_modules == plan.modules
        && before.extras == plan.extras
        && before.vocabulary == vocabulary;
    let after = if unchanged {
        before
    } else {
        let saved = core_db::workspaces::apply_setup(
            &mut tx,
            ctx.workspace_id,
            core_db::workspaces::Setup {
                // No role ticked keeps the perspective as it is.
                perspective,
                presets: &plan.presets,
                enabled_modules: &plan.modules,
                extras: &plan.extras,
                vocabulary: &vocabulary,
                answers: &answers_json,
            },
            expected,
            ctx.user_id(),
        )
        .await?;
        if !saved {
            return Err(ApiError::version_conflict());
        }
        let after = core_db::workspaces::get(&mut tx, ctx.workspace_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        let (was, now) = changed(&setup_view(&before), &setup_view(&after));
        record(
            &mut tx,
            ctx,
            Change {
                topic: "workspace.setup_changed",
                resource: "workspace",
                object_id: Some(ctx.workspace_id.0),
                before: Some(was),
                after: Some(now),
                event_extra: None,
            },
        )
        .await?;
        after
    };
    if let Some(body) = comment {
        if core_db::setup_requests::open_count(&mut tx).await? >= OPEN_REQUESTS_MAX {
            return Err(ApiError::invalid(
                "comment",
                format!(
                    "you have {OPEN_REQUESTS_MAX} messages waiting for the OnHold team; we'll answer those first"
                ),
            ));
        }
        let request = core_db::setup_requests::insert(&mut tx, &body, ctx.user_id()).await?;
        // The comment itself stays in its own table, not copied into the log.
        record(
            &mut tx,
            ctx,
            Change {
                topic: "setup_request.created",
                resource: "setup_request",
                object_id: Some(request.id),
                before: None,
                after: Some(json!({ "id": request.id })),
                event_extra: None,
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(after)
}

/// Only the keys that differ between two flat objects.
fn changed(before: &Value, after: &Value) -> (Value, Value) {
    let (mut was, mut now) = (serde_json::Map::new(), serde_json::Map::new());
    if let (Some(b), Some(a)) = (before.as_object(), after.as_object()) {
        for (k, v) in a {
            if b.get(k) != Some(v) {
                was.insert(k.clone(), b.get(k).cloned().unwrap_or(Value::Null));
                now.insert(k.clone(), v.clone());
            }
        }
    }
    (Value::Object(was), Value::Object(now))
}

fn setup_view(w: &core_db::workspaces::Workspace) -> Value {
    json!({
        "perspective": w.perspective, "presets": w.presets, "enabled_modules": w.enabled_modules,
        "extras": w.extras, "vocabulary": w.vocabulary, "answers": w.setup,
    })
}

// Streamlining (OnHold's team) -------------------------------------------------------

/// Entities whose fields can be hidden.
pub const FIELD_ENTITIES: &[&str] = &["contact", "organisation", "talent", "enquiry", "booking"];
const MAX_HIDDEN: usize = 300;

/// A field OnHold's team may hide, and whether it is hidden.
pub struct HideableField {
    pub entity: &'static str,
    pub key: String,
    pub label: String,
    pub hidden: bool,
}

/// Whether the actor is OnHold's team: visiting the workspace, or a
/// superadmin signed in with two-factor authentication.
async fn is_operator(tx: &mut core_db::Tx, ctx: &Ctx) -> ApiResult<bool> {
    if ctx.impersonator().is_some() {
        return Ok(true);
    }
    Ok(match ctx.user_id() {
        Some(id) => core_db::users::get(tx, id)
            .await?
            .is_some_and(|u| u.is_superadmin && u.totp_enabled_at.is_some()),
        None => false,
    })
}

/// Every optional field of the entities that can be streamlined, read in
/// the caller's transaction.
async fn optional_fields(tx: &mut core_db::Tx, ctx: &Ctx) -> ApiResult<Vec<HideableField>> {
    let mut fields = Vec::new();
    for entity in FIELD_ENTITIES {
        let schema = super::views::full_schema_in(tx, ctx, entity).await?;
        fields.extend(
            schema
                .fields
                .into_iter()
                .filter(|f| !f.required)
                .map(|f| HideableField {
                    entity,
                    key: f.key,
                    label: f.label,
                    hidden: false,
                }),
        );
    }
    Ok(fields)
}

/// What is hidden, and every optional field that could be, with the
/// workspace's version from the same read.
pub async fn streamline_options(
    state: &AppState,
    ctx: &Ctx,
) -> ApiResult<(Vec<String>, Vec<HideableField>, i64)> {
    ctx.authorize(Permission::WorkspaceRead)?;
    let mut tx = core_db::begin(&state.db, Some(ctx.workspace_id), ctx.user_id()).await?;
    let w = core_db::workspaces::get(&mut tx, ctx.workspace_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let mut fields = optional_fields(&mut tx, ctx).await?;
    tx.commit().await?;
    for f in &mut fields {
        f.hidden = w.hidden.contains(&format!("field:{}.{}", f.entity, f.key));
    }
    Ok((w.hidden, fields, w.version))
}

/// Checks hidden keys' shape. A field that no longer exists or has since
/// become required is dropped rather than refused, so a list saved before
/// a custom field was removed can still be saved again.
fn check_hidden(hidden: &mut Vec<String>, optional: &[HideableField]) -> ApiResult<()> {
    let bad = |m: String| ApiError::invalid("hidden", m);
    if hidden.len() > MAX_HIDDEN {
        return Err(bad(format!("at most {MAX_HIDDEN} items")));
    }
    for key in hidden.iter() {
        let (kind, rest) = key
            .split_once(':')
            .ok_or_else(|| bad(format!("`{key}` must start with nav:, section: or field:")))?;
        let shape_ok = (1..=100).contains(&rest.len())
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_./-".contains(&b));
        let kind_ok = match kind {
            "nav" => rest.starts_with('/'),
            "section" | "field" => true,
            _ => false,
        };
        if !shape_ok || !kind_ok {
            return Err(bad(format!(
                "`{key}` is not a valid key; keys start with nav:/, section: or field:"
            )));
        }
    }
    hidden.retain(|key| match key.strip_prefix("field:") {
        Some(rest) => optional.iter().any(|f| {
            rest.strip_prefix(f.entity)
                .and_then(|r| r.strip_prefix('.'))
                == Some(f.key.as_str())
        }),
        None => true,
    });
    Ok(())
}

/// Sets what OnHold's team hides in the workspace. It is kept apart from
/// the setup form's answers, so sending the form again keeps it.
pub async fn set_hidden(
    state: &AppState,
    ctx: &Ctx,
    mut hidden: Vec<String>,
    if_match: Option<i64>,
) -> ApiResult<core_db::workspaces::Workspace> {
    ctx.authorize(Permission::WorkspaceUpdate)?;
    let mut tx = core_db::begin(&state.db, Some(ctx.workspace_id), ctx.user_id()).await?;
    if !is_operator(&mut tx, ctx).await? {
        return Err(ApiError::forbidden(
            "streamlining_is_operator_only",
            "only OnHold's team can hide parts of the app; use Your setup instead",
        ));
    }
    hidden.sort();
    hidden.dedup();
    let optional = optional_fields(&mut tx, ctx).await?;
    check_hidden(&mut hidden, &optional)?;
    let before = core_db::workspaces::get(&mut tx, ctx.workspace_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let expected = if_match.unwrap_or(before.version);
    if expected != before.version
        || !core_db::workspaces::set_hidden(
            &mut tx,
            ctx.workspace_id,
            &hidden,
            expected,
            ctx.user_id(),
        )
        .await?
    {
        return Err(ApiError::version_conflict());
    }
    let after = core_db::workspaces::get(&mut tx, ctx.workspace_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "workspace.streamlined",
            resource: "workspace",
            object_id: Some(ctx.workspace_id.0),
            before: Some(json!({ "hidden": before.hidden })),
            after: Some(json!({ "hidden": after.hidden })),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

// OnHold's team ---------------------------------------------------------------------

pub async fn admin_requests(
    state: &AppState,
    session: &Session,
) -> ApiResult<Vec<core_db::setup_requests::AdminSetupRequest>> {
    admin::require_superadmin(state, session).await?;
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    let rows = core_db::setup_requests::admin_list(&mut tx, 200).await?;
    tx.commit().await?;
    Ok(rows)
}

pub async fn admin_resolve(state: &AppState, session: &Session, id: uuid::Uuid) -> ApiResult<()> {
    admin::require_superadmin(state, session).await?;
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    let done = core_db::setup_requests::admin_resolve(&mut tx, id).await?;
    tx.commit().await?;
    if done {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

pub async fn admin_price_list(state: &AppState, session: &Session) -> ApiResult<PriceList> {
    admin::require_superadmin(state, session).await?;
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    let list = core_db::platform_settings::get(&mut tx, PRICE_LIST).await?;
    tx.commit().await?;
    Ok(read_price_list(list))
}

pub async fn admin_set_price_list(
    state: &AppState,
    session: &Session,
    list: &PriceList,
) -> ApiResult<()> {
    admin::require_superadmin(state, session).await?;
    validate_price_list(list)?;
    let list = serde_json::to_value(list).map_err(ApiError::internal)?;
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    core_db::platform_settings::admin_set(&mut tx, PRICE_LIST, &list).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_price_list_is_valid() {
        validate_price_list(&default_price_list()).expect("valid");
        let mut bad = default_price_list();
        bad.features.insert("teleport".into(), 100);
        assert!(validate_price_list(&bad).is_err());
        let mut bad = default_price_list();
        bad.base = MAX_AMOUNT + 1;
        assert!(validate_price_list(&bad).is_err());
        let mut bad = default_price_list();
        bad.currency = "XYZ".into();
        assert!(validate_price_list(&bad).is_err());
        assert_eq!(
            read_price_list(Some(json!({ "base": "free" }))),
            default_price_list()
        );
    }
}
