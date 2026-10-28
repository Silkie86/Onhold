//! Connected mailboxes: a member connects their Gmail or Outlook (OAuth)
//! or any IMAP/SMTP mailbox, and the worker reads it into the inbox every
//! few minutes. The grant is encrypted and bound to the connection's id;
//! access tokens are refreshed as they run out, and a grant the provider
//! no longer honours turns the connection red ("reconnect") instead of
//! failing quietly.

use chrono::{DateTime, Duration, Utc};
use core_db::integrations::Integration;
use core_domain::{Ctx, Permission, UserId, WorkspaceId};
use core_integrations::gmail::Gmail;
use core_integrations::graph::Graph;
use core_integrations::imap::{self, Imap};
use core_integrations::mailbox::{FETCH_BATCH, MailProvider};
use core_integrations::oauth::{self, OAuthApp, Provider, Tokens};
use core_integrations::provider::ProviderError;
use data_encoding::BASE64URL_NOPAD;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{Change, begin, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

pub const ENTITY: &str = "integration";
/// Reads one mailbox (`integration_id`, `at`: the time it was due).
pub const JOB_POLL: &str = "mail.poll";

const STATE_AAD: &[u8] = b"mailbox_connect_state";
/// How long the member has to finish on the provider's side.
const CONNECT_TTL_SECS: i64 = 30 * 60;
/// How long one read may hold a mailbox before another may start.
const POLL_LEASE_SECS: f64 = 10.0 * 60.0;
/// Batches one read takes before letting other mailboxes have a turn.
const BATCHES_PER_POLL: usize = 4;
/// No further batch is started after this long, keeping a read well inside
/// the worker's two-minute limit for a job.
const POLL_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);
/// Failures in a row before a mailbox shows as failing rather than being
/// retried quietly: a provider's brief outage should not alarm anyone.
const QUIET_FAILURES: i32 = 3;

/// What a connection acts with: an OAuth grant or a password.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Grant {
    OAuth {
        refresh_token: String,
        access_token: String,
        expires_at: DateTime<Utc>,
    },
    Password {
        password: String,
    },
}

fn grant_aad(id: Uuid) -> Vec<u8> {
    [b"integration:".as_slice(), id.as_bytes()].concat()
}

fn seal(state: &AppState, id: Uuid, grant: &Grant) -> ApiResult<Vec<u8>> {
    let json = serde_json::to_vec(grant).map_err(ApiError::internal)?;
    Ok(state.config.keyring.encrypt(&json, &grant_aad(id)))
}

fn open(state: &AppState, i: &Integration) -> ApiResult<Grant> {
    let plain = state
        .config
        .keyring
        .decrypt(&i.secret, &grant_aad(i.id))
        .map_err(ApiError::internal)?;
    serde_json::from_slice(&plain).map_err(ApiError::internal)
}

/// A connection as the settings page shows it. Never carries the grant.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct IntegrationView {
    pub id: Uuid,
    /// `google`, `microsoft` or `imap`.
    pub provider: String,
    pub purposes: Vec<String>,
    pub owner_id: Uuid,
    /// The address mail is read from and sent as.
    pub mailbox: String,
    /// The account that connected it, when it differs (a shared mailbox).
    pub account: String,
    pub display_name: Option<String>,
    /// Everyone who can read the inbox sees its mail.
    pub shared: bool,
    /// `active`, `failing` (being retried) or `reconnect` (connect it again).
    pub status: String,
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub connected_at: DateTime<Utc>,
    /// IMAP and SMTP servers, for an IMAP mailbox.
    #[schema(value_type = Option<Object>)]
    pub servers: Option<Value>,
    pub version: i64,
}

impl From<&Integration> for IntegrationView {
    fn from(i: &Integration) -> Self {
        Self {
            id: i.id,
            provider: i.provider.clone(),
            purposes: i.purposes.clone(),
            owner_id: i.owner_id.0,
            mailbox: i.mailbox().to_owned(),
            account: i.account.clone(),
            display_name: i.display_name.clone(),
            shared: i.shared,
            status: i.status.clone(),
            last_error: i.last_error.clone(),
            last_error_at: i.last_error_at,
            last_synced_at: i.last_synced_at,
            connected_at: i.connected_at,
            servers: (i.provider == "imap").then(|| i.settings.clone()),
            version: i.version,
        }
    }
}

fn audit_json(i: &Integration) -> Value {
    json!({
        "provider": i.provider,
        "mailbox": i.mailbox(),
        "account": i.account,
        "owner_id": i.owner_id,
        "shared": i.shared,
        "display_name": i.display_name,
        "status": i.status,
    })
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MailSetup {
    /// Gmail can be connected on this server.
    pub google: bool,
    /// Outlook and Microsoft 365 can be connected on this server.
    pub microsoft: bool,
    /// The caller's own mailboxes and the shared ones; every mailbox for
    /// those with `integration:manage`.
    pub mailboxes: Vec<IntegrationView>,
}

/// Who sees a mailbox's mail: `None` sees every one.
pub(crate) fn viewer(ctx: &Ctx) -> Option<UserId> {
    match ctx.actor {
        core_domain::Actor::User { user_id, .. } => Some(user_id),
        core_domain::Actor::ApiKey { created_by, .. } => {
            Some(created_by.unwrap_or(UserId(Uuid::nil())))
        }
        core_domain::Actor::Rule { .. } | core_domain::Actor::System => None,
    }
}

pub async fn setup(state: &AppState, ctx: &Ctx) -> ApiResult<MailSetup> {
    ctx.authorize(Permission::InboxRead)?;
    let manage = ctx.permissions.contains(&Permission::IntegrationManage);
    let mut tx = begin(state, ctx).await?;
    let rows =
        core_db::integrations::list(&mut tx, if manage { None } else { viewer(ctx) }).await?;
    tx.commit().await?;
    Ok(MailSetup {
        google: state.config.mailboxes.google.is_some(),
        microsoft: state.config.mailboxes.microsoft.is_some(),
        mailboxes: rows.iter().map(IntegrationView::from).collect(),
    })
}

// Connecting through OAuth ------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct ConnectState {
    workspace: WorkspaceId,
    user: UserId,
    provider: String,
    shared_mailbox: Option<String>,
    nonce: String,
    exp: i64,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct StartConnect {
    /// `google` or `microsoft`.
    pub provider: String,
    /// A Microsoft 365 shared mailbox to connect through your own account.
    pub shared_mailbox: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConnectStart {
    /// Send the member here; the provider sends them back to Settings →
    /// Email with `code` and `state`, which the page passes to the finish call.
    pub url: String,
}

fn app<'a>(state: &'a AppState, provider: &str) -> ApiResult<&'a OAuthApp> {
    let app = match provider {
        "google" => state.config.mailboxes.google.as_ref(),
        "microsoft" => state.config.mailboxes.microsoft.as_ref(),
        _ => {
            return Err(ApiError::invalid(
                "provider",
                "use google or microsoft (or connect by IMAP)",
            ));
        }
    };
    app.ok_or_else(|| {
        ApiError::conflict(
            "provider_not_available",
            format!("{provider} accounts cannot be connected on this server"),
        )
    })
}

fn scopes(provider: Provider, shared: bool) -> Vec<&'static str> {
    match provider {
        Provider::Google => oauth::GOOGLE_MAIL_SCOPES.to_vec(),
        Provider::Microsoft => {
            let mut s = oauth::MICROSOFT_MAIL_SCOPES.to_vec();
            if shared {
                s.extend_from_slice(oauth::MICROSOFT_SHARED_MAIL_SCOPES);
            }
            s
        }
    }
}

fn redirect_uri(state: &AppState) -> String {
    state.config.app_url("/settings/email", &[])
}

fn clean_address(field: &str, value: &str) -> ApiResult<String> {
    let v = value.trim().to_ascii_lowercase();
    if !core_engines::fields::is_email(&v) || v.len() > 320 {
        return Err(ApiError::invalid(field, "must be an email address"));
    }
    Ok(v)
}

/// Where the member grants OnHold access to their mailbox.
pub async fn start_connect(
    state: &AppState,
    ctx: &Ctx,
    req: StartConnect,
) -> ApiResult<ConnectStart> {
    ctx.authorize(Permission::InboxWrite)?;
    let user = ctx.user_id().ok_or_else(|| {
        ApiError::forbidden(
            "member_required",
            "a mailbox is connected by its owner, signed in",
        )
    })?;
    let app = app(state, &req.provider)?;
    let shared_mailbox = match req.shared_mailbox.as_deref().map(str::trim) {
        Some(m) if !m.is_empty() => {
            if app.provider != Provider::Microsoft {
                return Err(ApiError::invalid(
                    "shared_mailbox",
                    "shared mailboxes are connected through Microsoft 365",
                ));
            }
            Some(clean_address("shared_mailbox", m)?)
        }
        _ => None,
    };
    let flow = ConnectState {
        workspace: ctx.workspace_id,
        user,
        provider: req.provider.clone(),
        shared_mailbox: shared_mailbox.clone(),
        nonce: BASE64URL_NOPAD.encode(&core_domain::crypto::random_bytes::<16>()),
        exp: Utc::now().timestamp() + CONNECT_TTL_SECS,
    };
    let json = serde_json::to_vec(&flow).map_err(ApiError::internal)?;
    let sealed = BASE64URL_NOPAD.encode(&state.config.keyring.encrypt(&json, STATE_AAD));
    let wanted = scopes(app.provider, shared_mailbox.is_some());
    Ok(ConnectStart {
        url: app
            .authorize(&sealed, &redirect_uri(state), &wanted, None)
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
pub struct FinishConnect {
    /// From the provider's redirect.
    pub code: String,
    /// From the provider's redirect, as the start call made it.
    pub state: String,
}

pub(crate) fn provider_error(provider: &str, err: ProviderError) -> ApiError {
    match err {
        ProviderError::Unavailable(m) => ApiError::unavailable(
            "provider_unavailable",
            format!("{provider} could not be reached; try again shortly ({m})"),
        ),
        ProviderError::RateLimited { .. } => ApiError::unavailable(
            "provider_unavailable",
            format!("{provider} asked us to slow down; try again shortly"),
        ),
        ProviderError::Revoked(m) => ApiError::bad_request("provider_refused", m),
        ProviderError::Refused { message, .. } => {
            ApiError::bad_request("provider_refused", format!("{provider} said: {message}"))
        }
        ProviderError::Unauthorized | ProviderError::CursorExpired => ApiError::bad_request(
            "provider_refused",
            format!("{provider} did not accept the sign-in; start again"),
        ),
        ProviderError::Invalid(m) => ApiError::internal(format!("{provider}: {m}")),
    }
}

/// Finishes connecting with what the provider sent back. Only the member
/// who started, in the same workspace, can finish.
pub async fn finish_connect(
    state: &AppState,
    ctx: &Ctx,
    req: FinishConnect,
) -> ApiResult<IntegrationView> {
    ctx.authorize(Permission::InboxWrite)?;
    let flow = open_state(state, &req.state)?;
    if flow.workspace != ctx.workspace_id || Some(flow.user) != ctx.user_id() {
        return Err(ApiError::bad_request(
            "connect_state_invalid",
            "this connection was started by someone else or in another workspace",
        ));
    }
    let app = app(state, &flow.provider)?;
    let name = match app.provider {
        Provider::Google => "Google",
        Provider::Microsoft => "Microsoft",
    };
    let http = &state.config.mailboxes.http;
    let tokens = app
        .exchange(http, req.code.trim(), &redirect_uri(state))
        .await
        .map_err(|e| provider_error(name, e))?;
    let wanted = scopes(app.provider, flow.shared_mailbox.is_some());
    if !tokens.scopes.is_empty() && !oauth::granted(&tokens.scopes, &wanted) {
        return Err(ApiError::bad_request(
            "scopes_missing",
            "OnHold needs permission to read and send your mail; connect again and allow both",
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
    // Whose mailbox this is, according to the provider.
    let account = match app.provider {
        Provider::Google => {
            Gmail::new(
                http.clone(),
                state.config.mailboxes.gmail_api.clone(),
                tokens.access_token.clone(),
            )
            .profile()
            .await
            .map_err(|e| provider_error(name, e))?
            .email_address
        }
        Provider::Microsoft => {
            let me = Graph::new(
                http.clone(),
                state.config.mailboxes.graph_api.clone(),
                tokens.access_token.clone(),
                None,
            )
            .address()
            .await
            .map_err(|e| provider_error(name, e))?;
            if let Some(shared) = &flow.shared_mailbox {
                // The shared mailbox must be one this account can read.
                Graph::new(
                    http.clone(),
                    state.config.mailboxes.graph_api.clone(),
                    tokens.access_token.clone(),
                    Some(shared),
                )
                .check_access()
                .await
                .map_err(|e| provider_error(name, e))?;
            }
            me
        }
    };
    let account = account.trim().to_ascii_lowercase();
    let grant = Grant::OAuth {
        refresh_token,
        access_token: tokens.access_token.clone(),
        expires_at: tokens.expires_at,
    };
    save_connection(
        state,
        ctx,
        flow.user,
        app.provider.as_str(),
        &account,
        flow.shared_mailbox.as_deref(),
        None,
        &json!({}),
        &grant,
        &tokens.scopes,
    )
    .await
}

/// Adds the connection, or renews its owner's existing one to the same mailbox.
#[allow(clippy::too_many_arguments)]
async fn save_connection(
    state: &AppState,
    ctx: &Ctx,
    owner: UserId,
    provider: &str,
    account: &str,
    shared_mailbox: Option<&str>,
    display_name: Option<&str>,
    settings: &Value,
    grant: &Grant,
    scopes: &[String],
) -> ApiResult<IntegrationView> {
    let mailbox = shared_mailbox.unwrap_or(account);
    let now = Utc::now();
    let mut tx = begin(state, ctx).await?;
    let id = match core_db::integrations::live_for(&mut tx, provider, mailbox).await? {
        Some(existing) if existing.owner_id != owner => {
            return Err(ApiError::conflict(
                "mailbox_taken",
                "another member has already connected this mailbox",
            ));
        }
        Some(existing) => {
            let secret = seal(state, existing.id, grant)?;
            core_db::integrations::set_secret(&mut tx, existing.id, &secret, scopes).await?;
            core_db::integrations::revive(&mut tx, existing.id, now).await?;
            if provider == "imap" {
                core_db::integrations::set_settings(&mut tx, existing.id, settings).await?;
            }
            record(
                &mut tx,
                ctx,
                Change {
                    topic: "integration.reconnected",
                    resource: ENTITY,
                    object_id: Some(existing.id),
                    before: Some(audit_json(&existing)),
                    after: None,
                    event_extra: None,
                },
            )
            .await?;
            existing.id
        }
        None => {
            match core_db::integrations::last_disconnected_for(&mut tx, provider, mailbox, owner)
                .await?
            {
                // Connected again by the member who disconnected it: the same
                // connection carries on, so what it read is recognised and its
                // conversations continue.
                Some(old) => {
                    let secret = seal(state, old.id, grant)?;
                    core_db::integrations::reconnect(&mut tx, old.id).await?;
                    core_db::integrations::set_secret(&mut tx, old.id, &secret, scopes).await?;
                    core_db::integrations::revive(&mut tx, old.id, now).await?;
                    if provider == "imap" {
                        core_db::integrations::set_settings(&mut tx, old.id, settings).await?;
                    }
                    record(
                        &mut tx,
                        ctx,
                        Change {
                            topic: "integration.reconnected",
                            resource: ENTITY,
                            object_id: Some(old.id),
                            before: Some(audit_json(&old)),
                            after: None,
                            event_extra: None,
                        },
                    )
                    .await?;
                    old.id
                }
                None => {
                    let id = core_domain::ids::new_id();
                    let secret = seal(state, id, grant)?;
                    core_db::integrations::insert(
                        &mut tx,
                        core_db::integrations::NewIntegration {
                            id,
                            provider,
                            purposes: &["mail".to_owned()],
                            owner_id: owner,
                            account,
                            shared_mailbox,
                            shared: shared_mailbox.is_some(),
                            display_name,
                            settings,
                            secret: &secret,
                            scopes,
                            mail_since: now
                                - Duration::days(state.config.mailboxes.first_read_days),
                            next_poll_at: Some(now),
                        },
                    )
                    .await?;
                    let saved = core_db::integrations::get(&mut tx, id)
                        .await?
                        .ok_or(ApiError::NotFound)?;
                    record(
                        &mut tx,
                        ctx,
                        Change {
                            topic: "integration.connected",
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
            }
        }
    };
    queue_poll(&mut tx, ctx.workspace_id, id, now).await?;
    let saved = core_db::integrations::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(IntegrationView::from(&saved))
}

async fn queue_poll(
    tx: &mut core_db::Tx,
    ws: WorkspaceId,
    id: Uuid,
    at: DateTime<Utc>,
) -> ApiResult<()> {
    let payload = json!({ "integration_id": id, "at": at });
    let key = poll_key(id, at);
    core_db::jobs::enqueue(
        tx,
        core_db::jobs::NewJob::new(Some(ws), JOB_POLL, &payload).dedupe(&key),
    )
    .await?;
    Ok(())
}

/// One read per mailbox and due time.
pub fn poll_key(id: Uuid, at: DateTime<Utc>) -> String {
    format!("{id}@{}", at.to_rfc3339())
}

// Connecting by IMAP and SMTP ---------------------------------------------------------

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ImapConnect {
    /// The address mail is sent as.
    pub address: String,
    pub display_name: Option<String>,
    pub imap: MailServer,
    pub smtp: MailServer,
    /// Default: the address.
    pub username: Option<String>,
    /// Often an app password made in the mail provider's settings.
    pub password: String,
}

/// An IMAP or SMTP server.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MailServer {
    pub host: String,
    /// IMAP: 993 or 143. SMTP: 465, 587, 25 or 2525.
    pub port: u16,
    pub security: MailSecurity,
}

/// How the connection is protected.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MailSecurity {
    /// TLS from the first byte (IMAP 993, SMTP 465).
    Tls,
    /// Upgraded with STARTTLS before signing in (IMAP 143, SMTP 587).
    Starttls,
    /// None: development servers only.
    None,
}

impl From<MailServer> for imap::Server {
    fn from(s: MailServer) -> Self {
        Self {
            host: s.host.trim().to_ascii_lowercase(),
            port: s.port,
            security: match s.security {
                MailSecurity::Tls => imap::Security::Tls,
                MailSecurity::Starttls => imap::Security::Starttls,
                MailSecurity::None => imap::Security::None,
            },
        }
    }
}

/// Connects a mailbox by IMAP and SMTP. The servers are tried before it is saved.
pub async fn connect_imap(
    state: &AppState,
    ctx: &Ctx,
    req: ImapConnect,
) -> ApiResult<IntegrationView> {
    ctx.authorize(Permission::InboxWrite)?;
    let user = ctx.user_id().ok_or_else(|| {
        ApiError::forbidden(
            "member_required",
            "a mailbox is connected by its owner, signed in",
        )
    })?;
    let address = clean_address("address", &req.address)?;
    let display_name = super::optional_text("display_name", req.display_name, 200)?;
    if req.password.is_empty() || req.password.len() > 1000 {
        return Err(ApiError::invalid("password", "is required"));
    }
    let settings = imap::Settings {
        imap: req.imap.into(),
        smtp: req.smtp.into(),
        username: req
            .username
            .map(|u| u.trim().to_owned())
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| address.clone()),
    };
    let allow_private = state.config.webhooks_allow_private;
    Imap::check_settings(&settings, allow_private).map_err(|m| ApiError::invalid("servers", m))?;
    Imap::new(settings.clone(), req.password.clone(), allow_private)
        .verify()
        .await
        .map_err(|e| provider_error("The mail server", e))?;
    let settings_json = serde_json::to_value(&settings).map_err(ApiError::internal)?;
    save_connection(
        state,
        ctx,
        user,
        "imap",
        &address,
        None,
        display_name.as_deref(),
        &settings_json,
        &Grant::Password {
            password: req.password,
        },
        &[],
    )
    .await
}

// Managing a connection ---------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct IntegrationPatch {
    /// Let everyone who can read the inbox see this mailbox's mail.
    pub shared: Option<bool>,
    /// The name replies are sent under; `null` uses the account's own.
    #[serde(default, deserialize_with = "crate::routes::nullable")]
    #[schema(value_type = Option<String>)]
    pub display_name: Option<Option<String>>,
}

async fn load_own(
    tx: &mut core_db::Tx,
    ctx: &Ctx,
    id: Uuid,
    manage_ok: bool,
) -> ApiResult<Integration> {
    let i = core_db::integrations::lock(tx, id)
        .await?
        .filter(|i| i.disconnected_at.is_none())
        .ok_or(ApiError::NotFound)?;
    let own = viewer(ctx).is_none_or(|v| v == i.owner_id);
    let manager = manage_ok && ctx.permissions.contains(&Permission::IntegrationManage);
    if own || manager {
        return Ok(i);
    }
    // Others may know a shared mailbox exists, but not change it.
    if i.shared {
        return Err(ApiError::forbidden(
            "owner_required",
            "only the member who connected this mailbox can change it",
        ));
    }
    Err(ApiError::NotFound)
}

/// Changes a connection's sharing or name. Only its owner can.
pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    patch: IntegrationPatch,
    if_match: Option<i64>,
) -> ApiResult<IntegrationView> {
    ctx.authorize(Permission::InboxWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = load_own(&mut tx, ctx, id, false).await?;
    super::check_version(before.version, if_match)?;
    let shared = patch.shared.unwrap_or(before.shared);
    let display_name = match patch.display_name {
        Some(v) => super::optional_text("display_name", v, 200)?,
        None => before.display_name.clone(),
    };
    core_db::integrations::set_preferences(&mut tx, id, shared, display_name.as_deref()).await?;
    let after = core_db::integrations::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "integration.updated",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(audit_json(&before)),
            after: Some(audit_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(IntegrationView::from(&after))
}

/// Disconnects a mailbox: its owner, or someone who manages integrations.
/// Its threads stay. Google is asked to forget the grant too.
pub async fn disconnect(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::InboxWrite)?;
    let mut tx = begin(state, ctx).await?;
    let before = load_own(&mut tx, ctx, id, true).await?;
    let grant = open(state, &before).ok();
    core_db::integrations::disconnect(&mut tx, id).await?;
    // Revoking a Google grant ends every grant OnHold holds for that
    // account, so it is left alone while another connection (here or in
    // another workspace) still uses it.
    let in_use =
        core_db::integrations::account_in_use(&mut tx, &before.provider, &before.account, id)
            .await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "integration.disconnected",
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
        (grant, app(state, &before.provider), in_use)
        && let Err(e) = app
            .revoke(&state.config.mailboxes.http, &refresh_token)
            .await
    {
        tracing::warn!(integration_id = %id, error = %e, "revoking a mailbox grant failed");
    }
    Ok(())
}

/// Disconnects every mailbox a member connected, when they leave.
pub(crate) async fn disconnect_owned(
    tx: &mut core_db::Tx,
    ctx: &Ctx,
    owner: UserId,
) -> ApiResult<()> {
    for id in core_db::integrations::owned_by(tx, owner).await? {
        let before = core_db::integrations::get(tx, id).await?;
        core_db::integrations::disconnect(tx, id).await?;
        record(
            tx,
            ctx,
            Change {
                topic: "integration.disconnected",
                resource: ENTITY,
                object_id: Some(id),
                before: before.as_ref().map(audit_json),
                after: None,
                event_extra: Some(json!({ "reason": "member_removed" })),
            },
        )
        .await?;
    }
    Ok(())
}

/// Disconnects every mailbox of a workspace that is closing.
pub(crate) async fn disconnect_all(tx: &mut core_db::Tx, ctx: &Ctx) -> ApiResult<()> {
    for id in core_db::integrations::disconnect_all(tx).await? {
        record(
            tx,
            ctx,
            Change {
                topic: "integration.disconnected",
                resource: ENTITY,
                object_id: Some(id),
                before: None,
                after: None,
                event_extra: Some(json!({ "reason": "workspace_closed" })),
            },
        )
        .await?;
    }
    Ok(())
}

/// Reads a mailbox now rather than at its next turn.
pub async fn sync_now(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<IntegrationView> {
    ctx.authorize(Permission::InboxWrite)?;
    let mut tx = begin(state, ctx).await?;
    let i = load_own(&mut tx, ctx, id, true).await?;
    if i.status == "reconnect" {
        return Err(ApiError::conflict(
            "reconnect_required",
            "this mailbox needs to be connected again first",
        ));
    }
    if let Some(at) = core_db::integrations::poll_now(&mut tx, id).await? {
        queue_poll(&mut tx, ctx.workspace_id, id, at).await?;
    }
    let after = core_db::integrations::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(IntegrationView::from(&after))
}

// Reading and sending ------------------------------------------------------------------

/// A provider ready to use, with a fresh access token.
pub(crate) async fn provider_for(
    state: &AppState,
    ws: WorkspaceId,
    i: &Integration,
    force_refresh: bool,
) -> Result<Box<dyn MailProvider>, ProviderError> {
    let grant = open(state, i).map_err(|e| ProviderError::Invalid(e.to_string()))?;
    let cfg = &state.config.mailboxes;
    match grant {
        Grant::Password { password } => {
            let settings: imap::Settings = serde_json::from_value(i.settings.clone())
                .map_err(|e| ProviderError::Invalid(e.to_string()))?;
            Ok(Box::new(Imap::new(
                settings,
                password,
                state.config.webhooks_allow_private,
            )))
        }
        Grant::OAuth {
            refresh_token,
            access_token,
            expires_at,
        } => {
            let mut token = access_token;
            if force_refresh || expires_at - Duration::seconds(60) <= Utc::now() {
                let app = app(state, &i.provider).map_err(|_| {
                    ProviderError::Revoked("this server no longer connects to that provider".into())
                })?;
                let fresh = app.refresh(&cfg.http, &refresh_token).await?;
                store_refreshed(state, ws, i.id, &refresh_token, &fresh)
                    .await
                    .map_err(|e| ProviderError::Invalid(e.to_string()))?;
                token = fresh.access_token;
            }
            match i.provider.as_str() {
                "google" => Ok(Box::new(Gmail::new(
                    cfg.http.clone(),
                    cfg.gmail_api.clone(),
                    token,
                ))),
                "microsoft" => Ok(Box::new(Graph::new(
                    cfg.http.clone(),
                    cfg.graph_api.clone(),
                    token,
                    i.shared_mailbox.as_deref(),
                ))),
                other => Err(ProviderError::Invalid(format!("unknown provider {other}"))),
            }
        }
    }
}

/// Keeps a refreshed token. A provider that sends a new refresh token
/// (Microsoft does each time) replaces the old one.
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
    let Some(i) = core_db::integrations::lock(&mut tx, id).await? else {
        return Ok(());
    };
    if i.disconnected_at.is_some() {
        return Ok(());
    }
    let scopes = if fresh.scopes.is_empty() {
        i.scopes.clone()
    } else {
        fresh.scopes.clone()
    };
    core_db::integrations::set_secret(&mut tx, id, &secret, &scopes).await?;
    // The audit log records that a token was refreshed, never the token.
    core_db::audit::record(
        &mut tx,
        &ctx,
        core_db::audit::Entry {
            action: "integration.token_refreshed",
            resource: ENTITY,
            object_id: Some(id),
            before: None,
            after: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// What a read did, for the job's log.
#[derive(Debug, Default)]
pub struct PollOutcome {
    pub read: usize,
    pub added: usize,
    pub skipped: Option<&'static str>,
}

impl std::fmt::Display for PollOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.skipped {
            Some(why) => write!(f, "skipped: {why}"),
            None => write!(f, "read {} messages, {} new", self.read, self.added),
        }
    }
}

/// Reads new mail from one mailbox into the inbox (the worker's `mail.poll`).
/// Failures are recorded on the connection and retried with growing waits;
/// the job itself only fails when OnHold's own database does. A read that
/// stops part way (an error, the worker stopping) leaves its lease to run
/// out, and the mailbox is due again when it does.
pub async fn poll(state: &AppState, ws: WorkspaceId, id: Uuid) -> ApiResult<PollOutcome> {
    let ctx = Ctx::system(ws);
    let mut tx = begin(state, &ctx).await?;
    if !core_db::integrations::claim_poll(&mut tx, id, POLL_LEASE_SECS).await? {
        return Ok(PollOutcome {
            skipped: Some("disconnected, waiting to be connected again, or already being read"),
            ..PollOutcome::default()
        });
    }
    let mut i = core_db::integrations::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;

    let started = std::time::Instant::now();
    let mut outcome = PollOutcome::default();
    let mut refreshed = false;
    let mut restarted = false;
    let mut provider = match provider_for(state, ws, &i, false).await {
        Ok(p) => p,
        Err(e) => return failed(state, &ctx, &i, e).await.map(|()| outcome),
    };
    let mut batches = 0;
    loop {
        let fetched = provider
            .fetch(i.mail_cursor.as_deref(), i.mail_since, FETCH_BATCH)
            .await;
        let batch = match fetched {
            Ok(b) => b,
            Err(ProviderError::Unauthorized) if !refreshed => {
                refreshed = true;
                provider = match provider_for(state, ws, &i, true).await {
                    Ok(p) => p,
                    Err(e) => return failed(state, &ctx, &i, e).await.map(|()| outcome),
                };
                continue;
            }
            Err(ProviderError::CursorExpired) if !restarted => {
                // Read again from a little before the last good read; what
                // is already in the inbox is recognised and skipped.
                restarted = true;
                let since = i.last_synced_at.unwrap_or(i.mail_since) - Duration::days(1);
                i.mail_cursor = None;
                i.mail_since = since.max(i.mail_since);
                let mut tx = begin(state, &ctx).await?;
                core_db::integrations::restart(&mut tx, id, i.mail_since).await?;
                tx.commit().await?;
                continue;
            }
            Err(e) => return failed(state, &ctx, &i, e).await.map(|()| outcome),
        };
        for m in &batch.messages {
            if let Err(e) = read_one(state, ws, &i, provider.as_ref(), m, &mut outcome).await {
                return failed(state, &ctx, &i, e).await.map(|()| outcome);
            }
        }
        batches += 1;
        // Stop for now when the time is up, well inside the job's limit;
        // the rest is read straight after.
        let keep_going =
            batch.more && batches < BATCHES_PER_POLL && started.elapsed() < POLL_BUDGET;
        let next = if batch.more {
            Utc::now()
        } else {
            Utc::now()
                + Duration::from_std(state.config.mailboxes.poll_every)
                    .unwrap_or(Duration::minutes(2))
        };
        let mut tx = begin(state, &ctx).await?;
        core_db::integrations::synced(&mut tx, id, Some(&batch.cursor), Some(next)).await?;
        if !keep_going {
            // The next read is queued by the scheduler's sweep, or straight
            // away when there is more to read.
            if batch.more {
                queue_poll(&mut tx, ws, id, next).await?;
            }
            tx.commit().await?;
            break;
        }
        // Keep the lease while reading on, and stop if the mailbox was
        // disconnected meanwhile.
        if !core_db::integrations::renew_poll(&mut tx, id, POLL_LEASE_SECS).await? {
            tx.commit().await?;
            break;
        }
        tx.commit().await?;
        i.mail_cursor = Some(batch.cursor);
    }
    Ok(outcome)
}

/// Reads one listed message into the inbox: its body first, unless the
/// provider sent it or the inbox already holds the message. A message the
/// inbox cannot take (twice in a row) is logged and passed over, so one
/// odd message never holds the mailbox back.
async fn read_one(
    state: &AppState,
    ws: WorkspaceId,
    i: &Integration,
    provider: &dyn MailProvider,
    m: &core_integrations::mailbox::FetchedMail,
    outcome: &mut PollOutcome,
) -> Result<(), ProviderError> {
    outcome.read += 1;
    let fetched;
    let raw = match &m.raw {
        Some(r) => r,
        None => {
            match super::inbox::holds(state, ws, i.id, &m.external_id).await {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(e) => return Err(ProviderError::Unavailable(e.to_string())),
            }
            fetched = match provider.raw(&m.external_id).await {
                Ok(Some(r)) => r,
                // Gone since it was listed.
                Ok(None) => return Ok(()),
                Err(e) if !e.is_temporary() => {
                    tracing::warn!(integration_id = %i.id, external_id = %m.external_id, error = %e, "a message could not be read, passed over");
                    return Ok(());
                }
                Err(e) => return Err(e),
            };
            &fetched
        }
    };
    let mut tries = 0;
    let read = loop {
        tries += 1;
        match super::inbox::ingest(state, ws, i, m, raw).await {
            Ok(r) => break r,
            Err(e) if tries < 2 => {
                tracing::warn!(integration_id = %i.id, error = %e, "reading a message failed, trying again");
            }
            Err(e) => {
                tracing::error!(integration_id = %i.id, external_id = %m.external_id, error = %e, "a message could not be read into the inbox, passed over");
                return Ok(());
            }
        }
    };
    if read.added {
        outcome.added += 1;
    }
    // A new thread part way through a provider conversation: read the rest
    // of it, so an answer read before what it answers is not lost.
    if read.new_thread
        && let Some(t) = &m.thread_id
    {
        for other in provider.conversation(t).await? {
            if other.external_id != m.external_id {
                Box::pin(read_one(state, ws, i, provider, &other, outcome)).await?;
            }
        }
    }
    Ok(())
}

/// Records a failed read and when to try again: soon at first, then less
/// often (2 minutes doubling to 6 hours). A grant that is gone waits for
/// its owner to connect again.
async fn failed(state: &AppState, ctx: &Ctx, i: &Integration, err: ProviderError) -> ApiResult<()> {
    let wait = match &err {
        ProviderError::RateLimited {
            retry_after: Some(s),
        } => Duration::seconds((*s).clamp(30, 6 * 3600) as i64),
        _ => {
            let doubling = 2i64.saturating_pow(i.failures.clamp(0, 16) as u32);
            Duration::minutes(2 * doubling).min(Duration::hours(6))
        }
    };
    let (status, next) = match &err {
        ProviderError::Revoked(_) => ("reconnect", None),
        // A blip keeps the badge as it was while it is retried.
        e if e.is_temporary() && i.failures + 1 < QUIET_FAILURES => (
            if i.status == "failing" {
                "failing"
            } else {
                "active"
            },
            Some(Utc::now() + wait),
        ),
        _ => ("failing", Some(Utc::now() + wait)),
    };
    let message = err.to_string();
    tracing::warn!(integration_id = %i.id, provider = %i.provider, error = %message, "reading a mailbox failed");
    let mut tx = begin(state, ctx).await?;
    core_db::integrations::failed(&mut tx, i.id, status, &message, next).await?;
    if status != i.status {
        record(
            &mut tx,
            ctx,
            Change {
                topic: "integration.health_changed",
                resource: ENTITY,
                object_id: Some(i.id),
                before: Some(json!({ "status": i.status })),
                // The provider's own words stay on the connection: they
                // can name people, and events leave OnHold.
                after: Some(json!({ "status": status })),
                event_extra: None,
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Sends a message from a connected mailbox, refreshing the token once if
/// the provider refuses it.
pub(crate) async fn send_from(
    state: &AppState,
    ws: WorkspaceId,
    i: &Integration,
    mail: &core_integrations::mailbox::OutgoingMail,
) -> Result<core_integrations::mailbox::SentMail, ProviderError> {
    let provider = provider_for(state, ws, i, false).await?;
    match provider.send(mail).await {
        Err(ProviderError::Unauthorized) => {
            provider_for(state, ws, i, true).await?.send(mail).await
        }
        other => other,
    }
}

/// One message read again from the mailbox it came from, to open an
/// attachment OnHold did not keep. The caller has checked the reader may
/// see the message.
pub(crate) async fn fetch_raw(
    state: &AppState,
    ctx: &Ctx,
    integration_id: Uuid,
    external_id: &str,
) -> ApiResult<Vec<u8>> {
    let mut tx = begin(state, ctx).await?;
    let i = core_db::integrations::get(&mut tx, integration_id).await?;
    tx.commit().await?;
    let gone = || {
        ApiError::conflict(
            "mailbox_unavailable",
            "this file stays in the mailbox it came with, which is no longer connected",
        )
    };
    let i = i
        .filter(|i| i.disconnected_at.is_none() && i.status != "reconnect")
        .ok_or_else(gone)?;
    let read = async {
        match provider_for(state, ctx.workspace_id, &i, false)
            .await?
            .raw(external_id)
            .await
        {
            Err(ProviderError::Unauthorized) => {
                provider_for(state, ctx.workspace_id, &i, true)
                    .await?
                    .raw(external_id)
                    .await
            }
            other => other,
        }
    };
    match read.await {
        Ok(Some(raw)) => Ok(raw),
        Ok(None) => Err(ApiError::conflict(
            "message_gone",
            "the message is no longer in the mailbox, so its files cannot be opened",
        )),
        Err(e) => Err(provider_error(i.mailbox(), e)),
    }
}

/// Marks a connection as needing to be connected again, after a send found
/// its grant gone.
pub(crate) async fn mark_revoked(
    state: &AppState,
    ws: WorkspaceId,
    i: &Integration,
    err: ProviderError,
) -> ApiResult<()> {
    failed(state, &Ctx::system(ws), i, err).await
}
