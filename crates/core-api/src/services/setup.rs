//! The setup questionnaire: what a workspace's people say they will use
//! OnHold for decides its presets, modules, extras and words. It can be sent
//! again at any time; switching something off only hides it. Comments go to
//! OnHold's team, and a price list (OnHold's to set) gives a live estimate.

use core_domain::workspace::{self as ws, Perspective};
use core_domain::{Ctx, Permission};
use core_modules::setup::{self, Answers, Plan, Question};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::extract::Session;
use crate::services::{Change, admin, record};

pub const PRICE_LIST: &str = "price_list";
const COMMENT_MAX: usize = 4000;

/// What OnHold charges, in minor units a month: a base price, plus each
/// ticked feature, plus the team size. Shown as an estimate only.
pub fn default_price_list() -> Value {
    json!({
        "currency": "GBP",
        "placeholder": true,
        "base": 1500,
        "features": {
            "bookings": 0, "calendar": 0, "email": 500, "invoicing": 800, "documents": 500,
            "events": 500, "portal": 1000, "commission": 500, "custom": 1000
        },
        "team": { "just_me": 0, "small": 2000, "large": 6000 }
    })
}

/// Checks a price list's shape: a known currency, and whole, non-negative
/// amounts for known features and team sizes only.
pub fn validate_price_list(v: &Value) -> ApiResult<()> {
    let bad = |m: &str| ApiError::invalid("price_list", m.to_owned());
    let obj = v.as_object().ok_or_else(|| bad("must be an object"))?;
    let currency = obj
        .get("currency")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("currency is required"))?;
    ws::validate_currency(currency).map_err(|e| bad(&e.message))?;
    let amount = |x: &Value| x.as_u64().is_some_and(|n| n <= 10_000_000);
    if !obj.get("base").is_some_and(amount) {
        return Err(bad("base must be a whole amount in minor units"));
    }
    let questions = setup::questions();
    for (group, question) in [("features", "features"), ("team", "team")] {
        let known: Vec<&str> = questions
            .iter()
            .find(|q| q.key == question)
            .map(|q| q.choices.iter().map(|c| c.key.as_str()).collect())
            .unwrap_or_default();
        let prices = obj
            .get(group)
            .and_then(Value::as_object)
            .ok_or_else(|| bad(&format!("{group} must be an object")))?;
        for (k, p) in prices {
            if !known.contains(&k.as_str()) {
                return Err(bad(&format!("unknown {group} key `{k}`")));
            }
            if !amount(p) {
                return Err(bad(&format!("{group}.{k} must be a whole amount")));
            }
        }
    }
    for k in obj.keys() {
        if !["currency", "placeholder", "base", "features", "team"].contains(&k.as_str()) {
            return Err(bad(&format!("unknown field `{k}`")));
        }
    }
    Ok(())
}

pub struct SetupState {
    pub questions: Vec<Question>,
    pub answers: Answers,
    /// When the form was last sent; `None` if never.
    pub done_at: Option<chrono::DateTime<chrono::Utc>>,
    pub current: Plan,
    pub requests: Vec<core_db::setup_requests::SetupRequest>,
    pub price_list: Value,
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
    let price_list = core_db::platform_settings::get(&mut tx, PRICE_LIST)
        .await?
        .unwrap_or_else(default_price_list);
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

pub async fn admin_price_list(state: &AppState, session: &Session) -> ApiResult<Value> {
    admin::require_superadmin(state, session).await?;
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    let list = core_db::platform_settings::get(&mut tx, PRICE_LIST).await?;
    tx.commit().await?;
    Ok(list.unwrap_or_else(default_price_list))
}

pub async fn admin_set_price_list(
    state: &AppState,
    session: &Session,
    list: &Value,
) -> ApiResult<()> {
    admin::require_superadmin(state, session).await?;
    validate_price_list(list)?;
    let mut tx = core_db::begin(&state.db, None, Some(session.user_id)).await?;
    core_db::platform_settings::admin_set(&mut tx, PRICE_LIST, list).await?;
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
        bad["features"]["teleport"] = json!(100);
        assert!(validate_price_list(&bad).is_err());
        let mut bad = default_price_list();
        bad["base"] = json!(-1);
        assert!(validate_price_list(&bad).is_err());
        let mut bad = default_price_list();
        bad["currency"] = json!("XYZ");
        assert!(validate_price_list(&bad).is_err());
    }
}
