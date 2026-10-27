//! Tasks: to-dos with a due date and an assignee, about any record or none.
//! People make them by hand; rules make them with the `create_task` action.

use chrono::{DateTime, Utc};
use core_db::Tx;
use core_db::tasks::Task;
use core_domain::{Ctx, Permission, UserId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::crm::Check;
use super::{Change, begin, check_version, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};
use crate::routes::nullable;

pub const ENTITY: &str = "task";

/// Records a task can be about.
pub const RELATED: &[&str] = &[
    "talent",
    "organisation",
    "contact",
    "enquiry",
    "booking",
    "quote",
];

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TaskView {
    pub id: Uuid,
    pub title: String,
    pub notes: Option<String>,
    pub due_at: Option<DateTime<Utc>>,
    pub assignee_id: Option<Uuid>,
    /// `talent`, `organisation`, `contact`, `enquiry`, `booking` or `quote`.
    pub related_entity: Option<String>,
    pub related_id: Option<Uuid>,
    /// The related record's name or title, if it still exists.
    pub related_name: Option<String>,
    /// `open` or `done`.
    pub status: String,
    pub done_at: Option<DateTime<Utc>>,
    pub done_by: Option<Uuid>,
    /// `manual` or `rule`.
    pub origin: String,
    pub rule_id: Option<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub updated_by: Option<UserId>,
}

/// The name or title of a record, if it exists.
pub(crate) async fn record_name(tx: &mut Tx, entity: &str, id: Uuid) -> ApiResult<Option<String>> {
    Ok(match entity {
        "talent" => core_db::talent::get(tx, id).await?.map(|r| r.name),
        "organisation" => core_db::organisations::get(tx, id).await?.map(|r| r.name),
        "contact" => core_db::contacts::get(tx, id).await?.map(|r| r.name),
        "enquiry" => core_db::enquiries::get(tx, id).await?.map(|r| r.title),
        "booking" => core_db::bookings::get(tx, id).await?.map(|r| r.title),
        "quote" => core_db::quotes::get(tx, id).await?.map(|q| {
            format!(
                "{} · {}",
                super::quotes::reference(q.number, q.revision),
                q.booking_title
            )
        }),
        _ => None,
    })
}

async fn view(tx: &mut Tx, t: Task) -> ApiResult<TaskView> {
    let related_name = match (&t.related_entity, t.related_id) {
        (Some(e), Some(id)) => record_name(tx, e, id).await?,
        _ => None,
    };
    Ok(TaskView {
        id: t.id,
        title: t.title,
        notes: t.notes,
        due_at: t.due_at,
        assignee_id: t.assignee_id,
        related_entity: t.related_entity,
        related_id: t.related_id,
        related_name,
        status: t.status,
        done_at: t.done_at,
        done_by: t.done_by,
        origin: t.origin,
        rule_id: t.rule_id,
        version: t.version,
        created_at: t.created_at,
        updated_at: t.updated_at,
        created_by: t.created_by,
        updated_by: t.updated_by,
    })
}

pub fn to_json(v: &TaskView) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

async fn load(tx: &mut Tx, id: Uuid) -> ApiResult<Option<TaskView>> {
    match core_db::tasks::get(tx, id).await? {
        Some(t) => Ok(Some(view(tx, t).await?)),
        None => Ok(None),
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TaskPage {
    pub items: Vec<TaskView>,
    pub total: i64,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct TaskFilter {
    /// `open` (default), `done` or `all`.
    pub status: Option<String>,
    /// A member's id, or `me`.
    pub assignee: Option<String>,
    /// Tasks about this record.
    pub related_id: Option<Uuid>,
    /// Open tasks due before this.
    pub due_before: Option<DateTime<Utc>>,
    pub offset: Option<i64>,
    /// 1 to 200, default 50.
    pub limit: Option<i64>,
}

pub async fn list(state: &AppState, ctx: &Ctx, f: &TaskFilter) -> ApiResult<TaskPage> {
    ctx.authorize(Permission::TaskRead)?;
    let status = match f.status.as_deref().map(str::trim).unwrap_or("open") {
        "all" => None,
        s @ ("open" | "done") => Some(s.to_owned()),
        _ => return Err(ApiError::invalid("status", "use open, done or all")),
    };
    let assignee_id = match f.assignee.as_deref().map(str::trim) {
        None | Some("") => None,
        Some("me") => Some(
            ctx.user_id()
                .ok_or_else(|| {
                    ApiError::invalid("assignee", "only a person has tasks of their own")
                })?
                .0,
        ),
        Some(id) => Some(
            id.parse()
                .map_err(|_| ApiError::invalid("assignee", "give a member's id or `me`"))?,
        ),
    };
    let mut tx = begin(state, ctx).await?;
    let (rows, total) = core_db::tasks::list(
        &mut tx,
        core_db::tasks::Filter {
            status,
            assignee_id,
            related_id: f.related_id,
            due_before: f.due_before,
            offset: f.offset.unwrap_or(0).max(0),
            limit: f.limit.unwrap_or(50).clamp(1, 200),
        },
    )
    .await?;
    let mut items = Vec::with_capacity(rows.len());
    for t in rows {
        items.push(view(&mut tx, t).await?);
    }
    tx.commit().await?;
    Ok(TaskPage { items, total })
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<TaskView> {
    ctx.authorize(Permission::TaskRead)?;
    let mut tx = begin(state, ctx).await?;
    let v = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(v)
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct NewTask {
    pub title: String,
    pub notes: Option<String>,
    pub due_at: Option<DateTime<Utc>>,
    /// Default: the member creating it.
    pub assignee_id: Option<Uuid>,
    pub related_entity: Option<String>,
    pub related_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct TaskPatch {
    pub title: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub notes: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<DateTime<Utc>>)]
    pub due_at: Option<Option<DateTime<Utc>>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<Uuid>)]
    pub assignee_id: Option<Option<Uuid>>,
    /// `open` or `done`.
    pub status: Option<String>,
}

async fn check_assignee(tx: &mut Tx, check: &mut Check, id: Option<Uuid>) -> ApiResult<()> {
    if let Some(id) = id
        && core_db::members::get(tx, UserId(id)).await?.is_none()
    {
        check.issue("assignee_id", "must be a member of the workspace");
    }
    Ok(())
}

pub async fn create(state: &AppState, ctx: &Ctx, new: NewTask) -> ApiResult<TaskView> {
    ctx.authorize(Permission::TaskWrite)?;
    let mut tx = begin(state, ctx).await?;
    let view = create_in_tx(&mut tx, ctx, new, None).await?;
    tx.commit().await?;
    Ok(view)
}

/// The create, inside the caller's transaction; `rule_id` marks a rule's task.
pub(crate) async fn create_in_tx(
    tx: &mut Tx,
    ctx: &Ctx,
    new: NewTask,
    rule_id: Option<Uuid>,
) -> ApiResult<TaskView> {
    let mut check = Check::default();
    let title = check.required("title", &new.title, 300);
    let notes = check.long("notes", new.notes.as_deref(), 5000);
    let assignee = new.assignee_id.or(ctx.user_id().map(|u| u.0));
    check_assignee(tx, &mut check, assignee).await?;
    let related_entity = new
        .related_entity
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty());
    match (related_entity, new.related_id) {
        (None, None) => {}
        (Some(e), Some(id)) if RELATED.contains(&e) => {
            if record_name(tx, e, id).await?.is_none() {
                check.issue("related_id", format!("there is no such {e}"));
            }
        }
        (Some(_), Some(_)) => check.issue(
            "related_entity",
            format!("use one of: {}", RELATED.join(", ")),
        ),
        _ => check.issue("related_id", "give both related_entity and related_id"),
    }
    check.finish()?;
    let id = core_domain::ids::new_id();
    core_db::tasks::insert(
        tx,
        id,
        core_db::tasks::Values {
            title: &title,
            notes: notes.as_deref(),
            due_at: new.due_at,
            assignee_id: assignee,
            related_entity,
            related_id: new.related_id,
            status: "open",
            done_at: None,
            done_by: None,
        },
        rule_id,
        ctx.user_id(),
    )
    .await?;
    let view = load(tx, id).await?.ok_or(ApiError::NotFound)?;
    record(
        tx,
        ctx,
        Change {
            topic: "task.created",
            resource: ENTITY,
            object_id: Some(id),
            before: None,
            after: Some(to_json(&view)),
            event_extra: None,
        },
    )
    .await?;
    Ok(view)
}

pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    p: TaskPatch,
    if_match: Option<i64>,
) -> ApiResult<TaskView> {
    ctx.authorize(Permission::TaskWrite)?;
    let mut tx = begin(state, ctx).await?;
    let version = core_db::tasks::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    check_version(version, if_match)?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    let mut check = Check::default();
    let title = match &p.title {
        Some(t) => check.required("title", t, 300),
        None => before.title.clone(),
    };
    let notes = match &p.notes {
        Some(n) => check.long("notes", n.as_deref(), 5000),
        None => before.notes.clone(),
    };
    let assignee_id = p.assignee_id.unwrap_or(before.assignee_id);
    if p.assignee_id.is_some() {
        check_assignee(&mut tx, &mut check, assignee_id).await?;
    }
    let status = match p.status.as_deref().map(str::trim) {
        None => before.status.clone(),
        Some(s @ ("open" | "done")) => s.to_owned(),
        Some(_) => {
            check.issue("status", "use open or done");
            before.status.clone()
        }
    };
    check.finish()?;
    let due_at = p.due_at.unwrap_or(before.due_at);
    let (done_at, done_by) = match (before.status.as_str(), status.as_str()) {
        ("open", "done") => (Some(Utc::now()), ctx.user_id().map(|u| u.0)),
        (_, "open") => (None, None),
        _ => (before.done_at, before.done_by),
    };
    if title == before.title
        && notes == before.notes
        && due_at == before.due_at
        && assignee_id == before.assignee_id
        && status == before.status
    {
        return Ok(before);
    }
    core_db::tasks::update(
        &mut tx,
        id,
        core_db::tasks::Values {
            title: &title,
            notes: notes.as_deref(),
            due_at,
            assignee_id,
            related_entity: before.related_entity.as_deref(),
            related_id: before.related_id,
            status: &status,
            done_at,
            done_by,
        },
        ctx.user_id(),
    )
    .await?;
    let after = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    let topic = match (before.status.as_str(), after.status.as_str()) {
        ("open", "done") => "task.completed",
        ("done", "open") => "task.reopened",
        _ => "task.updated",
    };
    record(
        &mut tx,
        ctx,
        Change {
            topic,
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: Some(to_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

pub async fn delete(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::TaskWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = load(&mut tx, id).await?.ok_or(ApiError::NotFound)?;
    core_db::tasks::soft_delete(&mut tx, id, ctx.user_id()).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "task.deleted",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(to_json(&before)),
            after: None,
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
