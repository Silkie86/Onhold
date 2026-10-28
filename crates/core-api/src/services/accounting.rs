//! A workspace's books in Xero or QuickBooks Online. An owner, admin or
//! finance member connects them from Settings → Accounting and chooses
//! where sales go, the account payments are paid into, which tax rate each
//! tax code is, and from which day OnHold sends what it issues. From then
//! on the worker sends invoices, credit notes and the payments recorded
//! against them as they happen, reads back what was paid in the books, and
//! on request imports invoices from before that day as past ones.

use std::collections::HashMap;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use core_db::Tx;
use core_db::accounting::{AllocationWork, Connection};
use core_domain::money::minor_unit_exponent;
use core_domain::{Ctx, Money, Permission, UserId, WorkspaceId};
use core_integrations::accounting::{self as books, Books, DocKind, Kind};
use core_integrations::oauth::{OAuthApp, Tokens};
use core_integrations::provider::ProviderError;
use data_encoding::BASE64URL_NOPAD;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use super::integrations::provider_error;
use super::invoices::InvoiceView;
use super::{Change, begin, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

pub const ENTITY: &str = "accounting";
/// Brings the books in line (`connection_id`).
pub const JOB_SYNC: &str = "accounting.sync";
/// Revokes a disconnected connection's grant and forgets it (`connection_id`).
pub const JOB_REVOKE: &str = "accounting.revoke";

const STATE_AAD: &[u8] = b"accounting_state";
const CONNECT_TTL_SECS: i64 = 30 * 60;
/// How long one sync may hold the connection.
const LEASE_SECS: f64 = 10.0 * 60.0;
/// Calls to the books in one sync; the rest follow a minute later, which
/// keeps well within Xero's 60 a minute.
const MAX_CALLS: usize = 50;
/// How often the books are looked at when nothing changes: what was paid
/// there is read back, and refused items are tried again.
const EVERY: Duration = Duration::hours(3);
/// A sync this long after the last read-back reads back again (a little
/// under [`EVERY`], so the scheduled sync always does).
const PAID_EVERY: Duration = Duration::minutes(3 * 60 - 15);
const QUIET_FAILURES: i32 = 3;
/// How long revoking a grant is tried before it is forgotten anyway.
const REVOKE_FOR: Duration = Duration::days(3);

#[derive(Serialize, Deserialize)]
struct Grant {
    refresh_token: String,
    access_token: String,
    expires_at: DateTime<Utc>,
}

fn grant_aad(id: Uuid) -> Vec<u8> {
    [b"accounting:".as_slice(), id.as_bytes()].concat()
}

fn seal(state: &AppState, id: Uuid, grant: &Grant) -> ApiResult<Vec<u8>> {
    let json = serde_json::to_vec(grant).map_err(ApiError::internal)?;
    Ok(state.config.keyring.encrypt(&json, &grant_aad(id)))
}

fn open(state: &AppState, c: &Connection) -> ApiResult<Grant> {
    let plain = state
        .config
        .keyring
        .decrypt(&c.secret, &grant_aad(c.id))
        .map_err(ApiError::internal)?;
    serde_json::from_slice(&plain).map_err(ApiError::internal)
}

fn kind(provider: &str) -> ApiResult<Kind> {
    Kind::parse(provider).ok_or_else(|| ApiError::invalid("provider", "use xero or quickbooks"))
}

fn oauth_app(state: &AppState, kind: Kind) -> ApiResult<&OAuthApp> {
    let app = match kind {
        Kind::Xero => state.config.accounting.xero.as_ref(),
        Kind::QuickBooks => state.config.accounting.quickbooks.as_ref(),
    };
    app.ok_or_else(|| {
        ApiError::conflict(
            "provider_not_available",
            format!("{} cannot be connected on this server", kind.name()),
        )
    })
}

fn scopes(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Xero => books::XERO_SCOPES,
        Kind::QuickBooks => books::QUICKBOOKS_SCOPES,
    }
}

fn redirect_uri(state: &AppState) -> String {
    state.config.app_url("/settings/accounting", &[])
}

fn client(state: &AppState, kind: Kind, token: String, tenant: &str) -> Books {
    let cfg = &state.config.accounting;
    let base = match kind {
        Kind::Xero => cfg.xero_api.clone(),
        Kind::QuickBooks => cfg.quickbooks_api.clone(),
    };
    Books::new(
        kind,
        cfg.http.clone(),
        base,
        token,
        tenant.to_owned(),
        minor_unit_exponent,
    )
}

// Views ----------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AccountingImport {
    /// `none`, `running`, `done` or `failed`.
    pub status: String,
    /// Past invoices brought in so far.
    pub imported: i32,
    pub error: Option<String>,
}

/// The workspace's books. Never carries the grant.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AccountingConnection {
    pub id: Uuid,
    /// `xero` or `quickbooks`.
    pub provider: String,
    /// The organisation (Xero) or company (QuickBooks).
    pub tenant_name: String,
    /// `active`, `failing` (being retried) or `reconnect`.
    pub status: String,
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub last_synced_at: Option<DateTime<Utc>>,
    /// When what was paid in the books was last read back.
    pub paid_checked_at: Option<DateTime<Utc>>,
    /// Xero: the revenue account's code. QuickBooks: the service item.
    pub sales_account: Option<String>,
    /// The account payments are paid into.
    pub bank_account: Option<String>,
    /// What is issued on or after this day is sent; earlier invoices can be imported.
    pub start_date: NaiveDate,
    /// QuickBooks works out US sales tax itself: tax codes are only taxable or not.
    pub automatic_tax: bool,
    /// Sending has started: where sales go is chosen.
    pub ready: bool,
    /// Things the books refused, waiting to be put right.
    pub refused: i64,
    pub import: AccountingImport,
    pub connected_at: DateTime<Utc>,
}

fn view(c: &Connection, refused: i64) -> AccountingConnection {
    AccountingConnection {
        id: c.id,
        provider: c.provider.clone(),
        tenant_name: c.tenant_name.clone(),
        status: c.status.clone(),
        last_error: c.last_error.clone(),
        last_error_at: c.last_error_at,
        last_synced_at: c.last_synced_at,
        paid_checked_at: c.paid_checked_at,
        sales_account: c.sales_account.clone(),
        bank_account: c.bank_account.clone(),
        start_date: c.start_date,
        automatic_tax: c.automatic_tax,
        ready: c.sales_account.is_some(),
        refused,
        import: AccountingImport {
            status: c.import_status.clone(),
            imported: c.imported,
            error: c.import_error.clone(),
        },
        connected_at: c.connected_at,
    }
}

fn audit_json(c: &Connection) -> Value {
    json!({
        "provider": c.provider,
        "tenant_id": c.tenant_id,
        "tenant_name": c.tenant_name,
        "sales_account": c.sales_account,
        "bank_account": c.bank_account,
        "start_date": c.start_date,
        "status": c.status,
    })
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Accounting {
    /// The live connection, if any.
    pub connection: Option<AccountingConnection>,
    /// Xero can be connected on this server.
    pub xero: bool,
    /// QuickBooks can be connected on this server.
    pub quickbooks: bool,
}

async fn live_view(tx: &mut Tx) -> ApiResult<Option<AccountingConnection>> {
    Ok(match core_db::accounting::live(tx).await? {
        Some(c) => {
            let refused = core_db::accounting::failure_count(tx, c.id).await?;
            Some(view(&c, refused))
        }
        None => None,
    })
}

pub async fn get(state: &AppState, ctx: &Ctx) -> ApiResult<Accounting> {
    ctx.authorize(Permission::InvoiceRead)?;
    let mut tx = begin(state, ctx).await?;
    let connection = live_view(&mut tx).await?;
    tx.commit().await?;
    Ok(Accounting {
        connection,
        xero: state.config.accounting.xero.is_some(),
        quickbooks: state.config.accounting.quickbooks.is_some(),
    })
}

async fn live(tx: &mut Tx) -> ApiResult<Connection> {
    core_db::accounting::live(tx)
        .await?
        .ok_or_else(|| ApiError::conflict("not_connected", "connect Xero or QuickBooks first"))
}

// Connecting ----------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct ConnectState {
    workspace: WorkspaceId,
    user: Option<UserId>,
    provider: String,
    nonce: String,
    exp: i64,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct StartAccounting {
    /// `xero` or `quickbooks`.
    pub provider: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AccountingStart {
    /// Send the person here; the provider sends them back to
    /// `/settings/accounting` with `code`, `state` (and QuickBooks'
    /// `realmId`), which the page passes to the finish call.
    pub url: String,
}

fn already_connected(c: &Connection) -> ApiError {
    ApiError::conflict(
        "already_connected",
        format!(
            "this workspace's books are in {} already; disconnect them first",
            c.tenant_name
        ),
    )
}

/// Books with another provider are connected: refused before anything is
/// granted. (The same provider may be connecting those books again.)
async fn other_books(state: &AppState, ctx: &Ctx, kind: Kind) -> ApiResult<()> {
    let mut tx = begin(state, ctx).await?;
    let live = core_db::accounting::live(&mut tx).await?;
    tx.commit().await?;
    match live {
        Some(c) if c.provider != kind.as_str() => Err(already_connected(&c)),
        _ => Ok(()),
    }
}

/// Where the person lets OnHold into their books.
pub async fn start(
    state: &AppState,
    ctx: &Ctx,
    req: StartAccounting,
) -> ApiResult<AccountingStart> {
    ctx.authorize(Permission::FinanceConfig)?;
    let kind = kind(&req.provider)?;
    let app = oauth_app(state, kind)?;
    other_books(state, ctx, kind).await?;
    let flow = ConnectState {
        workspace: ctx.workspace_id,
        user: ctx.user_id(),
        provider: kind.as_str().to_owned(),
        nonce: BASE64URL_NOPAD.encode(&core_domain::crypto::random_bytes::<16>()),
        exp: Utc::now().timestamp() + CONNECT_TTL_SECS,
    };
    let json = serde_json::to_vec(&flow).map_err(ApiError::internal)?;
    let sealed = BASE64URL_NOPAD.encode(&state.config.keyring.encrypt(&json, STATE_AAD));
    Ok(AccountingStart {
        url: app
            .authorize_exact(&sealed, &redirect_uri(state), scopes(kind))
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
pub struct FinishAccounting {
    /// From the provider's redirect.
    pub code: String,
    /// From the provider's redirect, as the start call made it.
    pub state: String,
    /// QuickBooks' company, from its redirect (`realmId`).
    pub realm_id: Option<String>,
}

/// Finishes connecting with what the provider sent back. Connecting the
/// same books again picks up where they were left.
pub async fn finish(
    state: &AppState,
    ctx: &Ctx,
    req: FinishAccounting,
) -> ApiResult<AccountingConnection> {
    ctx.authorize(Permission::FinanceConfig)?;
    let flow = open_state(state, &req.state)?;
    if flow.workspace != ctx.workspace_id || flow.user != ctx.user_id() {
        return Err(ApiError::bad_request(
            "connect_state_invalid",
            "this connection was started by someone else or in another workspace",
        ));
    }
    let kind = kind(&flow.provider)?;
    let app = oauth_app(state, kind)?;
    let name = kind.name();
    let http = &state.config.accounting.http;
    other_books(state, ctx, kind).await?;
    let tokens = app
        .exchange(http, req.code.trim(), &redirect_uri(state))
        .await
        .map_err(|e| provider_error(name, e))?;
    if !tokens.scopes.is_empty() && !core_integrations::oauth::granted(&tokens.scopes, scopes(kind))
    {
        return Err(ApiError::bad_request(
            "scopes_missing",
            format!(
                "OnHold needs to read and write invoices, payments and contacts in {name}; connect again and allow it"
            ),
        ));
    }
    let Some(refresh_token) = tokens.refresh_token.clone() else {
        return Err(ApiError::bad_request(
            "offline_access_missing",
            format!("{name} did not let OnHold stay connected; connect again"),
        ));
    };
    let (tenant_id, tenant_name, automatic_tax) = match kind {
        Kind::Xero => {
            let tenants = books::xero_tenants(
                http,
                &state.config.accounting.xero_api,
                &tokens.access_token,
            )
            .await
            .map_err(|e| provider_error(name, e))?;
            // The organisation just chosen is the one connected last.
            let t = tenants.into_iter().next().ok_or_else(|| {
                ApiError::bad_request(
                    "no_organisation",
                    "no Xero organisation was chosen; connect again and pick one",
                )
            })?;
            (t.id, t.name, false)
        }
        Kind::QuickBooks => {
            let realm = req
                .realm_id
                .as_deref()
                .map(str::trim)
                .filter(|r| !r.is_empty() && r.len() <= 100)
                .ok_or_else(|| {
                    ApiError::invalid(
                        "realm_id",
                        "QuickBooks sends the company's realmId back; pass it on",
                    )
                })?
                .to_owned();
            let b = client(state, kind, tokens.access_token.clone(), &realm);
            let company = b.name().await.map_err(|e| provider_error(name, e))?;
            let automatic = b
                .automatic_tax()
                .await
                .map_err(|e| provider_error(name, e))?;
            (realm, company, automatic)
        }
    };
    let tenant_name: String = tenant_name.chars().take(300).collect();
    let grant = Grant {
        refresh_token,
        access_token: tokens.access_token.clone(),
        expires_at: tokens.expires_at,
    };
    let mut tx = begin(state, ctx).await?;
    let id = match core_db::accounting::live(&mut tx).await? {
        Some(c) if c.provider == kind.as_str() && c.tenant_id == tenant_id => {
            core_db::accounting::lock(&mut tx, c.id).await?;
            core_db::accounting::revive(
                &mut tx,
                c.id,
                &tenant_name,
                automatic_tax,
                &seal(state, c.id, &grant)?,
                ctx.user_id(),
            )
            .await?;
            record(
                &mut tx,
                ctx,
                Change {
                    topic: "accounting.reconnected",
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
        Some(c) => {
            // Other books with the same provider: the grant just made is
            // for nothing. Intuit's are per company, so it goes, unless
            // another workspace uses that company (revoking would cut it
            // off too); Xero's can reach the connected organisation too, so
            // it is left to lapse.
            let elsewhere =
                core_db::accounting::books_in_use(&mut tx, kind.as_str(), &tenant_id, c.id).await?;
            tx.commit().await?;
            if kind == Kind::QuickBooks
                && !elsewhere
                && let Err(e) = app.revoke(http, &grant.refresh_token).await
            {
                tracing::info!(error = %e, "revoking an unused accounting grant failed");
            }
            return Err(already_connected(&c));
        }
        None => {
            let earlier =
                core_db::accounting::for_books(&mut tx, kind.as_str(), &tenant_id).await?;
            let id = match &earlier {
                Some(c) => {
                    // A revoke still waiting is called off: revoking the
                    // old grant would disconnect OnHold from these books
                    // altogether (Intuit), or the connection the new one
                    // shares (Xero). The old grant is simply forgotten.
                    core_db::accounting::revive(
                        &mut tx,
                        c.id,
                        &tenant_name,
                        automatic_tax,
                        &seal(state, c.id, &grant)?,
                        ctx.user_id(),
                    )
                    .await?;
                    c.id
                }
                None => {
                    let id = core_domain::ids::new_id();
                    let today = super::finance::today(&mut tx, ctx).await?;
                    core_db::accounting::insert(
                        &mut tx,
                        core_db::accounting::NewConnection {
                            id,
                            provider: kind.as_str(),
                            tenant_id: &tenant_id,
                            tenant_name: &tenant_name,
                            automatic_tax,
                            secret: &seal(state, id, &grant)?,
                            start_date: today,
                            connected_by: ctx.user_id(),
                        },
                    )
                    .await?;
                    id
                }
            };
            let saved = core_db::accounting::get(&mut tx, id)
                .await?
                .ok_or(ApiError::NotFound)?;
            record(
                &mut tx,
                ctx,
                Change {
                    topic: "accounting.connected",
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
    queue_sync(&mut tx, ctx.workspace_id, id).await?;
    let saved = core_db::accounting::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let refused = core_db::accounting::failure_count(&mut tx, id).await?;
    tx.commit().await?;
    Ok(view(&saved, refused))
}

/// Stops syncing. The worker then revokes the grant.
pub async fn disconnect(state: &AppState, ctx: &Ctx) -> ApiResult<()> {
    ctx.authorize(Permission::FinanceConfig)?;
    let mut tx = begin(state, ctx).await?;
    let c = live(&mut tx).await?;
    core_db::accounting::lock(&mut tx, c.id).await?;
    if core_db::accounting::disconnect(&mut tx, c.id).await? {
        queue_revoke(&mut tx, ctx.workspace_id, c.id, Utc::now()).await?;
        record(
            &mut tx,
            ctx,
            Change {
                topic: "accounting.disconnected",
                resource: ENTITY,
                object_id: Some(c.id),
                before: Some(audit_json(&c)),
                after: None,
                event_extra: None,
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Disconnects a closing workspace's books; the worker revokes the grant.
pub(crate) async fn disconnect_on_close(tx: &mut Tx, ctx: &Ctx) -> ApiResult<()> {
    if let Some(c) = core_db::accounting::live(tx).await?
        && core_db::accounting::disconnect(tx, c.id).await?
    {
        queue_revoke(tx, ctx.workspace_id, c.id, Utc::now()).await?;
        record(
            tx,
            ctx,
            Change {
                topic: "accounting.disconnected",
                resource: ENTITY,
                object_id: Some(c.id),
                before: Some(audit_json(&c)),
                after: None,
                event_extra: Some(json!({ "reason": "workspace_closed" })),
            },
        )
        .await?;
    }
    Ok(())
}

// Access ----------------------------------------------------------------------------------

/// A working access token, refreshed under the connection's lock so two
/// refreshes never race (Xero's refresh tokens are used once). `stale` is
/// a token the books just refused.
async fn access_token(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    stale: Option<&str>,
) -> Result<(String, String), ProviderError> {
    let fail = |e: ApiError| ProviderError::Unavailable(e.to_string());
    let mut tx = begin(state, ctx).await.map_err(fail)?;
    let c = core_db::accounting::lock(&mut tx, id)
        .await
        .map_err(|e| fail(e.into()))?
        .filter(|c| c.disconnected_at.is_none())
        .ok_or_else(|| ProviderError::Revoked("disconnected".into()))?;
    // A grant this server cannot read (its key retired) only helps if the
    // books are connected again.
    let grant = open(state, &c)
        .map_err(|_| ProviderError::Revoked("the saved connection could not be read".into()))?;
    let fresh_enough = grant.expires_at - Duration::seconds(60) > Utc::now();
    if fresh_enough && stale != Some(grant.access_token.as_str()) {
        tx.commit().await.map_err(|e| fail(e.into()))?;
        return Ok((grant.access_token, c.tenant_id));
    }
    let kind = kind(&c.provider).map_err(fail)?;
    let app = oauth_app(state, kind).map_err(|_| {
        ProviderError::Revoked("this server no longer connects to that provider".into())
    })?;
    let fresh: Tokens = app
        .refresh(&state.config.accounting.http, &grant.refresh_token)
        .await?;
    let next = Grant {
        refresh_token: fresh.refresh_token.clone().unwrap_or(grant.refresh_token),
        access_token: fresh.access_token.clone(),
        expires_at: fresh.expires_at,
    };
    let secret = seal(state, id, &next).map_err(fail)?;
    core_db::accounting::set_secret(&mut tx, id, &secret)
        .await
        .map_err(|e| fail(e.into()))?;
    tx.commit().await.map_err(|e| fail(e.into()))?;
    Ok((fresh.access_token, c.tenant_id))
}

/// Runs `call` against the books, refreshing the token once if refused.
/// A grant that no longer works marks the connection for connecting again.
async fn with_books<T, F, Fut>(state: &AppState, ctx: &Ctx, c: &Connection, call: F) -> ApiResult<T>
where
    F: Fn(Books) -> Fut,
    Fut: std::future::Future<Output = Result<T, ProviderError>>,
{
    let kind = kind(&c.provider)?;
    let mut stale: Option<String> = None;
    loop {
        let result: Result<T, (ProviderError, Option<String>)> = async {
            let (token, tenant) = access_token(state, ctx, c.id, stale.as_deref())
                .await
                .map_err(|e| (e, None))?;
            call(client(state, kind, token.clone(), &tenant))
                .await
                .map_err(|e| (e, Some(token)))
        }
        .await;
        match result {
            Ok(v) => return Ok(v),
            Err((ProviderError::Unauthorized, used)) if stale.is_none() => {
                stale = used.or(Some(String::new()));
            }
            Err((ProviderError::Revoked(m), _)) => {
                let mut tx = begin(state, ctx).await?;
                core_db::accounting::needs_reconnect(&mut tx, c.id, &m).await?;
                tx.commit().await?;
                return Err(provider_error(kind.name(), ProviderError::Revoked(m)));
            }
            Err((e, _)) => return Err(provider_error(kind.name(), e)),
        }
    }
}

// Settings ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AccountingChoice {
    pub id: String,
    pub name: String,
}

impl From<books::Choice> for AccountingChoice {
    fn from(c: books::Choice) -> Self {
        Self {
            id: c.id,
            name: c.name,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TaxCodeMapping {
    pub tax_code_id: Uuid,
    pub name: String,
    pub rate_bps: i32,
    /// The tax rate (Xero) or code (QuickBooks) it is in the books.
    pub remote: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AccountingOptions {
    /// Xero: revenue accounts. QuickBooks: service items.
    pub sales: Vec<AccountingChoice>,
    /// Accounts payments are paid into.
    pub banks: Vec<AccountingChoice>,
    /// Tax rates (Xero) or codes (QuickBooks).
    pub taxes: Vec<AccountingChoice>,
    /// OnHold's tax codes and which of those each is.
    pub tax_codes: Vec<TaxCodeMapping>,
    /// Settings in the books worth changing for OnHold, in words.
    pub warnings: Vec<String>,
}

async fn tax_codes(tx: &mut Tx, ctx: &Ctx, c: &Connection) -> ApiResult<Vec<TaxCodeMapping>> {
    super::finance::ensure_tax_codes(tx, ctx).await?;
    let map: HashMap<Uuid, String> = core_db::accounting::tax_map(tx, c.id)
        .await?
        .into_iter()
        .collect();
    Ok(core_db::finance::tax_codes(tx, false)
        .await?
        .into_iter()
        .map(|t| TaxCodeMapping {
            remote: map.get(&t.id).cloned(),
            tax_code_id: t.id,
            name: t.name,
            rate_bps: t.rate_bps,
        })
        .collect())
}

/// What there is to choose from in the books, read from them now.
pub async fn options(state: &AppState, ctx: &Ctx) -> ApiResult<AccountingOptions> {
    ctx.authorize(Permission::FinanceConfig)?;
    let mut tx = begin(state, ctx).await?;
    let c = live(&mut tx).await?;
    let codes = tax_codes(&mut tx, ctx, &c).await?;
    tx.commit().await?;
    let (o, auto_credit) = with_books(state, ctx, &c, |b| async move {
        Ok((b.options().await?, b.applies_credits_itself().await?))
    })
    .await?;
    let mut warnings = Vec::new();
    if auto_credit {
        warnings.push(
            "QuickBooks is set to apply credits automatically, so a credit note may be set against the customer's oldest invoice rather than the one it credits. Turn off \"Automatically apply credits\" in QuickBooks (Account and settings, Advanced, Automation).".to_owned(),
        );
    }
    Ok(AccountingOptions {
        warnings,
        sales: o.sales.into_iter().map(Into::into).collect(),
        banks: o.banks.into_iter().map(Into::into).collect(),
        taxes: o.taxes.into_iter().map(Into::into).collect(),
        tax_codes: codes,
    })
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct TaxCodeChoice {
    pub tax_code_id: Uuid,
    /// None: let the books decide (Xero uses the account's default).
    pub remote: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AccountingSettings {
    /// Xero: a revenue account's code. QuickBooks: a service item's id.
    pub sales_account: Option<String>,
    /// The account payments are paid into (Xero needs one to send payments).
    pub bank_account: Option<String>,
    /// What is issued on or after this day is sent. Default: unchanged.
    pub start_date: Option<NaiveDate>,
    #[serde(default)]
    pub tax_codes: Vec<TaxCodeChoice>,
}

fn choice(field: &str, v: Option<&str>) -> ApiResult<Option<String>> {
    match v.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) if s.len() > 100 => Err(ApiError::invalid(field, "must be at most 100 characters")),
        other => Ok(other.map(str::to_owned)),
    }
}

/// Saves the choices; sending starts once where sales go is chosen.
pub async fn update_settings(
    state: &AppState,
    ctx: &Ctx,
    s: AccountingSettings,
) -> ApiResult<AccountingConnection> {
    ctx.authorize(Permission::FinanceConfig)?;
    let sales = choice("sales_account", s.sales_account.as_deref())?;
    let bank = choice("bank_account", s.bank_account.as_deref())?;
    let mut tx = begin(state, ctx).await?;
    let before = live(&mut tx).await?;
    core_db::accounting::lock(&mut tx, before.id).await?;
    let codes: Vec<Uuid> = core_db::finance::tax_codes(&mut tx, true)
        .await?
        .into_iter()
        .map(|t| t.id)
        .collect();
    for (i, t) in s.tax_codes.iter().enumerate() {
        if !codes.contains(&t.tax_code_id) {
            return Err(ApiError::invalid(
                format!("tax_codes[{i}].tax_code_id"),
                "there is no such tax code",
            ));
        }
        let remote = choice(&format!("tax_codes[{i}].remote"), t.remote.as_deref())?;
        core_db::accounting::set_tax(&mut tx, before.id, t.tax_code_id, remote.as_deref()).await?;
    }
    core_db::accounting::set_settings(
        &mut tx,
        before.id,
        sales.as_deref(),
        bank.as_deref(),
        s.start_date.unwrap_or(before.start_date),
    )
    .await?;
    let after = core_db::accounting::get(&mut tx, before.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "accounting.updated",
            resource: ENTITY,
            object_id: Some(before.id),
            before: Some(audit_json(&before)),
            after: Some(audit_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    queue_sync(&mut tx, ctx.workspace_id, before.id).await?;
    let refused = core_db::accounting::failure_count(&mut tx, before.id).await?;
    tx.commit().await?;
    Ok(view(&after, refused))
}

/// Syncs now, reading back what was paid too, and tries refused items again.
pub async fn sync_now(state: &AppState, ctx: &Ctx) -> ApiResult<AccountingConnection> {
    ctx.authorize(Permission::InvoiceWrite)?;
    let mut tx = begin(state, ctx).await?;
    let c = live(&mut tx).await?;
    if c.status == "reconnect" {
        return Err(ApiError::conflict(
            "reconnect_required",
            "the books need to be connected again first",
        ));
    }
    if core_db::accounting::sync_now(&mut tx, c.id, true)
        .await?
        .is_some()
    {
        queue_sync(&mut tx, ctx.workspace_id, c.id).await?;
    }
    let after = core_db::accounting::get(&mut tx, c.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let refused = core_db::accounting::failure_count(&mut tx, c.id).await?;
    tx.commit().await?;
    Ok(view(&after, refused))
}

/// Imports the books' invoices from before the start date as past ones.
pub async fn import(state: &AppState, ctx: &Ctx) -> ApiResult<AccountingConnection> {
    ctx.authorize(Permission::FinanceConfig)?;
    ctx.authorize(Permission::InvoiceWrite)?;
    let mut tx = begin(state, ctx).await?;
    let c = live(&mut tx).await?;
    if c.status == "reconnect" {
        return Err(ApiError::conflict(
            "reconnect_required",
            "the books need to be connected again first",
        ));
    }
    if c.import_status == "running" {
        return Err(ApiError::conflict(
            "import_running",
            "past invoices are being imported already",
        ));
    }
    core_db::accounting::lock(&mut tx, c.id).await?;
    core_db::accounting::start_import(&mut tx, c.id).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "accounting.import_started",
            resource: ENTITY,
            object_id: Some(c.id),
            before: None,
            after: Some(json!({ "before": c.start_date })),
            event_extra: None,
        },
    )
    .await?;
    queue_sync(&mut tx, ctx.workspace_id, c.id).await?;
    let after = core_db::accounting::get(&mut tx, c.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let refused = core_db::accounting::failure_count(&mut tx, c.id).await?;
    tx.commit().await?;
    Ok(view(&after, refused))
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AccountingFailure {
    /// `invoice` (or credit note), `allocation` (a payment), `credit` (a
    /// credit note set against its invoice) or `contact`.
    pub kind: String,
    pub local_id: Uuid,
    /// The invoice's or credit note's number.
    pub reference: Option<String>,
    pub error: Option<String>,
    pub attempts: i32,
    pub at: DateTime<Utc>,
}

/// What the books refused, newest first.
pub async fn failures(state: &AppState, ctx: &Ctx) -> ApiResult<Vec<AccountingFailure>> {
    ctx.authorize(Permission::InvoiceRead)?;
    let mut tx = begin(state, ctx).await?;
    let rows = match core_db::accounting::live(&mut tx).await? {
        Some(c) => core_db::accounting::failures(&mut tx, c.id, 100).await?,
        None => Vec::new(),
    };
    tx.commit().await?;
    Ok(rows
        .into_iter()
        .map(|f| AccountingFailure {
            kind: f.kind,
            local_id: f.local_id,
            reference: f.reference,
            error: f.error,
            attempts: f.attempts,
            at: f.updated_at,
        })
        .collect())
}

// Queueing --------------------------------------------------------------------------------

/// One sync per connection and due time.
pub fn sync_key(id: Uuid, at: DateTime<Utc>) -> String {
    format!("{id}@{}", at.to_rfc3339())
}

/// One revoke per connection and due time.
pub fn revoke_key(id: Uuid, at: DateTime<Utc>) -> String {
    format!("revoke:{id}@{}", at.to_rfc3339())
}

async fn queue_sync(tx: &mut Tx, ws: WorkspaceId, id: Uuid) -> ApiResult<()> {
    let Some(at) = core_db::accounting::next_sync_at(tx, id).await? else {
        return Ok(());
    };
    let payload = json!({ "connection_id": id, "at": at });
    core_db::jobs::enqueue(
        tx,
        core_db::jobs::NewJob::new(Some(ws), JOB_SYNC, &payload).dedupe(&sync_key(id, at)),
    )
    .await?;
    Ok(())
}

async fn queue_revoke(tx: &mut Tx, ws: WorkspaceId, id: Uuid, at: DateTime<Utc>) -> ApiResult<()> {
    let payload = json!({ "connection_id": id });
    core_db::jobs::enqueue(
        tx,
        core_db::jobs::NewJob::new(Some(ws), JOB_REVOKE, &payload).dedupe(&revoke_key(id, at)),
    )
    .await?;
    Ok(())
}

/// Something the books keep changed (the event dispatcher): an invoice or
/// a payment. A sync already waiting covers it.
pub(crate) async fn changed(tx: &mut Tx, ws: WorkspaceId, topic: &str) -> ApiResult<()> {
    if !(topic.starts_with("invoice.") || topic.starts_with("payment.")) {
        return Ok(());
    }
    if matches!(
        topic,
        "invoice.created"
            | "invoice.updated"
            | "invoice.deleted"
            | "invoice.viewed"
            | "invoice.sent"
    ) {
        // Drafts, and changes the books do not keep.
        return Ok(());
    }
    let Some(id) = core_db::accounting::syncing(tx).await? else {
        return Ok(());
    };
    if let Some(at) = core_db::accounting::request_sync(tx, id).await? {
        let payload = json!({ "connection_id": id, "at": at });
        core_db::jobs::enqueue(
            tx,
            core_db::jobs::NewJob::new(Some(ws), JOB_SYNC, &payload).dedupe(&sync_key(id, at)),
        )
        .await?;
    }
    Ok(())
}

// Revoking --------------------------------------------------------------------------------

/// The worker's `accounting.revoke`: ends OnHold's access to a
/// disconnected connection's books and forgets the grant, trying again
/// while the provider is unavailable. Intuit's grant is revoked (it is for
/// that company only); for Xero only the connection to that organisation
/// is ended, since revoking the grant would end the person's connections
/// to every organisation. Books another workspace still uses are left
/// connected.
pub async fn revoke(state: &AppState, ws: WorkspaceId, id: Uuid) -> ApiResult<&'static str> {
    let ctx = Ctx::system(ws);
    let mut tx = begin(state, &ctx).await?;
    let Some(c) = core_db::accounting::to_revoke(&mut tx, id).await? else {
        tx.commit().await?;
        return Ok("nothing to revoke");
    };
    let grant = open(state, &c).ok();
    let kind = Kind::parse(&c.provider);
    let app = kind.and_then(|k| oauth_app(state, k).ok());
    let in_use =
        core_db::accounting::books_in_use(&mut tx, &c.provider, &c.tenant_id, c.id).await?;
    let mut rotated = None;
    if let (Some(g), Some(kind), Some(app), false) = (&grant, kind, app, in_use)
        && let Err(e) = end_access(state, kind, app, g, &c.tenant_id, &mut rotated).await
    {
        let gave_up = c
            .disconnected_at
            .is_none_or(|at| Utc::now() - at > REVOKE_FOR);
        if e.is_temporary() && !gave_up {
            // Xero's refresh tokens work once: trying again needs the new one.
            if let Some(next) = &rotated {
                let secret = seal(state, id, next)?;
                core_db::accounting::keep_revoking_secret(&mut tx, id, &secret).await?;
            }
            let at = Utc::now() + Duration::minutes(10);
            core_db::accounting::revoke_later(&mut tx, id, at).await?;
            tx.commit().await?;
            return Ok("revoking failed for now; trying again later");
        }
        tracing::warn!(connection_id = %id, error = %e, "revoking an accounting grant failed");
    }
    core_db::accounting::forget_grant(&mut tx, id).await?;
    tx.commit().await?;
    Ok(match (&grant, in_use) {
        (Some(_), false) => "revoked",
        (Some(_), true) => "forgotten; another workspace uses these books",
        (None, _) => "forgotten",
    })
}

async fn end_access(
    state: &AppState,
    kind: Kind,
    app: &OAuthApp,
    g: &Grant,
    tenant: &str,
    rotated: &mut Option<Grant>,
) -> Result<(), ProviderError> {
    let http = &state.config.accounting.http;
    match kind {
        Kind::QuickBooks => app.revoke(http, &g.refresh_token).await,
        Kind::Xero => {
            let token = if g.expires_at - Duration::seconds(60) > Utc::now() {
                g.access_token.clone()
            } else {
                match app.refresh(http, &g.refresh_token).await {
                    Ok(t) => {
                        *rotated = Some(Grant {
                            refresh_token: t
                                .refresh_token
                                .unwrap_or_else(|| g.refresh_token.clone()),
                            access_token: t.access_token.clone(),
                            expires_at: t.expires_at,
                        });
                        t.access_token
                    }
                    // Nothing left to end.
                    Err(ProviderError::Revoked(_)) => return Ok(()),
                    Err(e) => return Err(e),
                }
            };
            match books::xero_disconnect(http, &state.config.accounting.xero_api, &token, tenant)
                .await
            {
                Err(ProviderError::Revoked(_)) => Ok(()),
                other => other,
            }
        }
    }
}

// Syncing ---------------------------------------------------------------------------------

/// What a sync did, for the job's log.
#[derive(Debug, Default)]
pub struct SyncOutcome {
    pub sent: usize,
    pub voided: usize,
    pub payments: usize,
    pub refused: usize,
    pub paid_in_books: usize,
    pub imported: usize,
    pub more: bool,
    pub paid_checked: bool,
    pub skipped: Option<&'static str>,
    /// The connection was disconnected, or taken over, part way.
    pub lost: bool,
}

impl std::fmt::Display for SyncOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.skipped {
            Some(why) => write!(f, "skipped: {why}"),
            None => write!(
                f,
                "{} sent, {} voided, {} payments, {} refused, {} paid in the books, {} imported{}",
                self.sent,
                self.voided,
                self.payments,
                self.refused,
                self.paid_in_books,
                self.imported,
                if self.more { ", more to come" } else { "" }
            ),
        }
    }
}

/// The worker's `accounting.sync`: sends what the books do not have yet,
/// reads back what was paid there, and imports a page of past invoices
/// when asked. Provider failures are recorded on the connection and tried
/// again with growing waits; what the books refuse is kept per item.
pub async fn sync(state: &AppState, ws: WorkspaceId, id: Uuid) -> ApiResult<SyncOutcome> {
    let ctx = Ctx::system(ws);
    let mut tx = begin(state, &ctx).await?;
    let Some(token) = core_db::accounting::claim(&mut tx, id, LEASE_SECS).await? else {
        // Another sync holds it: one follows it.
        core_db::accounting::request_sync(&mut tx, id).await?;
        tx.commit().await?;
        return Ok(SyncOutcome {
            skipped: Some("disconnected, waiting to be connected again, or syncing"),
            ..SyncOutcome::default()
        });
    };
    let c = core_db::accounting::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if c.sales_account.is_none() {
        core_db::accounting::idle(&mut tx, id, token).await?;
        tx.commit().await?;
        return Ok(SyncOutcome {
            skipped: Some("where sales go is not chosen yet"),
            ..SyncOutcome::default()
        });
    }
    let tax: HashMap<Uuid, String> = core_db::accounting::tax_map(&mut tx, id)
        .await?
        .into_iter()
        .collect();
    tx.commit().await?;
    let kind = kind(&c.provider)?;
    let check_paid = c
        .paid_checked_at
        .is_none_or(|at| Utc::now() - at > PAID_EVERY);
    let mut stale: Option<String> = None;
    loop {
        let result: Result<SyncOutcome, (ProviderError, Option<String>)> = async {
            let (access, tenant) = access_token(state, &ctx, id, stale.as_deref())
                .await
                .map_err(|e| (e, None))?;
            let run = Run {
                state,
                ctx: &ctx,
                c: &c,
                token,
                tax: &tax,
                b: client(state, kind, access.clone(), &tenant),
                contacts: HashMap::new(),
                calls: 0,
                out: SyncOutcome {
                    paid_checked: check_paid,
                    ..SyncOutcome::default()
                },
            };
            run.go(check_paid).await.map_err(|e| (e, Some(access)))
        }
        .await;
        match result {
            Ok(out) if out.lost => return Ok(out),
            Ok(out) => {
                let next = if out.more {
                    Utc::now() + Duration::minutes(1)
                } else {
                    Utc::now() + EVERY
                };
                let mut tx = begin(state, &ctx).await?;
                core_db::accounting::synced(&mut tx, id, token, next, out.paid_checked).await?;
                tx.commit().await?;
                return Ok(out);
            }
            Err((ProviderError::Unauthorized, used)) if stale.is_none() => {
                stale = used.or(Some(String::new()));
            }
            Err((e, _)) => {
                failed(state, &ctx, &c, token, e).await?;
                return Ok(SyncOutcome::default());
            }
        }
    }
}

/// A database failure part way through: tried again later.
fn invalid(e: impl std::fmt::Display) -> ProviderError {
    ProviderError::Unavailable(e.to_string())
}

/// A stable key for one try at creating `local` in the books: the same
/// after a lost answer, a new one after a refusal.
fn create_key(connection: Uuid, kind: &str, local: Uuid, attempts: i32) -> String {
    let mut h = Sha256::new();
    h.update(connection.as_bytes());
    h.update(kind.as_bytes());
    h.update(local.as_bytes());
    h.update(attempts.to_le_bytes());
    h.finalize()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// One sync holding the connection (`token`).
struct Run<'a> {
    state: &'a AppState,
    ctx: &'a Ctx,
    c: &'a Connection,
    token: Uuid,
    tax: &'a HashMap<Uuid, String>,
    b: Books,
    /// Name-only payers' contacts, found once per sync.
    contacts: HashMap<String, String>,
    calls: usize,
    out: SyncOutcome,
}

/// What a step against the books came to: done, or refused (kept on the
/// item, and the sync goes on).
enum Step<T> {
    Done(T),
    Refused(String),
}

fn step<T>(r: Result<T, ProviderError>) -> Result<Step<T>, ProviderError> {
    match r {
        Ok(v) => Ok(Step::Done(v)),
        Err(ProviderError::Refused { message, .. }) => Ok(Step::Refused(message)),
        Err(e) => Err(e),
    }
}

impl Run<'_> {
    async fn tx(&self) -> Result<Tx, ProviderError> {
        begin(self.state, self.ctx).await.map_err(invalid)
    }

    /// Keeps the lease and checks it is still this sync's.
    async fn still_mine(&self, tx: &mut Tx) -> Result<bool, ProviderError> {
        core_db::accounting::renew(tx, self.c.id, self.token, LEASE_SECS)
            .await
            .map_err(invalid)
    }

    fn budget(&mut self) -> bool {
        if self.calls >= MAX_CALLS {
            self.out.more = true;
            false
        } else {
            true
        }
    }

    /// Records a link, if the sync still holds the connection.
    async fn save(
        &mut self,
        kind: &str,
        local: Uuid,
        remote: Option<&str>,
        state: &str,
        status: &str,
    ) -> Result<bool, ProviderError> {
        let mut tx = self.tx().await?;
        if !self.still_mine(&mut tx).await? {
            self.out.lost = true;
            return Ok(false);
        }
        core_db::accounting::save_link(
            &mut tx,
            self.c.id,
            kind,
            local,
            remote,
            Some(state),
            status,
        )
        .await
        .map_err(invalid)?;
        tx.commit().await.map_err(invalid)?;
        Ok(true)
    }

    async fn refuse(&mut self, kind: &str, local: Uuid, why: &str) -> Result<bool, ProviderError> {
        let mut tx = self.tx().await?;
        if !self.still_mine(&mut tx).await? {
            self.out.lost = true;
            return Ok(false);
        }
        core_db::accounting::fail_link(&mut tx, self.c.id, kind, local, why)
            .await
            .map_err(invalid)?;
        tx.commit().await.map_err(invalid)?;
        self.out.refused += 1;
        Ok(true)
    }

    /// The books refused a payment or credit asked for again after its
    /// answer was lost: kept as on its way, and asked again later.
    async fn refuse_replay(
        &mut self,
        kind: &str,
        local: Uuid,
        why: &str,
    ) -> Result<bool, ProviderError> {
        let mut tx = self.tx().await?;
        if !self.still_mine(&mut tx).await? {
            self.out.lost = true;
            return Ok(false);
        }
        core_db::accounting::refuse_sending(&mut tx, self.c.id, kind, local, why)
            .await
            .map_err(invalid)?;
        tx.commit().await.map_err(invalid)?;
        self.out.refused += 1;
        Ok(true)
    }

    /// Notes a payment or credit about to be asked for, so a lost answer
    /// is asked for again just so.
    async fn mark_sending(
        &mut self,
        kind: &str,
        local: Uuid,
        amount: i64,
    ) -> Result<bool, ProviderError> {
        let mut tx = self.tx().await?;
        if !self.still_mine(&mut tx).await? {
            self.out.lost = true;
            return Ok(false);
        }
        core_db::accounting::sending(&mut tx, self.c.id, kind, local, amount)
            .await
            .map_err(invalid)?;
        tx.commit().await.map_err(invalid)?;
        Ok(true)
    }

    async fn go(mut self, check_paid: bool) -> Result<SyncOutcome, ProviderError> {
        self.documents().await?;
        if !self.out.lost {
            self.credits().await?;
        }
        if !self.out.lost {
            self.allocations().await?;
        }
        if !self.out.lost && check_paid && !self.out.more {
            self.paid_in_books().await?;
        } else {
            // Not read back this time: the next sync does.
            self.out.paid_checked = false;
        }
        if !self.out.lost && self.c.import_status == "running" && !self.out.more {
            self.import_page().await?;
        }
        Ok(self.out)
    }

    async fn invoice(&self, id: Uuid) -> Result<Option<InvoiceView>, ProviderError> {
        let mut tx = self.tx().await?;
        let inv = super::invoices::load(&mut tx, id).await.map_err(invalid)?;
        tx.commit().await.map_err(invalid)?;
        Ok(inv)
    }

    /// The payer's contact in the books, found by name or added. `attempts`
    /// are the refusals of what needs it, so a new try after a refusal is
    /// a new request.
    async fn contact(
        &mut self,
        inv: &InvoiceView,
        attempts: i32,
    ) -> Result<Step<String>, ProviderError> {
        let local = inv.payer_organisation_id.or(inv.payer_talent_id);
        let name = inv.payer_name.trim().to_owned();
        if let Some(l) = local {
            let mut tx = self.tx().await?;
            let link = core_db::accounting::link(&mut tx, self.c.id, "contact", l)
                .await
                .map_err(invalid)?;
            tx.commit().await.map_err(invalid)?;
            if let Some(remote) = link.and_then(|k| k.remote_id) {
                if attempts > 0 && self.c.provider == Kind::QuickBooks.as_str() {
                    // Refused before: perhaps for the customer having been
                    // made inactive in QuickBooks since.
                    self.calls += 1;
                    if let Step::Refused(m) = step(self.b.reactivate_contact(&remote).await)? {
                        return Ok(Step::Refused(m));
                    }
                }
                return Ok(Step::Done(remote));
            }
        } else if let Some(remote) = self.contacts.get(&name.to_lowercase()) {
            return Ok(Step::Done(remote.clone()));
        }
        self.calls += 1;
        let found = match step(self.b.find_contact(&name).await)? {
            Step::Done(f) => f,
            Step::Refused(m) => return Ok(Step::Refused(m)),
        };
        let remote = match found {
            Some(r) => r,
            None => {
                let email = match inv.payer_contact_id {
                    Some(c) => {
                        let mut tx = self.tx().await?;
                        let contact = core_db::contacts::get(&mut tx, c).await.map_err(invalid)?;
                        tx.commit().await.map_err(invalid)?;
                        contact.and_then(|c| c.emails.into_iter().next())
                    }
                    None => None,
                };
                self.calls += 1;
                let key = create_key(self.c.id, "contact", local.unwrap_or(inv.id), attempts);
                match step(self.b.create_contact(&name, email.as_deref(), &key).await)? {
                    Step::Done(r) => r,
                    Step::Refused(m) => {
                        return Ok(Step::Refused(format!("adding the customer: {m}")));
                    }
                }
            }
        };
        match local {
            Some(l) => {
                if !self
                    .save("contact", l, Some(&remote), "live", "synced")
                    .await?
                {
                    return Ok(Step::Refused("disconnected part way".into()));
                }
            }
            None => {
                self.contacts.insert(name.to_lowercase(), remote.clone());
            }
        }
        Ok(Step::Done(remote))
    }

    fn document(&self, inv: &InvoiceView, contact: String) -> books::Document {
        let automatic = self.c.automatic_tax;
        books::Document {
            kind: if inv.kind == "credit_note" {
                DocKind::CreditNote
            } else {
                DocKind::Invoice
            },
            contact,
            number: inv.reference.clone().unwrap_or_else(|| inv.id.to_string()),
            date: inv.issue_date.unwrap_or_else(|| Utc::now().date_naive()),
            due: if inv.kind == "credit_note" {
                None
            } else {
                inv.due_date
            },
            currency: inv.currency.clone(),
            exponent: minor_unit_exponent(&inv.currency),
            lines: inv
                .line_items
                .iter()
                .map(|l| books::Line {
                    description: if l.description.trim().is_empty() {
                        "Booking".to_owned()
                    } else {
                        l.description.clone()
                    },
                    quantity: l.quantity,
                    unit_minor: l.unit_minor,
                    amount_minor: l.amount_minor,
                    tax: l
                        .tax_code_id
                        .and_then(|t| self.tax.get(&t).cloned())
                        .or_else(|| {
                            automatic
                                .then(|| if l.tax_rate_bps > 0 { "TAX" } else { "NON" }.to_owned())
                        }),
                })
                .collect(),
            sales: self.c.sales_account.clone().unwrap_or_default(),
            automatic_tax: automatic,
        }
    }

    /// Invoices and credit notes: sent when issued, voided when voided.
    async fn documents(&mut self) -> Result<(), ProviderError> {
        let ids = {
            let mut tx = self.tx().await?;
            let ids = core_db::accounting::pending_documents(
                &mut tx,
                self.c.id,
                self.c.start_date,
                self.c.retry_before,
                MAX_CALLS as i64,
            )
            .await
            .map_err(invalid)?;
            tx.commit().await.map_err(invalid)?;
            ids
        };
        for id in ids {
            if !self.budget() || self.out.lost {
                break;
            }
            let Some(inv) = self.invoice(id).await? else {
                continue;
            };
            let link = {
                let mut tx = self.tx().await?;
                let l = core_db::accounting::link(&mut tx, self.c.id, "invoice", id)
                    .await
                    .map_err(invalid)?;
                tx.commit().await.map_err(invalid)?;
                l
            };
            let doc_kind = if inv.kind == "credit_note" {
                DocKind::CreditNote
            } else {
                DocKind::Invoice
            };
            let voided = inv.status == core_domain::InvoiceStatus::Void;
            match link.as_ref().and_then(|l| l.remote_id.clone()) {
                None if voided => {
                    // Never in the books: nothing to void.
                    self.save("invoice", id, None, "void", "synced").await?;
                }
                None => {
                    let attempts = link.as_ref().map_or(0, |l| l.attempts);
                    let contact = match self.contact(&inv, attempts).await? {
                        Step::Done(c) => c,
                        Step::Refused(m) => {
                            self.refuse("invoice", id, &m).await?;
                            continue;
                        }
                    };
                    let doc = self.document(&inv, contact);
                    let key = create_key(
                        self.c.id,
                        "invoice",
                        id,
                        link.as_ref().map_or(0, |l| l.attempts),
                    );
                    self.calls += 1;
                    match step(self.b.create_document(&doc, &key).await)? {
                        Step::Done(remote) => {
                            if self
                                .save("invoice", id, Some(&remote), "issued", "synced")
                                .await?
                            {
                                self.out.sent += 1;
                            }
                        }
                        Step::Refused(m) => {
                            self.refuse("invoice", id, &m).await?;
                        }
                    }
                }
                Some(remote) if voided => {
                    // A credit note set against its invoice is taken off it first.
                    if doc_kind == DocKind::CreditNote {
                        let applied = {
                            let mut tx = self.tx().await?;
                            let l = core_db::accounting::link(&mut tx, self.c.id, "credit", id)
                                .await
                                .map_err(invalid)?;
                            tx.commit().await.map_err(invalid)?;
                            l
                        };
                        if let Some(a) = applied
                            .as_ref()
                            .filter(|a| a.state.as_deref() == Some("sending"))
                        {
                            // Setting it against its invoice is still
                            // waiting for an answer: that comes first (in
                            // this sync, unless the books refused it last
                            // time, when it waits for the next retry).
                            if a.status != "failed" {
                                self.out.more = true;
                            }
                            continue;
                        }
                        if let Some(a) = applied
                            .filter(|a| a.state.as_deref() == Some("live"))
                            .and_then(|a| a.remote_id)
                        {
                            self.calls += 1;
                            if let Step::Refused(m) =
                                step(self.b.unapply_credit(&remote, &a).await)?
                            {
                                self.refuse("invoice", id, &m).await?;
                                continue;
                            }
                            if !self
                                .save("credit", id, Some(&a), "removed", "synced")
                                .await?
                            {
                                break;
                            }
                        }
                    }
                    self.calls += 1;
                    match step(self.b.void_document(doc_kind, &remote).await)? {
                        Step::Done(()) => {
                            if self
                                .save("invoice", id, Some(&remote), "void", "synced")
                                .await?
                            {
                                self.out.voided += 1;
                            }
                        }
                        Step::Refused(m) => {
                            self.refuse("invoice", id, &m).await?;
                        }
                    }
                }
                Some(remote) => {
                    // In the books already (a refusal since was for
                    // something else): as it should be.
                    self.save("invoice", id, Some(&remote), "issued", "synced")
                        .await?;
                }
            }
        }
        Ok(())
    }

    /// Credit notes set against the invoices they credit.
    async fn credits(&mut self) -> Result<(), ProviderError> {
        let ids = {
            let mut tx = self.tx().await?;
            let ids = core_db::accounting::pending_credits(
                &mut tx,
                self.c.id,
                self.c.retry_before,
                MAX_CALLS as i64,
            )
            .await
            .map_err(invalid)?;
            tx.commit().await.map_err(invalid)?;
            ids
        };
        for id in ids {
            if !self.budget() || self.out.lost {
                break;
            }
            let Some(note) = self.invoice(id).await? else {
                continue;
            };
            let Some(original) = note.credits_invoice_id else {
                continue;
            };
            let (credit, invoice, applied) = {
                let mut tx = self.tx().await?;
                let c = core_db::accounting::link(&mut tx, self.c.id, "invoice", id)
                    .await
                    .map_err(invalid)?;
                let i = core_db::accounting::link(&mut tx, self.c.id, "invoice", original)
                    .await
                    .map_err(invalid)?;
                let a = core_db::accounting::link(&mut tx, self.c.id, "credit", id)
                    .await
                    .map_err(invalid)?;
                tx.commit().await.map_err(invalid)?;
                (c.and_then(|l| l.remote_id), i.and_then(|l| l.remote_id), a)
            };
            let (Some(credit), Some(invoice)) = (credit, invoice) else {
                continue;
            };
            let attempts = applied.as_ref().map_or(0, |l| l.attempts);
            let contact = match self.contact(&note, attempts).await? {
                Step::Done(c) => c,
                Step::Refused(m) => {
                    self.refuse("credit", id, &m).await?;
                    continue;
                }
            };
            let exponent = minor_unit_exponent(&note.currency);
            let resend = applied
                .as_ref()
                .filter(|l| l.state.as_deref() == Some("sending"))
                .and_then(|l| l.amount_minor);
            let amount = match resend {
                // Asked for before, and the answer was lost: asked for
                // again just so.
                Some(a) => a,
                None => {
                    // No more than the invoice still owes in the books, nor
                    // than is left of the credit there.
                    self.calls += 1;
                    let owed = self
                        .b
                        .invoice_states(std::slice::from_ref(&invoice))
                        .await?
                        .into_iter()
                        .next()
                        .map(|s| s.due_minor);
                    let mut amount = note.total.minor.min(owed.unwrap_or(0));
                    if self.c.provider == Kind::QuickBooks.as_str() {
                        self.calls += 1;
                        match step(self.b.remaining_credit(&credit, exponent).await)? {
                            Step::Done(Some(left)) => amount = amount.min(left),
                            Step::Done(None) => {}
                            Step::Refused(m) => {
                                self.refuse("credit", id, &m).await?;
                                continue;
                            }
                        }
                    }
                    if amount <= 0 {
                        self.save("credit", id, None, "live", "synced").await?;
                        continue;
                    }
                    if !self.mark_sending("credit", id, amount).await? {
                        break;
                    }
                    amount
                }
            };
            let key = create_key(self.c.id, "credit", id, attempts);
            self.calls += 1;
            match step(
                self.b
                    .apply_credit(
                        &credit,
                        &invoice,
                        &contact,
                        amount,
                        exponent,
                        note.issue_date.unwrap_or_else(|| Utc::now().date_naive()),
                        &key,
                    )
                    .await,
            )? {
                Step::Done(applied) => {
                    self.save("credit", id, Some(&applied), "live", "synced")
                        .await?;
                }
                Step::Refused(m) if resend.is_some() => {
                    self.refuse_replay("credit", id, &m).await?;
                }
                Step::Refused(m) => {
                    self.refuse("credit", id, &m).await?;
                }
            }
        }
        Ok(())
    }

    /// Asks the books for the payment `a` stands for, of `amount`.
    async fn send_payment(
        &mut self,
        a: &AllocationWork,
        amount: i64,
    ) -> Result<Step<String>, ProviderError> {
        let Some(inv) = self.invoice(a.invoice_id).await? else {
            return Ok(Step::Refused("the invoice is no longer in OnHold".into()));
        };
        let contact = match self.contact(&inv, a.attempts).await? {
            Step::Done(c) => c,
            Step::Refused(m) => return Ok(Step::Refused(m)),
        };
        let payment = books::Payment {
            invoice: a.remote_invoice.clone(),
            contact,
            amount_minor: amount,
            exponent: minor_unit_exponent(&a.currency),
            date: a.paid_on,
            account: self.c.bank_account.clone(),
            reference: a.reference.clone(),
        };
        let key = create_key(self.c.id, "allocation", a.id, a.attempts);
        self.calls += 1;
        step(self.b.create_payment(&payment, &key).await)
    }

    /// Payments recorded against invoices in the books: sent, or taken
    /// back when the payment is voided. Xero needs the account payments
    /// are paid into; without one, none are sent.
    async fn allocations(&mut self) -> Result<(), ProviderError> {
        let send_new = !(self.c.provider == Kind::Xero.as_str() && self.c.bank_account.is_none());
        let work: Vec<AllocationWork> = {
            let mut tx = self.tx().await?;
            let w = core_db::accounting::pending_allocations(
                &mut tx,
                self.c.id,
                send_new,
                self.c.retry_before,
                MAX_CALLS as i64,
            )
            .await
            .map_err(invalid)?;
            tx.commit().await.map_err(invalid)?;
            w
        };
        let sending = |a: &AllocationWork| {
            a.state
                .as_deref()
                .filter(|s| *s == "sending")
                .and(a.sent_minor)
        };
        // What the invoices still owe in the books, so nothing is paid twice.
        let wanted: Vec<String> = {
            let mut v: Vec<String> = work
                .iter()
                .filter(|a| !a.removed && sending(a).is_none())
                .map(|a| a.remote_invoice.clone())
                .collect();
            v.sort();
            v.dedup();
            v
        };
        let mut owed: HashMap<String, i64> = HashMap::new();
        for chunk in wanted.chunks(books::STATE_BATCH) {
            self.calls += 1;
            for s in self.b.invoice_states(chunk).await? {
                if !s.voided {
                    owed.insert(s.id, s.due_minor);
                }
            }
        }
        for a in work {
            if !self.budget() || self.out.lost {
                break;
            }
            if a.removed {
                let remote = match (&a.remote_id, a.state.as_deref(), sending(&a)) {
                    (Some(remote), Some("live"), _) => Some(remote.clone()),
                    // Its answer was lost: asked for again (same key), the
                    // books give its id, and it is taken back.
                    (_, _, Some(amount)) => match self.send_payment(&a, amount).await? {
                        Step::Done(remote) => Some(remote),
                        Step::Refused(m) => {
                            // That does not show it never landed: kept
                            // as on its way, and asked again later.
                            self.refuse_replay("allocation", a.id, &m).await?;
                            continue;
                        }
                    },
                    // Never in the books.
                    _ => None,
                };
                if self.out.lost {
                    break;
                }
                match remote {
                    Some(remote) => {
                        self.calls += 1;
                        match step(self.b.delete_payment(&remote).await)? {
                            Step::Done(()) => {
                                self.save("allocation", a.id, Some(&remote), "removed", "synced")
                                    .await?;
                            }
                            Step::Refused(m) => {
                                // Kept as sent, so taking it back is tried again.
                                self.save("allocation", a.id, Some(&remote), "live", "synced")
                                    .await?;
                                self.refuse("allocation", a.id, &m).await?;
                            }
                        }
                    }
                    None => {
                        self.save("allocation", a.id, None, "removed", "synced")
                            .await?;
                    }
                }
                continue;
            }
            let replay = sending(&a);
            let amount = match replay {
                Some(amount) => amount,
                None => {
                    let Some(due) = owed.get(&a.remote_invoice).copied() else {
                        self.refuse("allocation", a.id, "the invoice is no longer in the books")
                            .await?;
                        continue;
                    };
                    let amount = a.amount_minor.min(due);
                    if amount <= 0 {
                        // Paid in the books already.
                        self.save("allocation", a.id, None, "live", "synced")
                            .await?;
                        continue;
                    }
                    if !self.mark_sending("allocation", a.id, amount).await? {
                        break;
                    }
                    owed.insert(a.remote_invoice.clone(), due - amount);
                    amount
                }
            };
            match self.send_payment(&a, amount).await? {
                Step::Done(remote) => {
                    if self
                        .save("allocation", a.id, Some(&remote), "live", "synced")
                        .await?
                    {
                        self.out.payments += 1;
                    }
                }
                Step::Refused(m) if replay.is_some() => {
                    self.refuse_replay("allocation", a.id, &m).await?;
                }
                Step::Refused(m) => {
                    if let Some(due) = owed.get_mut(&a.remote_invoice) {
                        *due += amount;
                    }
                    self.refuse("allocation", a.id, &m).await?;
                }
            }
        }
        Ok(())
    }

    /// Reads back what was paid in the books and records it in OnHold, so
    /// invoices paid there show paid here.
    async fn paid_in_books(&mut self) -> Result<(), ProviderError> {
        let name = Kind::parse(&self.c.provider).map_or("the books", Kind::name);
        // Goes on from where a read-back cut short stopped.
        let mut after = self.c.paid_cursor.unwrap_or_else(Uuid::nil);
        loop {
            let page = {
                let mut tx = self.tx().await?;
                let p = core_db::accounting::open_invoices(
                    &mut tx,
                    self.c.id,
                    after,
                    books::STATE_BATCH as i64,
                )
                .await
                .map_err(invalid)?;
                tx.commit().await.map_err(invalid)?;
                p
            };
            let Some(last) = page.last() else {
                break;
            };
            if !self.budget() {
                self.out.paid_checked = false;
                let mut tx = self.tx().await?;
                core_db::accounting::set_paid_cursor(&mut tx, self.c.id, self.token, Some(after))
                    .await
                    .map_err(invalid)?;
                tx.commit().await.map_err(invalid)?;
                break;
            }
            after = last.id;
            self.calls += 1;
            let ids: Vec<String> = page.iter().map(|i| i.remote_id.clone()).collect();
            let states: HashMap<String, books::InvoiceState> = self
                .b
                .invoice_states(&ids)
                .await?
                .into_iter()
                .map(|s| (s.id.clone(), s))
                .collect();
            for i in &page {
                let Some(s) = states.get(&i.remote_id).filter(|s| !s.voided) else {
                    continue;
                };
                let settled_there = s.total_minor - s.due_minor;
                let settled_here = i.paid_minor + i.credited_minor;
                let owed_here = (i.total_minor - settled_here).max(0);
                let new = (settled_there - settled_here).min(owed_here);
                if new <= 0 {
                    continue;
                }
                let mut tx = self.tx().await?;
                if !self.still_mine(&mut tx).await? {
                    self.out.lost = true;
                    return Ok(());
                }
                // A payment from here still on its way there, or being
                // taken back: what the books show is not final yet.
                if core_db::accounting::allocation_work_pending(&mut tx, self.c.id, i.id)
                    .await
                    .map_err(invalid)?
                {
                    continue;
                }
                let paid = super::payments::create_in(
                    &mut tx,
                    self.ctx,
                    super::payments::NewPayment {
                        amount: Money {
                            minor: new,
                            currency: i.currency.clone(),
                        },
                        method: Some("other".into()),
                        paid_on: None,
                        reference: Some(format!("Paid in {name}")),
                        organisation_id: None,
                        booking_id: None,
                        allocations: vec![super::payments::AllocationInput {
                            invoice_id: i.id,
                            amount_minor: Some(new),
                        }],
                        notes: None,
                    },
                    None,
                )
                .await;
                let payment = match paid {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(invoice_id = %i.id, error = %e, "recording a payment made in the books failed");
                        continue;
                    }
                };
                // Came from the books: never sent back.
                for a in core_db::payments::allocations(&mut tx, Some(payment), None)
                    .await
                    .map_err(invalid)?
                {
                    core_db::accounting::save_link(
                        &mut tx,
                        self.c.id,
                        "allocation",
                        a.id,
                        None,
                        Some("live"),
                        "imported",
                    )
                    .await
                    .map_err(invalid)?;
                }
                tx.commit().await.map_err(invalid)?;
                self.out.paid_in_books += 1;
            }
        }
        Ok(())
    }

    /// One page of the books' invoices from before the start date, brought
    /// in as past invoices: their own numbers, never sent, what was paid
    /// already counted. Matched to an organisation of the same name.
    async fn import_page(&mut self) -> Result<(), ProviderError> {
        let name = Kind::parse(&self.c.provider).map_or("the books", Kind::name);
        let page_no = self.c.import_page.max(1) as u32;
        self.calls += 1;
        let page = match step(self.b.past_invoices(self.c.start_date, page_no).await)? {
            Step::Done(p) => p,
            Step::Refused(m) => {
                let mut tx = self.tx().await?;
                core_db::accounting::import_failed(&mut tx, self.c.id, &m)
                    .await
                    .map_err(invalid)?;
                tx.commit().await.map_err(invalid)?;
                return Ok(());
            }
        };
        let mut added = 0;
        for p in &page.invoices {
            let mut tx = self.tx().await?;
            if !self.still_mine(&mut tx).await? {
                self.out.lost = true;
                return Ok(());
            }
            if core_db::accounting::by_remote(&mut tx, self.c.id, "invoice", &p.id)
                .await
                .map_err(invalid)?
                .is_some()
            {
                continue;
            }
            // One OnHold has already (sent from here, or imported from a PDF).
            if let Some(existing) = core_db::accounting::invoice_by_reference(&mut tx, &p.number)
                .await
                .map_err(invalid)?
            {
                if core_db::accounting::link(&mut tx, self.c.id, "invoice", existing)
                    .await
                    .map_err(invalid)?
                    .is_none()
                {
                    core_db::accounting::save_link(
                        &mut tx,
                        self.c.id,
                        "invoice",
                        existing,
                        Some(&p.id),
                        Some("issued"),
                        "imported",
                    )
                    .await
                    .map_err(invalid)?;
                }
                tx.commit().await.map_err(invalid)?;
                continue;
            }
            let organisation = core_db::accounting::organisation_named(&mut tx, &p.contact_name)
                .await
                .map_err(invalid)?;
            let lines: Vec<String> = p
                .lines
                .iter()
                .take(10)
                .filter(|l| !l.description.trim().is_empty())
                .map(|l| {
                    format!(
                        "{} — {}",
                        l.description.trim(),
                        core_domain::money::format_minor(l.amount_minor, &p.currency, "en")
                    )
                })
                .collect();
            let notes = if lines.is_empty() {
                format!("Imported from {name}.")
            } else {
                format!("Imported from {name}.\n{}", lines.join("\n"))
            };
            let notes: String = notes.chars().take(5000).collect();
            let historical = super::invoices::HistoricalInvoice {
                reference: p.number.chars().take(60).collect(),
                kind: None,
                payer: super::invoices::Payer {
                    organisation_id: organisation,
                    talent_id: None,
                    contact_id: None,
                    name: organisation
                        .is_none()
                        .then(|| p.contact_name.chars().take(200).collect()),
                },
                booking_id: None,
                issuer_talent_id: None,
                currency: p.currency.clone(),
                issue_date: p.date,
                due_date: p.due.filter(|d| *d >= p.date),
                line_items: None,
                total_minor: Some(p.total_minor),
                paid_minor: Some((p.total_minor - p.due_minor).clamp(0, p.total_minor)),
                paid: false,
                document_id: None,
                notes: Some(notes),
                tags: Vec::new(),
            };
            match super::invoices::insert_historical(&mut tx, self.ctx, historical).await {
                Ok(id) => {
                    core_db::accounting::save_link(
                        &mut tx,
                        self.c.id,
                        "invoice",
                        id,
                        Some(&p.id),
                        Some("issued"),
                        "imported",
                    )
                    .await
                    .map_err(invalid)?;
                    if let (Some(org), Some(contact)) = (organisation, &p.contact_id)
                        && core_db::accounting::link(&mut tx, self.c.id, "contact", org)
                            .await
                            .map_err(invalid)?
                            .is_none()
                    {
                        core_db::accounting::save_link(
                            &mut tx,
                            self.c.id,
                            "contact",
                            org,
                            Some(contact),
                            Some("live"),
                            "synced",
                        )
                        .await
                        .map_err(invalid)?;
                    }
                    tx.commit().await.map_err(invalid)?;
                    added += 1;
                }
                Err(e) => {
                    // Not one OnHold can keep (a currency it does not know,
                    // say): left out, the rest go on.
                    tracing::info!(remote_id = %p.id, error = %e, "a past invoice was not imported");
                }
            }
        }
        let next = page.more.then_some(page_no as i32 + 1);
        let mut tx = self.tx().await?;
        core_db::accounting::import_progress(&mut tx, self.c.id, self.token, next, added)
            .await
            .map_err(invalid)?;
        tx.commit().await.map_err(invalid)?;
        self.out.imported += added as usize;
        if page.more {
            self.out.more = true;
        }
        Ok(())
    }
}

/// Records a failed sync and when to try again: soon at first, then less
/// often (5 minutes doubling to 6 hours). A grant that no longer works
/// waits for connecting again.
async fn failed(
    state: &AppState,
    ctx: &Ctx,
    c: &Connection,
    token: Uuid,
    err: ProviderError,
) -> ApiResult<()> {
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
    tracing::warn!(connection_id = %c.id, provider = %c.provider, error = %message, "syncing the books failed");
    let mut tx = begin(state, ctx).await?;
    core_db::accounting::failed(&mut tx, c.id, token, status, &message, next).await?;
    if status != c.status {
        record(
            &mut tx,
            ctx,
            Change {
                topic: "accounting.health_changed",
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

    #[test]
    fn create_keys_are_stable_until_refused() {
        let (c, l) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let key = create_key(c, "invoice", l, 0);
        assert_eq!(key, create_key(c, "invoice", l, 0));
        assert_ne!(key, create_key(c, "invoice", l, 1));
        assert_ne!(key, create_key(c, "allocation", l, 0));
        // Intuit's request ids are at most 50 characters.
        assert_eq!(key.len(), 32);
    }
}
