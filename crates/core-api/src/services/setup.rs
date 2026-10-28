//! The setup questionnaire: what a workspace's people say they will use
//! OnHold for decides its presets, modules, extras and words. It can be sent
//! again at any time; switching something off only hides it. Comments go to
//! OnHold's team, and a price list (OnHold's to set) gives a live estimate.

use std::collections::BTreeMap;

use core_domain::workspace::{self as ws, Perspective};
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
    pub requests: Vec<core_db::setup_requests::SetupRequest>,
    pub price_list: PriceList,
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
    let requests = core_db::setup_requests::recent(&mut tx, 10).await?;
    let price_list = read_price_list(core_db::platform_settings::get(&mut tx, PRICE_LIST).await?);
    tx.commit().await?;
    let answers = w
        .setup
        .clone()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_else(|| setup::answers_for(&w.presets));
    Ok(SetupState {
        questions: setup::questions(),
        answers,
        done_at: w.setup_at,
        current: current_plan(&w),
        requests,
        price_list,
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
    answers: Answers,
    comment: Option<String>,
    if_match: Option<i64>,
) -> ApiResult<core_db::workspaces::Workspace> {
    ctx.authorize(Permission::WorkspaceUpdate)?;
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
    let saved = core_db::workspaces::apply_setup(
        &mut tx,
        ctx.workspace_id,
        core_db::workspaces::Setup {
            // No role ticked keeps the perspective as it is.
            perspective: plan.perspective.unwrap_or(before.perspective),
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
    record(
        &mut tx,
        ctx,
        Change {
            topic: "workspace.setup_changed",
            resource: "workspace",
            object_id: Some(ctx.workspace_id.0),
            before: Some(setup_view(&before)),
            after: Some(setup_view(&after)),
            event_extra: None,
        },
    )
    .await?;
    if let Some(body) = comment {
        let request = core_db::setup_requests::insert(&mut tx, &body, ctx.user_id()).await?;
        record(
            &mut tx,
            ctx,
            Change {
                topic: "setup_request.created",
                resource: "setup_request",
                object_id: Some(request.id),
                before: None,
                after: Some(json!({ "body": request.body })),
                event_extra: None,
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(after)
}

fn setup_view(w: &core_db::workspaces::Workspace) -> Value {
    json!({
        "perspective": w.perspective, "presets": w.presets, "enabled_modules": w.enabled_modules,
        "extras": w.extras, "vocabulary": w.vocabulary, "answers": w.setup,
    })
}

/// The perspective a set of presets gives, for the command line.
pub fn perspective_of(presets: &[String]) -> ApiResult<Option<Perspective>> {
    core_modules::presets::combine(presets)
        .map(|c| c.perspective)
        .map_err(|e| ApiError::invalid("presets", e.to_string()))
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
