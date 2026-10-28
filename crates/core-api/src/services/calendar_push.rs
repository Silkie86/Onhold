//! Bookings in members' own calendars. A member connects their Google or
//! Microsoft calendar from Settings → Calendar; OnHold makes a calendar of
//! its own in that account and keeps the workspace's bookings in it (or one
//! talent's): holds and pending bookings as tentative, confirmed ones as
//! busy, with the venue and a link back, never fees. A booking that
//! changes is written straight away; every connection is also compared in
//! full once a day, which catches anything missed. Talent who want their
//! bookings in their own calendar subscribe to the talent's iCal link.

use chrono::{DateTime, Duration, Utc};
use core_db::Tx;
use core_db::calendar_push::Push;
use core_domain::{BookingStatus, Ctx, Permission, UserId, WorkspaceId};
use core_integrations::calendar_events::{self, CalendarWriter, EventBody};
use core_integrations::oauth::{OAuthApp, Provider, Tokens};
use core_integrations::provider::ProviderError;
use data_encoding::BASE64URL_NOPAD;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use super::integrations::provider_error;
use super::{Change, begin, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

pub const ENTITY: &str = "calendar_push";
/// Writes bookings into one connection (`push_id`): every booking, or the
/// one named (`booking_id`).
pub const JOB_WRITE: &str = "calendar.write";
/// Removes OnHold's calendar from a connection disconnected along with
/// others, revokes its grant and forgets it (`push_id`).
pub const JOB_REVOKE: &str = "calendar.push_revoke";

/// Connections one member may have.
const MAX_PER_MEMBER: i64 = 5;
const STATE_AAD: &[u8] = b"calendar_push_state";
const CONNECT_TTL_SECS: i64 = 30 * 60;
/// How long one write may hold a connection.
const WRITE_LEASE_SECS: f64 = 10.0 * 60.0;
/// Which bookings are written: from a month back to two years ahead.
const PAST: Duration = Duration::days(30);
const AHEAD: Duration = Duration::days(730);
/// Calls to the provider in one full write; the rest follow straight after.
const MAX_WRITES_PER_RUN: usize = 200;
/// How often every booking is compared with the calendar.
const FULL_EVERY: Duration = Duration::hours(24);
const QUIET_FAILURES: i32 = 3;

#[derive(Serialize, Deserialize)]
struct Grant {
    refresh_token: String,
    access_token: String,
    expires_at: DateTime<Utc>,
}

fn grant_aad(id: Uuid) -> Vec<u8> {
    [b"calendar_push:".as_slice(), id.as_bytes()].concat()
}

fn seal(state: &AppState, id: Uuid, grant: &Grant) -> ApiResult<Vec<u8>> {
    let json = serde_json::to_vec(grant).map_err(ApiError::internal)?;
    Ok(state.config.keyring.encrypt(&json, &grant_aad(id)))
}

fn open(state: &AppState, p: &Push) -> ApiResult<Grant> {
    let plain = state
        .config
        .keyring
        .decrypt(&p.secret, &grant_aad(p.id))
        .map_err(ApiError::internal)?;
    serde_json::from_slice(&plain).map_err(ApiError::internal)
}

fn oauth_app<'a>(state: &'a AppState, provider: &str) -> ApiResult<&'a OAuthApp> {
    let app = match provider {
        "google" => state.config.mailboxes.google.as_ref(),
        "microsoft" => state.config.mailboxes.microsoft.as_ref(),
        _ => return Err(ApiError::invalid("provider", "use google or microsoft")),
    };
    app.ok_or_else(|| {
        ApiError::conflict(
            "provider_not_available",
            format!("{provider} calendars cannot be connected on this server"),
        )
    })
}

fn scopes(provider: Provider) -> &'static [&'static str] {
    match provider {
        Provider::Google => calendar_events::GOOGLE_WRITE_SCOPES,
        Provider::Microsoft => calendar_events::MICROSOFT_WRITE_SCOPES,
    }
}

fn redirect_uri(state: &AppState) -> String {
    state.config.app_url("/settings/calendar", &[])
}

// Views ----------------------------------------------------------------------------------

/// A member's calendar that bookings go into. Never carries the grant.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CalendarPushView {
    pub id: Uuid,
    pub user_id: Uuid,
    /// `google` or `microsoft`.
    pub provider: String,
    pub account: String,
    /// Only this talent's bookings, or every booking when absent.
    pub talent_id: Option<Uuid>,
    pub talent_name: Option<String>,
    /// `active`, `failing` (being retried) or `reconnect`.
    pub status: String,
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub connected_at: DateTime<Utc>,
}

impl From<&Push> for CalendarPushView {
    fn from(p: &Push) -> Self {
        Self {
            id: p.id,
            user_id: p.user_id.0,
            provider: p.provider.clone(),
            account: p.account.clone(),
            talent_id: p.talent_id,
            talent_name: p.talent_name.clone(),
            status: p.status.clone(),
            last_error: p.last_error.clone(),
            last_error_at: p.last_error_at,
            last_synced_at: p.last_synced_at,
            connected_at: p.connected_at,
        }
    }
}

fn audit_json(p: &Push) -> Value {
    json!({
        "user_id": p.user_id.0,
        "provider": p.provider,
        "account": p.account,
        "talent_id": p.talent_id,
        "status": p.status,
    })
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CalendarPushes {
    /// Your connected calendars.
    pub calendars: Vec<CalendarPushView>,
    /// Google calendars can be connected on this server.
    pub google: bool,
    /// Microsoft calendars can be connected on this server.
    pub microsoft: bool,
}

fn member(ctx: &Ctx) -> ApiResult<UserId> {
    ctx.user_id().ok_or_else(|| {
        ApiError::forbidden(
            "member_required",
            "a calendar is connected by its owner, signed in",
        )
    })
}

/// The caller's connected calendars (none for an API key).
pub async fn list(state: &AppState, ctx: &Ctx) -> ApiResult<CalendarPushes> {
    ctx.authorize(Permission::BookingRead)?;
    let rows = match ctx.user_id() {
        Some(user) => {
            let mut tx = begin(state, ctx).await?;
            let rows = core_db::calendar_push::list(&mut tx, Some(user)).await?;
            tx.commit().await?;
            rows
        }
        None => Vec::new(),
    };
    Ok(CalendarPushes {
        calendars: rows.iter().map(CalendarPushView::from).collect(),
        google: state.config.mailboxes.google.is_some(),
        microsoft: state.config.mailboxes.microsoft.is_some(),
    })
}

#[derive(Debug, Serialize, Deserialize)]
struct ConnectState {
    workspace: WorkspaceId,
    user: UserId,
    provider: String,
    talent: Option<Uuid>,
    nonce: String,
    exp: i64,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct StartCalendarPush {
    /// `google` or `microsoft`.
    pub provider: String,
    /// Only this talent's bookings; every booking when absent.
    pub talent_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CalendarPushStart {
    /// Send the member here; the provider sends them back to
    /// `/settings/calendar` with `code` and `state`, which the page passes
    /// to the finish call.
    pub url: String,
}

/// Where the member lets OnHold make and fill a calendar in their account.
pub async fn start(
    state: &AppState,
    ctx: &Ctx,
    req: StartCalendarPush,
) -> ApiResult<CalendarPushStart> {
    ctx.authorize(Permission::BookingRead)?;
    let user = member(ctx)?;
    if let Some(t) = req.talent_id {
        let mut tx = begin(state, ctx).await?;
        core_db::talent::get(&mut tx, t)
            .await?
            .ok_or_else(|| ApiError::invalid("talent_id", "no such talent"))?;
        tx.commit().await?;
    }
    let app = oauth_app(state, &req.provider)?;
    let flow = ConnectState {
        workspace: ctx.workspace_id,
        user,
        provider: req.provider.clone(),
        talent: req.talent_id,
        nonce: BASE64URL_NOPAD.encode(&core_domain::crypto::random_bytes::<16>()),
        exp: Utc::now().timestamp() + CONNECT_TTL_SECS,
    };
    let json = serde_json::to_vec(&flow).map_err(ApiError::internal)?;
    let sealed = BASE64URL_NOPAD.encode(&state.config.keyring.encrypt(&json, STATE_AAD));
    Ok(CalendarPushStart {
        url: app
            .authorize_exact(&sealed, &redirect_uri(state), scopes(app.provider))
            .to_string(),
    })
}

fn open_state(state: &AppState, sealed: &str) -> ApiResult<ConnectState> {
    let bad = || {
        ApiError::bad_request(
            "connect_state_invalid",
            "the connection attempt expired or was not started here; start again",
        )
    };
    let raw = BASE64URL_NOPAD
        .decode(sealed.as_bytes())
        .map_err(|_| bad())?;
    let json = state
        .config
        .keyring
        .decrypt(&raw, STATE_AAD)
        .map_err(|_| bad())?;
    let flow: ConnectState = serde_json::from_slice(&json).map_err(|_| bad())?;
    if flow.exp < Utc::now().timestamp() {
        return Err(bad());
    }
    Ok(flow)
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct FinishCalendarPush {
    /// From the provider's redirect.
    pub code: String,
    /// From the provider's redirect, as the start call made it.
    pub state: String,
}

/// Finishes connecting with what the provider sent back. Only the member
/// who started, in the same workspace, can finish.
pub async fn finish(
    state: &AppState,
    ctx: &Ctx,
    req: FinishCalendarPush,
) -> ApiResult<CalendarPushView> {
    ctx.authorize(Permission::BookingRead)?;
    let flow = open_state(state, &req.state)?;
    if flow.workspace != ctx.workspace_id || Some(flow.user) != ctx.user_id() {
        return Err(ApiError::bad_request(
            "connect_state_invalid",
            "this connection was started by someone else or in another workspace",
        ));
    }
    let app = oauth_app(state, &flow.provider)?;
    let name = match app.provider {
        Provider::Google => "Google",
        Provider::Microsoft => "Microsoft",
    };
    let cfg = &state.config.mailboxes;
    let tokens = app
        .exchange(&cfg.http, req.code.trim(), &redirect_uri(state))
        .await
        .map_err(|e| provider_error(name, e))?;
    if !tokens.scopes.is_empty()
        && !core_integrations::oauth::granted(&tokens.scopes, scopes(app.provider))
    {
        return Err(ApiError::bad_request(
            "scopes_missing",
            "OnHold needs permission to add its own calendar; connect again and allow it",
        ));
    }
    let Some(refresh_token) = tokens.refresh_token.clone() else {
        return Err(ApiError::bad_request(
            "offline_access_missing",
            format!(
                "{name} did not let OnHold stay connected; remove OnHold from your account's connected apps and connect again"
            ),
        ));
    };
    let account = match app.provider {
        Provider::Google => {
            core_integrations::calendars::GoogleBusy::new(
                cfg.http.clone(),
                cfg.google_api.clone(),
                tokens.access_token.clone(),
            )
            .email()
            .await
        }
        Provider::Microsoft => {
            core_integrations::graph::Graph::new(
                cfg.http.clone(),
                cfg.graph_api.clone(),
                tokens.access_token.clone(),
                None,
            )
            .address()
            .await
        }
    }
    .map_err(|e| provider_error(name, e))?
    .trim()
    .to_ascii_lowercase();
    let grant = Grant {
        refresh_token,
        access_token: tokens.access_token.clone(),
        expires_at: tokens.expires_at,
    };
    let provider = app.provider.as_str();
    let mut tx = begin(state, ctx).await?;
    if let Some(t) = flow.talent {
        core_db::talent::get(&mut tx, t)
            .await?
            .ok_or_else(|| ApiError::invalid("talent_id", "no such talent"))?;
    }
    let id =
        match core_db::calendar_push::live_for(&mut tx, flow.user, provider, &account, flow.talent)
            .await?
        {
            Some(p) => {
                core_db::calendar_push::set_secret(&mut tx, p.id, &seal(state, p.id, &grant)?)
                    .await?;
                core_db::calendar_push::revive(&mut tx, p.id).await?;
                record(
                    &mut tx,
                    ctx,
                    Change {
                        topic: "calendar_push.reconnected",
                        resource: ENTITY,
                        object_id: Some(p.id),
                        before: Some(audit_json(&p)),
                        after: None,
                        event_extra: None,
                    },
                )
                .await?;
                p.id
            }
            None => {
                if core_db::calendar_push::count_live(&mut tx, flow.user).await? >= MAX_PER_MEMBER {
                    return Err(ApiError::conflict(
                        "too_many_calendars",
                        format!("you have {MAX_PER_MEMBER} calendars connected; remove one first"),
                    ));
                }
                let id = core_domain::ids::new_id();
                core_db::calendar_push::insert(
                    &mut tx,
                    core_db::calendar_push::NewPush {
                        id,
                        user_id: flow.user,
                        talent_id: flow.talent,
                        provider,
                        account: &account,
                        secret: &seal(state, id, &grant)?,
                    },
                )
                .await?;
                let saved = core_db::calendar_push::get(&mut tx, id)
                    .await?
                    .ok_or(ApiError::NotFound)?;
                record(
                    &mut tx,
                    ctx,
                    Change {
                        topic: "calendar_push.connected",
                        resource: ENTITY,
                        object_id: Some(id),
                        before: None,
                        after: Some(audit_json(&saved)),
                        event_extra: None,
                    },
                )
                .await?;
                id
            }
        };
    queue_full(&mut tx, ctx.workspace_id, id).await?;
    let view = core_db::calendar_push::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(CalendarPushView::from(&view))
}

async fn load_own(tx: &mut Tx, ctx: &Ctx, id: Uuid) -> ApiResult<Push> {
    core_db::calendar_push::lock(tx, id)
        .await?
        .filter(|p| p.disconnected_at.is_none() && Some(p.user_id) == ctx.user_id())
        .ok_or(ApiError::NotFound)
}

/// Writes the calendar in full now rather than at its next turn.
pub async fn sync_now(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<CalendarPushView> {
    ctx.authorize(Permission::BookingRead)?;
    let mut tx = begin(state, ctx).await?;
    let p = load_own(&mut tx, ctx, id).await?;
    if p.status == "reconnect" {
        return Err(ApiError::conflict(
            "reconnect_required",
            "this calendar needs to be connected again first",
        ));
    }
    if core_db::calendar_push::sync_now(&mut tx, id)
        .await?
        .is_some()
    {
        queue_full(&mut tx, ctx.workspace_id, id).await?;
    }
    let after = core_db::calendar_push::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(CalendarPushView::from(&after))
}

/// Stops writing, removes OnHold's calendar from the account, and asks
/// Google to forget the grant unless another connection uses the account.
pub async fn disconnect(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::BookingRead)?;
    let mut tx = begin(state, ctx).await?;
    let before = load_own(&mut tx, ctx, id).await?;
    let grant = open(state, &before).ok();
    core_db::calendar_push::disconnect(&mut tx, id).await?;
    let in_use =
        core_db::integrations::account_in_use(&mut tx, &before.provider, &before.account, id)
            .await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "calendar_push.disconnected",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(audit_json(&before)),
            after: None,
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    // Best effort, after the fact: OnHold has already forgotten the grant.
    if let Some(grant) = grant {
        clean_up(state, &before, grant, in_use).await;
    }
    Ok(())
}

/// Removes OnHold's calendar from the account and revokes the grant unless
/// it is still in use. Failures are logged: the grant is forgotten anyway.
async fn clean_up(state: &AppState, p: &Push, grant: Grant, in_use: bool) -> &'static str {
    let Ok(app) = oauth_app(state, &p.provider) else {
        return "forgotten; this server no longer connects to that provider";
    };
    let http = &state.config.mailboxes.http;
    let token = if grant.expires_at - Duration::seconds(60) > Utc::now() {
        Some(grant.access_token.clone())
    } else {
        app.refresh(http, &grant.refresh_token)
            .await
            .ok()
            .map(|t| t.access_token)
    };
    let mut outcome = "forgotten";
    if let (Some(token), Some(calendar)) = (token, p.remote_calendar.as_deref()) {
        match writer(state, &p.provider, token)
            .delete_calendar(calendar)
            .await
        {
            Ok(()) => outcome = "calendar removed",
            Err(e) => {
                tracing::warn!(push_id = %p.id, error = %e, "removing OnHold's calendar failed");
                outcome = "removing the calendar failed";
            }
        }
    }
    if !in_use && let Err(e) = app.revoke(http, &grant.refresh_token).await {
        tracing::warn!(push_id = %p.id, error = %e, "revoking a calendar grant failed");
    }
    outcome
}

/// Disconnects every calendar of a member leaving, or of a closing
/// workspace (`None`). The worker then removes OnHold's calendars, revokes
/// the grants and forgets them.
pub(crate) async fn disconnect_all(
    tx: &mut Tx,
    ctx: &Ctx,
    user: Option<UserId>,
    reason: &str,
) -> ApiResult<()> {
    for id in core_db::calendar_push::disconnect_all(tx, user).await? {
        record(
            tx,
            ctx,
            Change {
                topic: "calendar_push.disconnected",
                resource: ENTITY,
                object_id: Some(id),
                before: None,
                after: None,
                event_extra: Some(json!({ "reason": reason })),
            },
        )
        .await?;
    }
    Ok(())
}

/// The worker's `calendar.push_revoke`: cleans up a connection left by
/// [`disconnect_all`].
pub async fn revoke(state: &AppState, ws: WorkspaceId, id: Uuid) -> ApiResult<&'static str> {
    let ctx = Ctx::system(ws);
    let mut tx = begin(state, &ctx).await?;
    let Some(p) = core_db::calendar_push::to_revoke(&mut tx, id).await? else {
        tx.commit().await?;
        return Ok("nothing to revoke");
    };
    let grant = open(state, &p).ok();
    let in_use =
        core_db::integrations::account_in_use(&mut tx, &p.provider, &p.account, id).await?;
    core_db::calendar_push::forget_grant(&mut tx, id).await?;
    tx.commit().await?;
    Ok(match grant {
        Some(g) => clean_up(state, &p, g, in_use).await,
        None => "forgotten",
    })
}

// Queueing --------------------------------------------------------------------------------

async fn queue_full(tx: &mut Tx, ws: WorkspaceId, id: Uuid) -> ApiResult<()> {
    let now = Utc::now();
    let payload = json!({ "push_id": id, "at": now });
    core_db::jobs::enqueue(
        tx,
        core_db::jobs::NewJob::new(Some(ws), JOB_WRITE, &payload).dedupe(&full_key(id, now)),
    )
    .await?;
    Ok(())
}

/// One full write per connection and due time.
pub fn full_key(id: Uuid, at: DateTime<Utc>) -> String {
    format!("{id}@{}", at.to_rfc3339())
}

/// A booking changed (the event dispatcher): its event is written into
/// every connected calendar.
pub(crate) async fn booking_changed(
    tx: &mut Tx,
    ws: WorkspaceId,
    topic: &str,
    payload: &Value,
    event_id: Uuid,
) -> ApiResult<()> {
    if !topic.starts_with("booking.") {
        return Ok(());
    }
    let Some(booking) = payload
        .get("object_id")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<Uuid>().ok())
    else {
        return Ok(());
    };
    for push in core_db::calendar_push::writable(tx).await? {
        let payload = json!({ "push_id": push, "booking_id": booking });
        core_db::jobs::enqueue(
            tx,
            core_db::jobs::NewJob::new(Some(ws), JOB_WRITE, &payload)
                .dedupe(&format!("{push}:{booking}:{event_id}")),
        )
        .await?;
    }
    Ok(())
}

// Writing ---------------------------------------------------------------------------------

fn writer(state: &AppState, provider: &str, token: String) -> CalendarWriter {
    let cfg = &state.config.mailboxes;
    match provider {
        "microsoft" => CalendarWriter::microsoft(cfg.http.clone(), cfg.graph_api.clone(), token),
        _ => CalendarWriter::google(cfg.http.clone(), cfg.google_api.clone(), token),
    }
}

/// A fresh access token, stored when refreshed.
async fn access_token(
    state: &AppState,
    ws: WorkspaceId,
    p: &Push,
    grant: &Grant,
    force: bool,
) -> Result<String, ProviderError> {
    if !force && grant.expires_at - Duration::seconds(60) > Utc::now() {
        return Ok(grant.access_token.clone());
    }
    let app = oauth_app(state, &p.provider).map_err(|_| {
        ProviderError::Revoked("this server no longer connects to that provider".into())
    })?;
    let fresh = app
        .refresh(&state.config.mailboxes.http, &grant.refresh_token)
        .await?;
    store_refreshed(state, ws, p.id, &grant.refresh_token, &fresh)
        .await
        .map_err(|e| ProviderError::Invalid(e.to_string()))?;
    Ok(fresh.access_token)
}

async fn store_refreshed(
    state: &AppState,
    ws: WorkspaceId,
    id: Uuid,
    old_refresh: &str,
    fresh: &Tokens,
) -> ApiResult<()> {
    let grant = Grant {
        refresh_token: fresh
            .refresh_token
            .clone()
            .unwrap_or_else(|| old_refresh.to_owned()),
        access_token: fresh.access_token.clone(),
        expires_at: fresh.expires_at,
    };
    let secret = seal(state, id, &grant)?;
    let mut tx = begin(state, &Ctx::system(ws)).await?;
    core_db::calendar_push::set_secret(&mut tx, id, &secret).await?;
    tx.commit().await?;
    Ok(())
}

/// What OnHold writes for a booking into this connection, if anything.
fn wanted(
    state: &AppState,
    p: &Push,
    b: &super::bookings::BookingView,
    now: DateTime<Utc>,
) -> Option<EventBody> {
    let shown = matches!(
        b.status,
        BookingStatus::Hold
            | BookingStatus::Pending
            | BookingStatus::Confirmed
            | BookingStatus::Completed
    );
    let mine = p
        .talent_id
        .is_none_or(|t| b.talent.iter().any(|n| n.id == t));
    if !shown || !mine || b.ends_at < now - PAST || b.starts_at > now + AHEAD {
        return None;
    }
    let host = state
        .config
        .public_base_url
        .host_str()
        .unwrap_or("onhold.invalid")
        .to_owned();
    let url = state.config.app_url(&format!("/bookings/{}", b.id), &[]);
    let e = super::calendar_feeds::Event::from_booking(b, &host, url);
    Some(EventBody {
        summary: e.summary,
        description: e.description,
        location: e.location,
        starts_at: e.starts_at,
        ends_at: e.ends_at,
        all_day: e.all_day,
        tentative: e.status == "TENTATIVE",
        url: e.url,
    })
}

fn content_hash(calendar: &str, e: &EventBody) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(calendar.as_bytes());
    h.update(format!("{e:?}").as_bytes());
    h.finalize().to_vec()
}

/// Bookings as they should appear: all in the window, or one.
async fn bookings(
    tx: &mut Tx,
    one: Option<Uuid>,
    now: DateTime<Utc>,
) -> ApiResult<Vec<super::bookings::BookingView>> {
    let rows = match one {
        Some(id) => core_db::bookings::get(tx, id).await?.into_iter().collect(),
        None => {
            let statuses = [
                BookingStatus::Hold,
                BookingStatus::Pending,
                BookingStatus::Confirmed,
                BookingStatus::Completed,
            ];
            core_db::bookings::list(
                tx,
                core_db::bookings::Filter {
                    list: core_db::crm::ListParams {
                        q: None,
                        searchable: &[],
                        tag: None,
                        custom: None,
                        sort: "starts_at",
                        sort_custom: None,
                        desc: false,
                        offset: 0,
                        limit: 5000,
                    },
                    from: Some(now - PAST),
                    to: Some(now + AHEAD),
                    statuses: &statuses,
                    talent_id: None,
                    organisation_id: None,
                    contact_id: None,
                    enquiry_id: None,
                },
            )
            .await?
            .0
        }
    };
    let ids: Vec<Uuid> = rows.iter().map(|b| b.id).collect();
    let talent = core_db::bookings::talent_of(tx, &ids).await?;
    Ok(rows
        .into_iter()
        .map(|b| {
            let names = super::links::talent_by_owner(talent.clone(), b.id);
            super::bookings::view(b, names)
        })
        .collect())
}

/// What a write did, for the job's log.
#[derive(Debug, Default)]
pub struct WriteOutcome {
    pub written: usize,
    pub removed: usize,
    pub unchanged: usize,
    pub more: bool,
    pub skipped: Option<&'static str>,
}

impl std::fmt::Display for WriteOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.skipped {
            Some(why) => write!(f, "skipped: {why}"),
            None => write!(
                f,
                "{} written, {} removed, {} unchanged{}",
                self.written,
                self.removed,
                self.unchanged,
                if self.more { ", more to come" } else { "" }
            ),
        }
    }
}

/// The worker's `calendar.write`: brings one connection's calendar in
/// line with the bookings, all of them or one. Provider failures are
/// recorded on the connection and retried with growing waits.
pub async fn write(
    state: &AppState,
    ws: WorkspaceId,
    id: Uuid,
    booking: Option<Uuid>,
) -> ApiResult<WriteOutcome> {
    let ctx = Ctx::system(ws);
    let mut tx = begin(state, &ctx).await?;
    if !core_db::calendar_push::claim(&mut tx, id, WRITE_LEASE_SECS).await? {
        // Another write holds it: a full one soon picks this booking up.
        if booking.is_some() {
            core_db::calendar_push::sync_now(&mut tx, id).await?;
        }
        tx.commit().await?;
        return Ok(WriteOutcome {
            skipped: Some("disconnected, waiting to be connected again, or being written"),
            ..WriteOutcome::default()
        });
    }
    let p = core_db::calendar_push::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // Only bookings its owner may see: a member who left takes theirs away.
    let may_read = match core_db::members::role_of(&mut tx, p.user_id).await? {
        Some(role) => core_db::workspaces::role_permissions(&mut tx, role)
            .await?
            .contains(&Permission::BookingRead),
        None => false,
    };
    if !may_read {
        disconnect_all(&mut tx, &ctx, Some(p.user_id), "member_removed").await?;
        tx.commit().await?;
        return Ok(WriteOutcome {
            skipped: Some("its owner can no longer see bookings"),
            ..WriteOutcome::default()
        });
    }
    let now = Utc::now();
    let want = bookings(&mut tx, booking, now).await?;
    let have = core_db::calendar_push::written(&mut tx, id, booking).await?;
    let tz = core_db::workspaces::get(&mut tx, ws)
        .await?
        .map(|w| (w.name, w.default_timezone));
    tx.commit().await?;

    let grant = match open(state, &p) {
        Ok(g) => g,
        Err(e) => {
            failed(
                state,
                &ctx,
                &p,
                ProviderError::Invalid(e.to_string()),
                booking.is_none(),
            )
            .await?;
            return Ok(WriteOutcome::default());
        }
    };
    let mut forced = false;
    loop {
        let result = async {
            let token = access_token(state, ws, &p, &grant, forced).await?;
            let w = writer(state, &p.provider, token);
            apply(state, &ctx, &p, &w, &want, &have, booking, now, tz.clone()).await
        }
        .await;
        match result {
            Ok(outcome) => {
                let mut tx = begin(state, &ctx).await?;
                if booking.is_none() {
                    let next = if outcome.more {
                        Utc::now()
                    } else {
                        Utc::now() + FULL_EVERY
                    };
                    core_db::calendar_push::synced(&mut tx, id, next, WRITE_LEASE_SECS).await?;
                    if outcome.more {
                        queue_full(&mut tx, ws, id).await?;
                    }
                } else {
                    core_db::calendar_push::release(&mut tx, id).await?;
                }
                tx.commit().await?;
                return Ok(outcome);
            }
            Err(ProviderError::Unauthorized) if !forced => forced = true,
            // A calendar just made and already refused is a failure, not
            // one to make again.
            Err(e) if calendar_events::calendar_gone(&e) && p.remote_calendar.is_some() => {
                // OnHold's calendar was deleted in the account: make it
                // again and write everything afresh.
                let mut tx = begin(state, &ctx).await?;
                core_db::calendar_push::set_remote_calendar(&mut tx, id, None).await?;
                core_db::calendar_push::clear_events(&mut tx, id).await?;
                core_db::calendar_push::synced(&mut tx, id, Utc::now(), WRITE_LEASE_SECS).await?;
                queue_full(&mut tx, ws, id).await?;
                tx.commit().await?;
                return Ok(WriteOutcome {
                    skipped: Some("OnHold's calendar was gone; it is made again"),
                    ..WriteOutcome::default()
                });
            }
            Err(e) => {
                failed(state, &ctx, &p, e, booking.is_none()).await?;
                return Ok(WriteOutcome::default());
            }
        }
    }
}

/// Writes the differences: events for bookings wanted, removed for those
/// no longer wanted. Each write is recorded as it happens, so a failure
/// part way loses nothing.
/// A database failure part way through writing: tried again later.
fn invalid(e: impl std::fmt::Display) -> ProviderError {
    ProviderError::Invalid(e.to_string())
}

#[allow(clippy::too_many_arguments)]
async fn apply(
    state: &AppState,
    ctx: &Ctx,
    p: &Push,
    w: &CalendarWriter,
    list: &[super::bookings::BookingView],
    have: &[core_db::calendar_push::Written],
    one: Option<Uuid>,
    now: DateTime<Utc>,
    workspace: Option<(String, String)>,
) -> Result<WriteOutcome, ProviderError> {
    let mut out = WriteOutcome::default();
    let (ws_name, tz) = workspace.unwrap_or_else(|| ("OnHold".into(), "UTC".into()));
    let calendar = match &p.remote_calendar {
        Some(c) => c.clone(),
        None => {
            let name = match &p.talent_name {
                Some(t) => format!("OnHold · {ws_name} · {t}"),
                None => format!("OnHold · {ws_name}"),
            };
            let made = w.create_calendar(&name, &tz).await?;
            let mut tx = begin(state, ctx).await.map_err(invalid)?;
            core_db::calendar_push::set_remote_calendar(&mut tx, p.id, Some(&made))
                .await
                .map_err(invalid)?;
            tx.commit().await.map_err(invalid)?;
            made
        }
    };
    let held: std::collections::HashMap<Uuid, &core_db::calendar_push::Written> =
        have.iter().map(|h| (h.booking_id, h)).collect();
    let mut seen = std::collections::HashSet::new();
    let mut calls = 0;
    for b in list {
        let Some(event) = wanted(state, p, b, now) else {
            continue;
        };
        seen.insert(b.id);
        let hash = content_hash(&calendar, &event);
        let existing = held.get(&b.id);
        if existing.is_some_and(|h| h.content_hash == hash) {
            out.unchanged += 1;
            continue;
        }
        if calls >= MAX_WRITES_PER_RUN {
            out.more = true;
            break;
        }
        calls += 1;
        let remote = w
            .put(&calendar, existing.map(|h| h.remote_id.as_str()), &event)
            .await?;
        let mut tx = begin(state, ctx).await.map_err(invalid)?;
        core_db::calendar_push::record_written(&mut tx, p.id, b.id, &remote, &hash)
            .await
            .map_err(invalid)?;
        tx.commit().await.map_err(invalid)?;
        out.written += 1;
    }
    // Events whose bookings are no longer wanted: cancelled, removed,
    // moved to another talent. Past bookings that only left the window
    // stay, as a record.
    let mut gone = Vec::new();
    for h in have.iter().filter(|h| !seen.contains(&h.booking_id)) {
        if one.is_none() || one == Some(h.booking_id) {
            gone.push(h);
        }
    }
    if !gone.is_empty() {
        let mut tx = begin(state, ctx).await.map_err(invalid)?;
        let ids: Vec<Uuid> = gone.iter().map(|h| h.booking_id).collect();
        let mut still = Vec::new();
        for id in &ids {
            if let Some(b) = bookings(&mut tx, Some(*id), now)
                .await
                .map_err(invalid)?
                .into_iter()
                .next()
            {
                still.push(b);
            }
        }
        tx.commit().await.map_err(invalid)?;
        for h in gone {
            let past_but_kept = still.iter().any(|b| {
                b.id == h.booking_id
                    && b.ends_at < now - PAST
                    && wanted(state, p, b, b.starts_at).is_some()
            });
            if past_but_kept {
                continue;
            }
            if calls >= MAX_WRITES_PER_RUN {
                out.more = true;
                break;
            }
            calls += 1;
            w.delete(&calendar, &h.remote_id).await?;
            let mut tx = begin(state, ctx).await.map_err(invalid)?;
            core_db::calendar_push::forget_written(&mut tx, p.id, h.booking_id)
                .await
                .map_err(invalid)?;
            tx.commit().await.map_err(invalid)?;
            out.removed += 1;
        }
    }
    Ok(out)
}

/// Records a failed write and when to try in full again: soon at first,
/// then less often (5 minutes doubling to 6 hours). A grant that no longer
/// works waits for the member to connect again.
async fn failed(
    state: &AppState,
    ctx: &Ctx,
    p: &Push,
    err: ProviderError,
    _full: bool,
) -> ApiResult<()> {
    let wait = match &err {
        ProviderError::RateLimited {
            retry_after: Some(s),
        } => Duration::seconds((*s).clamp(30, 6 * 3600) as i64),
        _ => {
            let doubling = 2i64.saturating_pow(p.failures.clamp(0, 16) as u32);
            Duration::minutes(5 * doubling).min(Duration::hours(6))
        }
    };
    let (status, next) = match &err {
        ProviderError::Revoked(_) => ("reconnect", None),
        e if e.is_temporary() && p.failures + 1 < QUIET_FAILURES => (
            if p.status == "failing" {
                "failing"
            } else {
                "active"
            },
            Some(Utc::now() + wait),
        ),
        _ => ("failing", Some(Utc::now() + wait)),
    };
    let message = err.to_string();
    tracing::warn!(push_id = %p.id, provider = %p.provider, error = %message, "writing bookings into a calendar failed");
    let mut tx = begin(state, ctx).await?;
    core_db::calendar_push::failed(&mut tx, p.id, status, &message, next).await?;
    if status != p.status {
        record(
            &mut tx,
            ctx,
            Change {
                topic: "calendar_push.health_changed",
                resource: ENTITY,
                object_id: Some(p.id),
                before: Some(json!({ "status": p.status })),
                after: Some(json!({ "status": status })),
                event_extra: None,
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
