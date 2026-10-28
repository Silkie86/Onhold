//! Talent's own calendars, busy times only. Someone at the agency makes a
//! link for a talent and sends it to them (or to each member of a band).
//! Opening it needs no OnHold account: they connect their Google or
//! Microsoft calendar with a grant that shows only when they are busy, or
//! paste a private iCal link. The worker reads the year ahead every few
//! minutes and keeps the busy times, never titles, where clash checks and
//! the calendar count them like the talent's availability.

use chrono::{DateTime, Duration, Utc};
use core_db::Tx;
use core_db::talent_calendars::{Calendar, Link};
use core_domain::crypto::{hash_token, new_token};
use core_domain::workspace::WorkspaceStatus;
use core_domain::{Ctx, Permission, WorkspaceId};
use core_integrations::calendars::{self, Busy, GoogleBusy, GraphBusy};
use core_integrations::oauth::{OAuthApp, Provider, Tokens};
use core_integrations::provider::ProviderError;
use data_encoding::BASE64URL_NOPAD;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::integrations::provider_error;
use super::{Change, begin, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

pub const ENTITY: &str = "talent_calendar";
/// Reads one calendar's busy times (`calendar_id`, `at`: when it was due).
pub const JOB_READ: &str = "calendar.read";
/// Revokes and forgets the grant of a calendar disconnected along with
/// others (`calendar_id`).
pub const JOB_REVOKE: &str = "calendar.revoke";

/// How long a link works.
const LINK_DAYS: i64 = 14;
/// Calendars one talent may have connected (a band's members each).
pub const MAX_PER_TALENT: i64 = 12;
const STATE_AAD: &[u8] = b"calendar_connect_state";
/// How long the talent has to finish on the provider's side.
const CONNECT_TTL_SECS: i64 = 30 * 60;
/// How long one read may hold a calendar before another may start.
const READ_LEASE_SECS: f64 = 5.0 * 60.0;
/// What is read: from yesterday to a year ahead.
const PAST: Duration = Duration::days(1);
const AHEAD: Duration = Duration::days(365);
/// Failures in a row before a calendar shows as failing.
const QUIET_FAILURES: i32 = 3;
/// iCal files read at once across the server, off the async threads, and
/// how long one may take.
static ICAL_READS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
const ICAL_READ_SECS: u64 = 20;
/// Scopes a busy-only grant may carry: anything more (a mailbox granted to
/// the same app, say) is refused.
const ALLOWED_SCOPES: &[&str] = &[
    "openid",
    "email",
    "profile",
    "offline_access",
    "user.read",
    "calendars.readbasic",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/calendar.freebusy",
];

/// What a connection reads with.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Grant {
    OAuth {
        refresh_token: String,
        access_token: String,
        expires_at: DateTime<Utc>,
    },
    /// A private iCal link is itself the secret.
    Ical { url: String },
}

fn grant_aad(id: Uuid) -> Vec<u8> {
    [b"talent_calendar:".as_slice(), id.as_bytes()].concat()
}

fn seal(state: &AppState, id: Uuid, grant: &Grant) -> ApiResult<Vec<u8>> {
    let json = serde_json::to_vec(grant).map_err(ApiError::internal)?;
    Ok(state.config.keyring.encrypt(&json, &grant_aad(id)))
}

fn open(state: &AppState, c: &Calendar) -> ApiResult<Grant> {
    let plain = state
        .config
        .keyring
        .decrypt(&c.secret, &grant_aad(c.id))
        .map_err(ApiError::internal)?;
    serde_json::from_slice(&plain).map_err(ApiError::internal)
}

// Views -----------------------------------------------------------------------------------

/// A connected calendar as the agency sees it. Never carries the grant.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TalentCalendarView {
    pub id: Uuid,
    pub talent_id: Uuid,
    /// `google`, `microsoft` or `ical`.
    pub provider: String,
    /// The account's address, or the iCal link's host.
    pub account: String,
    /// A band member's name, as they gave it.
    pub person: Option<String>,
    /// `active`, `failing` (being retried) or `reconnect` (the talent needs
    /// to connect it again).
    pub status: String,
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub connected_at: DateTime<Utc>,
}

impl From<&Calendar> for TalentCalendarView {
    fn from(c: &Calendar) -> Self {
        Self {
            id: c.id,
            talent_id: c.talent_id,
            provider: c.provider.clone(),
            account: c.account.clone(),
            person: c.person.clone(),
            status: c.status.clone(),
            last_error: c.last_error.clone(),
            last_error_at: c.last_error_at,
            last_synced_at: c.last_synced_at,
            connected_at: c.connected_at,
        }
    }
}

fn audit_json(c: &Calendar) -> Value {
    json!({
        "talent_id": c.talent_id,
        "provider": c.provider,
        "account": c.account,
        "person": c.person,
        "status": c.status,
    })
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConnectLinkView {
    pub id: Uuid,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

impl From<Link> for ConnectLinkView {
    fn from(l: Link) -> Self {
        Self {
            id: l.id,
            created_by: l.created_by,
            created_at: l.created_at,
            expires_at: l.expires_at,
            last_used_at: l.last_used_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TalentCalendars {
    pub calendars: Vec<TalentCalendarView>,
    /// Links still working. Each is shown in full only once, when made.
    pub links: Vec<ConnectLinkView>,
    /// Google calendars can be connected on this server.
    pub google: bool,
    /// Microsoft calendars can be connected on this server.
    pub microsoft: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CreatedConnectLink {
    pub link: ConnectLinkView,
    /// The link to send the talent. Shown once.
    pub url: String,
}

// For the agency ------------------------------------------------------------------------------

async fn talent_exists(tx: &mut Tx, talent_id: Uuid) -> ApiResult<()> {
    core_db::talent::get(tx, talent_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(())
}

/// A talent's connected calendars and working links.
pub async fn list(state: &AppState, ctx: &Ctx, talent_id: Uuid) -> ApiResult<TalentCalendars> {
    ctx.authorize(Permission::TalentRead)?;
    let mut tx = begin(state, ctx).await?;
    talent_exists(&mut tx, talent_id).await?;
    let calendars = core_db::talent_calendars::list(&mut tx, Some(talent_id)).await?;
    let links = core_db::talent_calendars::links(&mut tx, talent_id).await?;
    tx.commit().await?;
    Ok(TalentCalendars {
        calendars: calendars.iter().map(TalentCalendarView::from).collect(),
        links: links.into_iter().map(Into::into).collect(),
        google: state.config.mailboxes.google.is_some(),
        microsoft: state.config.mailboxes.microsoft.is_some(),
    })
}

/// Makes a link for the talent to connect their calendar through. It works
/// for two weeks, for as many of the talent's people as need it.
pub async fn create_link(
    state: &AppState,
    ctx: &Ctx,
    talent_id: Uuid,
) -> ApiResult<CreatedConnectLink> {
    ctx.authorize(Permission::TalentWrite)?;
    let mut tx = begin(state, ctx).await?;
    talent_exists(&mut tx, talent_id).await?;
    let user = ctx.user_id().ok_or_else(|| {
        ApiError::forbidden(
            "person_required",
            "calendar links are made by a person, not a key",
        )
    })?;
    let token = new_token();
    let id = core_domain::ids::new_id();
    let expires_at = Utc::now() + Duration::days(LINK_DAYS);
    core_db::talent_calendars::insert_link(
        &mut tx,
        id,
        talent_id,
        &hash_token(&token),
        user,
        expires_at,
    )
    .await?;
    let link = core_db::talent_calendars::links(&mut tx, talent_id)
        .await?
        .into_iter()
        .find(|l| l.id == id)
        .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "calendar_connect_link.created",
            resource: "calendar_connect_link",
            object_id: Some(id),
            before: None,
            after: Some(json!({ "talent_id": talent_id, "expires_at": expires_at })),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(CreatedConnectLink {
        link: link.into(),
        url: state
            .config
            .app_url(&format!("/connect-calendar/{token}"), &[]),
    })
}

/// Stops a link working. Calendars connected through it carry on.
pub async fn revoke_link(
    state: &AppState,
    ctx: &Ctx,
    talent_id: Uuid,
    link_id: Uuid,
) -> ApiResult<()> {
    ctx.authorize(Permission::TalentWrite)?;
    let mut tx = begin(state, ctx).await?;
    if !core_db::talent_calendars::revoke_link(&mut tx, talent_id, link_id).await? {
        return Err(ApiError::NotFound);
    }
    record(
        &mut tx,
        ctx,
        Change {
            topic: "calendar_connect_link.revoked",
            resource: "calendar_connect_link",
            object_id: Some(link_id),
            before: Some(json!({ "talent_id": talent_id })),
            after: None,
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn load_for(tx: &mut Tx, talent_id: Uuid, id: Uuid) -> ApiResult<Calendar> {
    core_db::talent_calendars::lock(tx, id)
        .await?
        .filter(|c| c.talent_id == talent_id && c.disconnected_at.is_none())
        .ok_or(ApiError::NotFound)
}

fn oauth_app<'a>(state: &'a AppState, provider: &str) -> ApiResult<&'a OAuthApp> {
    let app = match provider {
        "google" => state.config.mailboxes.google.as_ref(),
        "microsoft" => state.config.mailboxes.microsoft.as_ref(),
        _ => {
            return Err(ApiError::invalid(
                "provider",
                "use google or microsoft, or paste an iCal link",
            ));
        }
    };
    app.ok_or_else(|| {
        ApiError::conflict(
            "provider_not_available",
            format!("{provider} calendars cannot be connected on this server"),
        )
    })
}

/// Stops reading a calendar and forgets its busy times. Google is asked to
/// forget the grant too.
pub async fn disconnect(state: &AppState, ctx: &Ctx, talent_id: Uuid, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::TalentWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = load_for(&mut tx, talent_id, id).await?;
    let grant = open(state, &before).ok();
    core_db::talent_calendars::disconnect(&mut tx, id).await?;
    // Revoking a Google grant ends every grant OnHold holds for that
    // account, mailboxes included, so it is left alone while another
    // connection (here or in another workspace) still uses it.
    let in_use =
        core_db::integrations::account_in_use(&mut tx, &before.provider, &before.account, id)
            .await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "talent_calendar.disconnected",
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
    if let (Some(Grant::OAuth { refresh_token, .. }), Ok(app), false) =
        (grant, oauth_app(state, &before.provider), in_use)
        && let Err(e) = app
            .revoke(&state.config.mailboxes.http, &refresh_token)
            .await
    {
        tracing::warn!(calendar_id = %id, error = %e, "revoking a calendar grant failed");
    }
    Ok(())
}

/// Disconnects every calendar of a talent being removed, or of a closing
/// workspace (`None`), and stops their links. Their Google grants are
/// revoked by the worker once this commits.
pub(crate) async fn disconnect_all(
    tx: &mut Tx,
    ctx: &Ctx,
    talent_id: Option<Uuid>,
    reason: &str,
) -> ApiResult<()> {
    core_db::talent_calendars::revoke_links(tx, talent_id).await?;
    for id in core_db::talent_calendars::disconnect_all(tx, talent_id).await? {
        record(
            tx,
            ctx,
            Change {
                topic: "talent_calendar.disconnected",
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

/// Revokes a grant left by [`disconnect_all`] (the worker's
/// `calendar.revoke`), unless another connection still uses the account,
/// and forgets it.
pub async fn revoke(state: &AppState, ws: WorkspaceId, id: Uuid) -> ApiResult<&'static str> {
    let ctx = Ctx::system(ws);
    let mut tx = begin(state, &ctx).await?;
    let Some(c) = core_db::talent_calendars::to_revoke(&mut tx, id).await? else {
        tx.commit().await?;
        return Ok("nothing to revoke");
    };
    let grant = open(state, &c).ok();
    let in_use =
        core_db::integrations::account_in_use(&mut tx, &c.provider, &c.account, id).await?;
    core_db::talent_calendars::forget_grant(&mut tx, id).await?;
    tx.commit().await?;
    match (grant, oauth_app(state, &c.provider), in_use) {
        (_, _, true) => Ok("forgotten; the account is still connected elsewhere"),
        (Some(Grant::OAuth { refresh_token, .. }), Ok(app), false) => {
            if let Err(e) = app
                .revoke(&state.config.mailboxes.http, &refresh_token)
                .await
            {
                tracing::warn!(calendar_id = %id, error = %e, "revoking a calendar grant failed");
                return Ok("forgotten; revoking failed");
            }
            Ok("revoked and forgotten")
        }
        _ => Ok("forgotten"),
    }
}

/// Reads a calendar now rather than at its next turn.
pub async fn sync_now(
    state: &AppState,
    ctx: &Ctx,
    talent_id: Uuid,
    id: Uuid,
) -> ApiResult<TalentCalendarView> {
    ctx.authorize(Permission::TalentWrite)?;
    let mut tx = begin(state, ctx).await?;
    let c = load_for(&mut tx, talent_id, id).await?;
    if c.status == "reconnect" {
        return Err(ApiError::conflict(
            "reconnect_required",
            "the talent needs to connect this calendar again; send them a new link",
        ));
    }
    if let Some(at) = core_db::talent_calendars::poll_now(&mut tx, id).await? {
        queue_read(&mut tx, ctx.workspace_id, id, at).await?;
    }
    let after = core_db::talent_calendars::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(TalentCalendarView::from(&after))
}

async fn queue_read(tx: &mut Tx, ws: WorkspaceId, id: Uuid, at: DateTime<Utc>) -> ApiResult<()> {
    let payload = json!({ "calendar_id": id, "at": at });
    core_db::jobs::enqueue(
        tx,
        core_db::jobs::NewJob::new(Some(ws), JOB_READ, &payload).dedupe(&read_key(id, at)),
    )
    .await?;
    Ok(())
}

/// One read per calendar and due time.
pub fn read_key(id: Uuid, at: DateTime<Utc>) -> String {
    format!("{id}@{}", at.to_rfc3339())
}

// For the talent, through their link ------------------------------------------------------------

/// What the talent sees on opening their link.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PublicCalendarConnect {
    pub workspace_name: String,
    /// The workspace's logo and colours, for a page in its own style.
    #[schema(value_type = Object)]
    pub branding: Value,
    pub talent_name: String,
    pub google: bool,
    pub microsoft: bool,
    /// Calendars already connected for this talent, by whose they are.
    pub connected: Vec<PublicConnected>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PublicConnected {
    /// `google`, `microsoft` or `ical`.
    pub provider: String,
    pub person: Option<String>,
    /// `active`, `failing` or `reconnect` (it needs connecting again).
    pub status: String,
}

/// The link's workspace, talent and link id, with a context acting for the
/// talent (as the system, with their address recorded).
async fn open_link(
    state: &AppState,
    token: &str,
    meta: core_domain::authz::RequestMeta,
) -> ApiResult<(Tx, Ctx, Uuid, Uuid)> {
    let mut tx = core_db::begin(&state.db, None, None).await?;
    let found = core_db::talent_calendars::find_link(&mut tx, &hash_token(token))
        .await?
        .ok_or(ApiError::NotFound)?;
    core_db::set_context(&mut tx, Some(found.workspace_id), None).await?;
    workspace_working(&mut tx, found.workspace_id).await?;
    let mut ctx = Ctx::system(found.workspace_id);
    ctx.meta = meta;
    Ok((tx, ctx, found.id, found.talent_id))
}

/// A closed or suspended workspace's links stop working, as if revoked.
async fn workspace_working(
    tx: &mut Tx,
    ws: WorkspaceId,
) -> ApiResult<core_db::workspaces::Workspace> {
    core_db::workspaces::get(tx, ws)
        .await?
        .filter(|w| matches!(w.status, WorkspaceStatus::Trial | WorkspaceStatus::Active))
        .ok_or(ApiError::NotFound)
}

/// The workspace's own zone, for calendars that name none.
fn zone_of(w: &core_db::workspaces::Workspace) -> chrono_tz::Tz {
    w.default_timezone.parse().unwrap_or(chrono_tz::UTC)
}

async fn public_view(
    state: &AppState,
    tx: &mut Tx,
    ctx: &Ctx,
    talent_id: Uuid,
) -> ApiResult<PublicCalendarConnect> {
    let w = workspace_working(tx, ctx.workspace_id).await?;
    let talent = core_db::talent::get(tx, talent_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let connected = core_db::talent_calendars::list(tx, Some(talent_id)).await?;
    Ok(PublicCalendarConnect {
        workspace_name: w.name,
        branding: w.branding,
        talent_name: talent.name,
        google: state.config.mailboxes.google.is_some(),
        microsoft: state.config.mailboxes.microsoft.is_some(),
        connected: connected
            .iter()
            .map(|c| PublicConnected {
                provider: c.provider.clone(),
                person: c.person.clone(),
                status: c.status.clone(),
            })
            .collect(),
    })
}

pub async fn public_get(
    state: &AppState,
    token: &str,
    meta: core_domain::authz::RequestMeta,
) -> ApiResult<PublicCalendarConnect> {
    let (mut tx, ctx, _, talent_id) = open_link(state, token, meta).await?;
    let view = public_view(state, &mut tx, &ctx, talent_id).await?;
    tx.commit().await?;
    Ok(view)
}

fn clean_person(person: Option<&str>) -> ApiResult<Option<String>> {
    match person.map(str::trim) {
        Some(p) if !p.is_empty() => {
            if p.chars().count() > 100 {
                return Err(ApiError::invalid("person", "at most 100 characters"));
            }
            Ok(Some(p.to_owned()))
        }
        _ => Ok(None),
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ConnectState {
    workspace: WorkspaceId,
    link: Uuid,
    talent: Uuid,
    provider: String,
    person: Option<String>,
    nonce: String,
    exp: i64,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct StartCalendarConnect {
    /// `google` or `microsoft`.
    pub provider: String,
    /// Whose calendar this is, for a band or group: their name.
    pub person: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CalendarConnectStart {
    /// Send the talent here; the provider sends them back to
    /// `/connect-calendar` with `code` and `state`, which the page passes to
    /// the finish call.
    pub url: String,
}

fn redirect_uri(state: &AppState) -> String {
    state.config.app_url("/connect-calendar", &[])
}

fn scopes(provider: Provider) -> &'static [&'static str] {
    match provider {
        Provider::Google => calendars::GOOGLE_BUSY_SCOPES,
        Provider::Microsoft => calendars::MICROSOFT_BUSY_SCOPES,
        Provider::Xero | Provider::QuickBooks => &[],
    }
}

/// Where the talent grants OnHold sight of when they are busy.
pub async fn public_start(
    state: &AppState,
    token: &str,
    meta: core_domain::authz::RequestMeta,
    req: StartCalendarConnect,
) -> ApiResult<CalendarConnectStart> {
    let (tx, ctx, link, talent) = open_link(state, token, meta).await?;
    tx.commit().await?;
    let app = oauth_app(state, &req.provider)?;
    let flow = ConnectState {
        workspace: ctx.workspace_id,
        link,
        talent,
        provider: req.provider.clone(),
        person: clean_person(req.person.as_deref())?,
        nonce: BASE64URL_NOPAD.encode(&core_domain::crypto::random_bytes::<16>()),
        exp: Utc::now().timestamp() + CONNECT_TTL_SECS,
    };
    let json = serde_json::to_vec(&flow).map_err(ApiError::internal)?;
    let sealed = BASE64URL_NOPAD.encode(&state.config.keyring.encrypt(&json, STATE_AAD));
    Ok(CalendarConnectStart {
        // Only these scopes: never the mailbox access the same app may hold.
        url: app
            .authorize_exact(&sealed, &redirect_uri(state), scopes(app.provider))
            .to_string(),
    })
}

fn open_state(state: &AppState, sealed: &str) -> ApiResult<ConnectState> {
    let bad = || {
        ApiError::bad_request(
            "connect_state_invalid",
            "connecting took too long or was not started here; open your link and start again",
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
pub struct FinishCalendarConnect {
    /// From the provider's redirect.
    pub code: String,
    /// From the provider's redirect, as the start call made it.
    pub state: String,
}

/// What the talent sees once connected.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CalendarConnected {
    pub workspace_name: String,
    #[schema(value_type = Object)]
    pub branding: Value,
    pub talent_name: String,
    pub provider: String,
    /// The account connected, or the iCal link's host.
    pub account: String,
}

/// Finishes connecting with what the provider sent back, while the link
/// still works.
pub async fn public_finish(
    state: &AppState,
    meta: core_domain::authz::RequestMeta,
    req: FinishCalendarConnect,
) -> ApiResult<CalendarConnected> {
    let flow = open_state(state, &req.state)?;
    let app = oauth_app(state, &flow.provider)?;
    let name = app.provider.name();
    let cfg = &state.config.mailboxes;
    let tokens = app
        .exchange(&cfg.http, req.code.trim(), &redirect_uri(state))
        .await
        .map_err(|e| provider_error(name, e))?;
    let wanted = scopes(app.provider);
    if !tokens.scopes.is_empty()
        && !core_integrations::oauth::granted(
            &tokens.scopes,
            &wanted
                .iter()
                .copied()
                .filter(|s| !matches!(*s, "openid" | "email" | "offline_access" | "User.Read"))
                .collect::<Vec<_>>(),
        )
    {
        return Err(ApiError::bad_request(
            "scopes_missing",
            "OnHold needs to see when you are busy; connect again and allow it",
        ));
    }
    let allowed = |s: &String| {
        let s = s.to_ascii_lowercase();
        let short = s.strip_prefix("https://graph.microsoft.com/").unwrap_or(&s);
        ALLOWED_SCOPES.contains(&short)
    };
    if let Some(extra) = tokens.scopes.iter().find(|s| !allowed(s)) {
        // Not revoked: the grant is the account's, and may be a mailbox's.
        tracing::info!(scope = %extra, "a calendar grant carried more than busy times");
        return Err(ApiError::bad_request(
            "scopes_too_wide",
            format!(
                "this {name} account has given OnHold more than your busy times (its mailbox, perhaps), so it cannot be used here; paste your calendar's private iCal link instead"
            ),
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
            GoogleBusy::new(
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
        Provider::Xero | Provider::QuickBooks => {
            Err(ProviderError::Invalid("not a calendar provider".into()))
        }
    }
    .map_err(|e| provider_error(name, e))?
    .trim()
    .to_ascii_lowercase();
    let grant = Grant::OAuth {
        refresh_token,
        access_token: tokens.access_token.clone(),
        expires_at: tokens.expires_at,
    };
    let mut tx = core_db::begin(&state.db, Some(flow.workspace), None).await?;
    let mut ctx = Ctx::system(flow.workspace);
    ctx.meta = meta;
    save(
        state,
        &mut tx,
        &ctx,
        &flow,
        app.provider.as_str(),
        &account,
        &grant,
    )
    .await?;
    let view = public_view(state, &mut tx, &ctx, flow.talent).await?;
    tx.commit().await?;
    Ok(CalendarConnected {
        workspace_name: view.workspace_name,
        branding: view.branding,
        talent_name: view.talent_name,
        provider: app.provider.as_str().to_owned(),
        account,
    })
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct IcalConnect {
    /// The calendar's private iCal address (`https://` or `webcal://`).
    pub url: String,
    /// Whose calendar this is, for a band or group: their name.
    pub person: Option<String>,
}

/// Connects a calendar by its private iCal link, once it has been read.
pub async fn public_ical(
    state: &AppState,
    token: &str,
    meta: core_domain::authz::RequestMeta,
    req: IcalConnect,
) -> ApiResult<CalendarConnected> {
    let person = clean_person(req.person.as_deref())?;
    let url = calendars::ical_url(&req.url).ok_or_else(|| {
        ApiError::invalid(
            "url",
            "paste the calendar's iCal address, starting https:// or webcal://",
        )
    })?;
    let (mut tx, ctx, link, talent) = open_link(state, token, meta.clone()).await?;
    let tz = zone_of(&workspace_working(&mut tx, ctx.workspace_id).await?);
    tx.commit().await?;
    // Read it once now, so a wrong link is caught while they are here.
    let text = fetch_ical(state, url.as_str()).await.map_err(|e| match e {
        ProviderError::Unavailable(_) | ProviderError::RateLimited { .. } => {
            provider_error("The calendar", e)
        }
        other => ApiError::bad_request(
            "calendar_unreadable",
            format!("that link could not be read as a calendar: {other}"),
        ),
    })?;
    let (from, to) = window(Utc::now());
    ical_busy(text, from, to, tz).await.map_err(|e| match e {
        ProviderError::Unavailable(_) => provider_error("The calendar", e),
        other => ApiError::bad_request(
            "calendar_unreadable",
            format!("that calendar could not be read: {other}"),
        ),
    })?;
    let account = url.host_str().unwrap_or("calendar").to_ascii_lowercase();
    let flow = ConnectState {
        workspace: ctx.workspace_id,
        link,
        talent,
        provider: "ical".into(),
        person,
        nonce: String::new(),
        exp: 0,
    };
    let grant = Grant::Ical {
        url: url.to_string(),
    };
    let mut tx = core_db::begin(&state.db, Some(ctx.workspace_id), None).await?;
    save(state, &mut tx, &ctx, &flow, "ical", &account, &grant).await?;
    let view = public_view(state, &mut tx, &ctx, talent).await?;
    tx.commit().await?;
    Ok(CalendarConnected {
        workspace_name: view.workspace_name,
        branding: view.branding,
        talent_name: view.talent_name,
        provider: "ical".into(),
        account,
    })
}

/// Adds the calendar, or renews the same account's existing connection for
/// this talent. The link must still work.
async fn save(
    state: &AppState,
    tx: &mut Tx,
    ctx: &Ctx,
    flow: &ConnectState,
    provider: &str,
    account: &str,
    grant: &Grant,
) -> ApiResult<()> {
    workspace_working(tx, ctx.workspace_id).await?;
    if !core_db::talent_calendars::link_works(tx, flow.link).await? {
        return Err(ApiError::NotFound);
    }
    core_db::talent::get(tx, flow.talent)
        .await?
        .ok_or(ApiError::NotFound)?;
    // An iCal link is matched by the link itself, not its host.
    let url_hash = match grant {
        Grant::Ical { url } => Some(hash_token(url)),
        Grant::OAuth { .. } => None,
    };
    let existing = match &url_hash {
        Some(h) => core_db::talent_calendars::live_ical(tx, flow.talent, h).await?,
        None => core_db::talent_calendars::live_for(tx, flow.talent, provider, account).await?,
    };
    let now = Utc::now();
    let id = match existing {
        Some(c) => {
            core_db::talent_calendars::set_secret(tx, c.id, &seal(state, c.id, grant)?).await?;
            core_db::talent_calendars::revive(tx, c.id, flow.link, flow.person.as_deref()).await?;
            record(
                tx,
                ctx,
                Change {
                    topic: "talent_calendar.reconnected",
                    resource: ENTITY,
                    object_id: Some(c.id),
                    before: Some(audit_json(&c)),
                    after: None,
                    event_extra: None,
                },
            )
            .await?;
            c.id
        }
        None => {
            if core_db::talent_calendars::count_live(tx, flow.talent).await? >= MAX_PER_TALENT {
                return Err(ApiError::conflict(
                    "too_many_calendars",
                    format!(
                        "{MAX_PER_TALENT} calendars are already connected; ask the agency to remove one first"
                    ),
                ));
            }
            let id = core_domain::ids::new_id();
            core_db::talent_calendars::insert(
                tx,
                core_db::talent_calendars::NewCalendar {
                    id,
                    talent_id: flow.talent,
                    link_id: flow.link,
                    provider,
                    account,
                    person: flow.person.as_deref(),
                    secret: &seal(state, id, grant)?,
                    url_hash: url_hash.as_deref(),
                },
            )
            .await?;
            let saved = core_db::talent_calendars::get(tx, id)
                .await?
                .ok_or(ApiError::NotFound)?;
            record(
                tx,
                ctx,
                Change {
                    topic: "talent_calendar.connected",
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
    queue_read(tx, ctx.workspace_id, id, now).await?;
    Ok(())
}

// Reading ------------------------------------------------------------------------------------

async fn fetch_ical(state: &AppState, url: &str) -> Result<String, ProviderError> {
    let url = url::Url::parse(url).map_err(|e| ProviderError::Invalid(e.to_string()))?;
    let allow_private = state.config.webhooks_allow_private;
    let http = core_integrations::http::public_client(
        allow_private,
        concat!("OnHold/", env!("CARGO_PKG_VERSION"), " (calendar)"),
    )
    .map_err(ProviderError::Unavailable)?;
    calendars::fetch_ical(&http, &url, allow_private).await
}

/// What is read: from the start of yesterday to a year ahead, in whole
/// days, so the edges move once a day rather than on every read.
fn window(now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let day = |at: DateTime<Utc>| at.date_naive().and_time(chrono::NaiveTime::MIN).and_utc();
    (day(now - PAST), day(now + AHEAD))
}

/// Reads an iCal file's busy times off the async threads, a couple at a
/// time across the server, giving up after a while.
async fn ical_busy(
    text: String,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    tz: chrono_tz::Tz,
) -> Result<Vec<Busy>, ProviderError> {
    let limit = std::time::Duration::from_secs(ICAL_READ_SECS);
    let too_long = || ProviderError::Refused {
        status: 200,
        message: "the calendar took too long to read".into(),
    };
    let permit = tokio::time::timeout(limit, ICAL_READS.acquire())
        .await
        .map_err(|_| ProviderError::Unavailable("busy reading other calendars".into()))?
        .map_err(|e| ProviderError::Unavailable(e.to_string()))?;
    // The permit goes with the work, so a read given up on still counts
    // until it ends.
    let work = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        calendars::ical_busy(&text, from, to, tz)
    });
    match tokio::time::timeout(limit, work).await {
        Ok(Ok(got)) => got,
        Ok(Err(e)) => Err(ProviderError::Invalid(e.to_string())),
        Err(_) => Err(too_long()),
    }
}

/// A fresh access token for a calendar's grant, stored when refreshed.
async fn access_token(
    state: &AppState,
    ws: WorkspaceId,
    c: &Calendar,
    refresh_token: &str,
    access: String,
    expires_at: DateTime<Utc>,
    force: bool,
) -> Result<String, ProviderError> {
    if !force && expires_at - Duration::seconds(60) > Utc::now() {
        return Ok(access);
    }
    let app = oauth_app(state, &c.provider).map_err(|_| {
        ProviderError::Revoked("this server no longer connects to that provider".into())
    })?;
    let fresh = app
        .refresh(&state.config.mailboxes.http, refresh_token)
        .await?;
    store_refreshed(state, ws, c.id, refresh_token, &fresh)
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
    let grant = Grant::OAuth {
        refresh_token: fresh
            .refresh_token
            .clone()
            .unwrap_or_else(|| old_refresh.to_owned()),
        access_token: fresh.access_token.clone(),
        expires_at: fresh.expires_at,
    };
    let secret = seal(state, id, &grant)?;
    let ctx = Ctx::system(ws);
    let mut tx = begin(state, &ctx).await?;
    if core_db::talent_calendars::lock(&mut tx, id)
        .await?
        .is_some_and(|c| c.disconnected_at.is_none())
    {
        core_db::talent_calendars::set_secret(&mut tx, id, &secret).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Busy times from the provider, refreshing the token once if refused.
async fn read_busy(
    state: &AppState,
    ws: WorkspaceId,
    c: &Calendar,
    (from, to): (DateTime<Utc>, DateTime<Utc>),
    tz: chrono_tz::Tz,
) -> Result<Vec<Busy>, ProviderError> {
    let grant = open(state, c).map_err(|e| ProviderError::Invalid(e.to_string()))?;
    let cfg = &state.config.mailboxes;
    match grant {
        Grant::Ical { url } => {
            let text = fetch_ical(state, &url).await?;
            ical_busy(text, from, to, tz).await
        }
        Grant::OAuth {
            refresh_token,
            access_token: access,
            expires_at,
        } => {
            let mut forced = false;
            let mut token = access_token(
                state,
                ws,
                c,
                &refresh_token,
                access.clone(),
                expires_at,
                false,
            )
            .await?;
            loop {
                let got = match c.provider.as_str() {
                    "google" => {
                        GoogleBusy::new(cfg.http.clone(), cfg.google_api.clone(), token.clone())
                            .busy(from, to)
                            .await
                    }
                    "microsoft" => {
                        GraphBusy::new(cfg.http.clone(), cfg.graph_api.clone(), token.clone())
                            .busy(from, to, tz)
                            .await
                    }
                    other => Err(ProviderError::Invalid(format!("unknown provider {other}"))),
                };
                match got {
                    Err(ProviderError::Unauthorized) if !forced => {
                        forced = true;
                        token = access_token(
                            state,
                            ws,
                            c,
                            &refresh_token,
                            access.clone(),
                            expires_at,
                            true,
                        )
                        .await?;
                    }
                    other => return other,
                }
            }
        }
    }
}

/// What a read did, for the job's log.
#[derive(Debug, Default)]
pub struct ReadOutcome {
    pub busy: usize,
    pub added: usize,
    pub removed: usize,
    pub skipped: Option<&'static str>,
}

impl std::fmt::Display for ReadOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.skipped {
            Some(why) => write!(f, "skipped: {why}"),
            None => write!(
                f,
                "{} busy stretches, {} new, {} gone",
                self.busy, self.added, self.removed
            ),
        }
    }
}

/// Reads one calendar's busy times (the worker's `calendar.read`). Failures
/// are recorded on the calendar and retried with growing waits; the job
/// itself only fails when OnHold's own database does.
pub async fn read(state: &AppState, ws: WorkspaceId, id: Uuid) -> ApiResult<ReadOutcome> {
    let ctx = Ctx::system(ws);
    let mut tx = begin(state, &ctx).await?;
    if !core_db::talent_calendars::claim_poll(&mut tx, id, READ_LEASE_SECS).await? {
        return Ok(ReadOutcome {
            skipped: Some("disconnected, waiting to be connected again, or already being read"),
            ..ReadOutcome::default()
        });
    }
    let c = core_db::talent_calendars::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let tz = core_db::workspaces::get(&mut tx, ws)
        .await?
        .map_or(chrono_tz::UTC, |w| zone_of(&w));
    // A talent removed since: their calendars go with them.
    if core_db::talent::get(&mut tx, c.talent_id).await?.is_none() {
        disconnect_all(&mut tx, &ctx, Some(c.talent_id), "talent_removed").await?;
        tx.commit().await?;
        return Ok(ReadOutcome {
            skipped: Some("the talent was removed"),
            ..ReadOutcome::default()
        });
    }
    tx.commit().await?;

    let busy = match read_busy(state, ws, &c, window(Utc::now()), tz).await {
        Ok(b) => b,
        Err(e) => {
            failed(state, &ctx, &c, e).await?;
            return Ok(ReadOutcome::default());
        }
    };
    let mut outcome = ReadOutcome {
        busy: busy.len(),
        ..ReadOutcome::default()
    };
    let mut tx = begin(state, &ctx).await?;
    // Disconnected while reading: nothing is kept.
    let Some(locked) = core_db::talent_calendars::lock(&mut tx, id)
        .await?
        .filter(|l| l.disconnected_at.is_none())
    else {
        tx.commit().await?;
        return Ok(ReadOutcome {
            skipped: Some("disconnected while being read"),
            ..ReadOutcome::default()
        });
    };
    let have = core_db::talent_calendars::busy(&mut tx, id).await?;
    let (gone, new) = diff(&have, &busy);
    outcome.removed = gone.len();
    outcome.added = new.len();
    core_db::talent_calendars::remove_busy(&mut tx, &gone).await?;
    let rows: Vec<_> = new
        .iter()
        .map(|b| {
            (
                core_domain::ids::new_id(),
                b.starts_at,
                b.ends_at,
                b.tentative,
            )
        })
        .collect();
    core_db::talent_calendars::add_busy(&mut tx, id, locked.talent_id, &rows).await?;
    let next = Utc::now()
        + Duration::from_std(state.config.mailboxes.calendar_every)
            .unwrap_or(Duration::minutes(15));
    core_db::talent_calendars::synced(&mut tx, id, next).await?;
    if locked.status != "active" {
        record(
            &mut tx,
            &ctx,
            Change {
                topic: "talent_calendar.health_changed",
                resource: ENTITY,
                object_id: Some(id),
                before: Some(json!({ "status": locked.status })),
                after: Some(json!({ "status": "active" })),
                event_extra: None,
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(outcome)
}

/// Rows to remove and stretches to add, so a calendar that has not
/// changed writes nothing.
fn diff(have: &[core_db::talent_calendars::BusyRow], want: &[Busy]) -> (Vec<Uuid>, Vec<Busy>) {
    let key = |s: DateTime<Utc>, e: DateTime<Utc>, t: bool| (s, e, t);
    let wanted: std::collections::HashSet<_> = want
        .iter()
        .map(|b| key(b.starts_at, b.ends_at, b.tentative))
        .collect();
    let held: std::collections::HashSet<_> = have
        .iter()
        .map(|r| key(r.starts_at, r.ends_at, r.tentative))
        .collect();
    let gone = have
        .iter()
        .filter(|r| !wanted.contains(&key(r.starts_at, r.ends_at, r.tentative)))
        .map(|r| r.id)
        .collect();
    let new = want
        .iter()
        .filter(|b| !held.contains(&key(b.starts_at, b.ends_at, b.tentative)))
        .copied()
        .collect();
    (gone, new)
}

/// Records a failed read and when to try again: soon at first, then less
/// often (5 minutes doubling to 6 hours). A grant or link that no longer
/// works waits for the talent to connect again; its busy times are dropped
/// meanwhile, as nobody can say they are still true.
async fn failed(state: &AppState, ctx: &Ctx, c: &Calendar, err: ProviderError) -> ApiResult<()> {
    let wait = match &err {
        ProviderError::RateLimited {
            retry_after: Some(s),
        } => Duration::seconds((*s).clamp(30, 6 * 3600) as i64),
        _ => {
            let doubling = 2i64.saturating_pow(c.failures.clamp(0, 16) as u32);
            Duration::minutes(5 * doubling).min(Duration::hours(6))
        }
    };
    let (status, next) = match &err {
        ProviderError::Revoked(_) => ("reconnect", None),
        e if e.is_temporary() && c.failures + 1 < QUIET_FAILURES => (
            if c.status == "failing" {
                "failing"
            } else {
                "active"
            },
            Some(Utc::now() + wait),
        ),
        _ => ("failing", Some(Utc::now() + wait)),
    };
    let message = err.to_string();
    tracing::warn!(calendar_id = %c.id, provider = %c.provider, error = %message, "reading a talent's calendar failed");
    let mut tx = begin(state, ctx).await?;
    core_db::talent_calendars::failed(&mut tx, c.id, status, &message, next).await?;
    if status == "reconnect" {
        core_db::talent_calendars::forget_busy(&mut tx, c.id).await?;
    }
    if status != c.status {
        record(
            &mut tx,
            ctx,
            Change {
                topic: "talent_calendar.health_changed",
                resource: ENTITY,
                object_id: Some(c.id),
                before: Some(json!({ "status": c.status })),
                after: Some(json!({ "status": status })),
                event_extra: None,
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .map(|d| d.to_utc())
            .expect("a test time")
    }

    #[test]
    fn an_unchanged_calendar_writes_nothing() {
        let b = |s: &str, e: &str| Busy {
            starts_at: at(s),
            ends_at: at(e),
            tentative: false,
        };
        let row = |id: u128, s: &str, e: &str| core_db::talent_calendars::BusyRow {
            id: Uuid::from_u128(id),
            starts_at: at(s),
            ends_at: at(e),
            tentative: false,
        };
        let have = vec![
            row(1, "2027-01-01T10:00:00Z", "2027-01-01T11:00:00Z"),
            row(2, "2027-01-02T10:00:00Z", "2027-01-02T11:00:00Z"),
        ];
        let same = vec![
            b("2027-01-01T10:00:00Z", "2027-01-01T11:00:00Z"),
            b("2027-01-02T10:00:00Z", "2027-01-02T11:00:00Z"),
        ];
        assert_eq!(diff(&have, &same), (vec![], vec![]));
        let moved = vec![
            b("2027-01-01T10:00:00Z", "2027-01-01T11:00:00Z"),
            b("2027-01-02T12:00:00Z", "2027-01-02T13:00:00Z"),
        ];
        assert_eq!(
            diff(&have, &moved),
            (
                vec![Uuid::from_u128(2)],
                vec![b("2027-01-02T12:00:00Z", "2027-01-02T13:00:00Z")]
            )
        );
    }
}
