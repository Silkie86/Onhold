//! Sending from a workspace's own domain. An owner or admin adds the
//! domain; the mail provider gives the DNS records it needs (DKIM, and SPF
//! with a bounce MX), which the owner adds at their DNS provider, by hand
//! or in one step through Domain Connect when their provider offers it.
//! The worker asks the provider how the records are doing, often at first
//! and then daily; once they are found, the workspace's email (system
//! emails, rule emails and replies sent without a mailbox) goes out from an
//! address at the domain. A domain never verified is removed after
//! [`UNVERIFIED_DAYS`], so nobody can hold a domain they do not own.

use chrono::{DateTime, Duration, Utc};
use core_db::sender_domains::{Checked, NewSenderDomain, SenderDomain};
use core_domain::{Ctx, Permission, WorkspaceId};
use core_integrations::dns::DnsProvider;
use core_integrations::domains::{DnsRecord, ProviderDomain};
use core_integrations::provider::ProviderError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::integrations::provider_error;
use super::{Change, begin, record};
use crate::AppState;
use crate::config::SenderDomainsConfig;
use crate::error::{ApiError, ApiResult};

pub const ENTITY: &str = "sender_domain";
/// Asks the provider about one domain (`domain_id`, `at`: when it was due).
pub const JOB_CHECK: &str = "domain.check";
/// Removes a domain from the provider's account (`provider_id`).
pub const JOB_FORGET: &str = "domain.forget";

/// How long a domain may wait for its records before it is removed.
pub const UNVERIFIED_DAYS: i64 = 14;
const MAX_DOMAINS: usize = 5;
/// The DMARC record suggested when the domain has none: reports nothing,
/// changes nothing, but tells receivers the domain is looked after.
const DMARC_SUGGESTION: &str = "v=DMARC1; p=none;";
/// Mailbox providers whose domains belong to them, not to their users.
const PUBLIC_MAIL: &[&str] = &[
    "gmail.com",
    "googlemail.com",
    "outlook.com",
    "hotmail.com",
    "hotmail.co.uk",
    "live.com",
    "msn.com",
    "yahoo.com",
    "yahoo.co.uk",
    "icloud.com",
    "me.com",
    "aol.com",
    "proton.me",
    "protonmail.com",
    "gmx.com",
];

/// The record names and values OnHold's Domain Connect template has; a
/// domain whose records differ is set up by hand.
const TEMPLATE_DKIM_HOST: &str = "resend._domainkey";
const TEMPLATE_SEND_HOST: &str = "send";
const TEMPLATE_SPF: &str = "v=spf1 include:amazonses.com ~all";

/// One DNS record to add.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RecordView {
    /// What it is for: `DKIM`, `SPF` or `DMARC`.
    pub purpose: String,
    /// `TXT`, `MX` or `CNAME`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The name as most DNS providers want it: relative to the zone, e.g.
    /// `resend._domainkey` (or `resend._domainkey.mail` for a subdomain).
    pub host: String,
    /// The full name.
    pub name: String,
    pub value: String,
    pub priority: Option<i32>,
    /// The provider has found it.
    pub found: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SenderDomainView {
    pub id: Uuid,
    pub domain: String,
    /// `pending` (waiting for its records), `verified` or `failed`.
    pub status: String,
    pub records: Vec<RecordView>,
    /// The suggested DMARC record, when the domain has no DMARC policy.
    pub dmarc_record: Option<RecordView>,
    /// The address email is sent from once verified and in use.
    pub from_address: String,
    pub local_part: String,
    /// Chosen to send from.
    pub in_use: bool,
    /// Email is going out from it now.
    pub sending: bool,
    /// The domain's DNS provider, when it can add the records itself.
    pub automatic_setup: Option<String>,
    pub checked_at: Option<DateTime<Utc>>,
    pub verified_at: Option<DateTime<Utc>>,
    /// When it is removed if its records are still not found.
    pub remove_after: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

fn records_of(d: &SenderDomain) -> Vec<DnsRecord> {
    serde_json::from_value(d.records.clone()).unwrap_or_default()
}

fn dns_provider_of(d: &SenderDomain) -> Option<DnsProvider> {
    d.dns_provider
        .clone()
        .and_then(|v| serde_json::from_value(v).ok())
}

/// The record's name relative to the zone the domain sits in, which is
/// what DNS providers' forms ask for. Without a known zone, relative to
/// the domain.
fn view_record(r: &DnsRecord, domain: &str, host_label: &str) -> RecordView {
    let host = if host_label.is_empty() {
        r.name.clone()
    } else {
        format!("{}.{host_label}", r.name)
    };
    RecordView {
        purpose: r.record.clone(),
        kind: r.kind.clone(),
        host,
        name: format!("{}.{domain}", r.name),
        value: r.value.clone(),
        priority: r.priority,
        found: r.status == "verified",
    }
}

impl From<&SenderDomain> for SenderDomainView {
    fn from(d: &SenderDomain) -> Self {
        let dns = dns_provider_of(d);
        let label = dns.as_ref().map(|p| p.host.clone()).unwrap_or_default();
        let dmarc = DnsRecord {
            record: "DMARC".into(),
            name: "_dmarc".into(),
            kind: "TXT".into(),
            value: DMARC_SUGGESTION.into(),
            priority: None,
            status: String::new(),
        };
        Self {
            id: d.id,
            domain: d.domain.clone(),
            status: d.status.clone(),
            records: records_of(d)
                .iter()
                .map(|r| view_record(r, &d.domain, &label))
                .collect(),
            dmarc_record: (!d.dmarc).then(|| view_record(&dmarc, &d.domain, &label)),
            from_address: format!("{}@{}", d.local_part, d.domain),
            local_part: d.local_part.clone(),
            in_use: d.in_use,
            sending: d.in_use && d.status == "verified",
            automatic_setup: (d.status != "verified")
                .then_some(dns)
                .flatten()
                .map(|p| p.name),
            checked_at: d.checked_at,
            verified_at: d.verified_at,
            remove_after: d
                .verified_at
                .is_none()
                .then(|| d.created_at + Duration::days(UNVERIFIED_DAYS)),
            created_at: d.created_at,
            version: d.version,
        }
    }
}

/// What the settings page shows.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SenderDomainSetup {
    /// This server can verify domains (its mail provider is set up for it).
    pub available: bool,
    pub domains: Vec<SenderDomainView>,
}

fn audit_json(d: &SenderDomain) -> Value {
    json!({
        "domain": d.domain,
        "status": d.status,
        "local_part": d.local_part,
        "in_use": d.in_use,
    })
}

fn configured(state: &AppState) -> ApiResult<&SenderDomainsConfig> {
    state.config.sender_domains.as_ref().ok_or_else(|| {
        ApiError::unavailable(
            "sender_domains_unavailable",
            "this server is not set up to send from your own domain",
        )
    })
}

pub async fn setup(state: &AppState, ctx: &Ctx) -> ApiResult<SenderDomainSetup> {
    ctx.authorize(Permission::IntegrationManage)?;
    let mut tx = begin(state, ctx).await?;
    let domains = core_db::sender_domains::list(&mut tx).await?;
    tx.commit().await?;
    Ok(SenderDomainSetup {
        available: state.config.sender_domains.is_some(),
        domains: domains.iter().map(SenderDomainView::from).collect(),
    })
}

/// A domain name as typed, made canonical: lower case, IDNs as punycode,
/// no scheme, path, trailing dot or `www.`.
pub(crate) fn clean_domain(raw: &str) -> ApiResult<String> {
    let bad = || ApiError::invalid("domain", "enter a domain name, such as acme.com");
    let trimmed = raw.trim();
    let trimmed = trimmed
        .split_once("://")
        .map_or(trimmed, |(_, rest)| rest)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let trimmed = trimmed
        .rsplit_once('@')
        .map_or(trimmed, |(_, d)| d)
        .trim_end_matches('.');
    let host = url::Host::parse(trimmed).map_err(|_| bad())?;
    let url::Host::Domain(d) = host else {
        return Err(bad());
    };
    let d = d.strip_prefix("www.").unwrap_or(&d).to_owned();
    let labels: Vec<&str> = d.split('.').collect();
    let label_ok = |l: &&str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    let tld_ok = labels
        .last()
        .is_some_and(|t| t.starts_with("xn--") || t.chars().all(|c| c.is_ascii_alphabetic()));
    if labels.len() < 2 || d.len() > 253 || !labels.iter().all(label_ok) || !tld_ok {
        return Err(bad());
    }
    Ok(d)
}

fn clean_local_part(raw: &str) -> ApiResult<String> {
    let v = raw.trim().to_ascii_lowercase();
    let ok = (1..=64).contains(&v.len())
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
        && !v.starts_with('.')
        && !v.ends_with('.')
        && !v.contains("..");
    if !ok {
        return Err(ApiError::invalid(
            "local_part",
            "use letters, digits, dots, dashes or underscores, such as hello or bookings",
        ));
    }
    Ok(v)
}

/// Whether `domain` is the platform's own, or under or above it.
fn is_platform(state: &AppState, domain: &str) -> bool {
    let Some(own) = state.config.public_base_url.host_str() else {
        return false;
    };
    let own = own.trim_start_matches("www.");
    let under = |a: &str, b: &str| a == b || a.ends_with(&format!(".{b}"));
    under(domain, own) || under(own, domain)
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AddSenderDomain {
    /// The domain to send from, e.g. `acme.com` or `mail.acme.com`.
    pub domain: String,
    /// The part before the @; `hello` if left out.
    pub local_part: Option<String>,
}

/// Adds a domain at the provider and keeps the records it needs.
pub async fn add(state: &AppState, ctx: &Ctx, req: AddSenderDomain) -> ApiResult<SenderDomainView> {
    ctx.authorize(Permission::IntegrationManage)?;
    let cfg = configured(state)?;
    let domain = clean_domain(&req.domain)?;
    let local_part = clean_local_part(req.local_part.as_deref().unwrap_or("hello"))?;
    if PUBLIC_MAIL.contains(&domain.as_str()) {
        return Err(ApiError::invalid(
            "domain",
            "that domain belongs to a mail service; use a domain you own",
        ));
    }
    if is_platform(state, &domain) {
        return Err(ApiError::invalid("domain", "use a domain you own"));
    }
    let taken = |_| {
        ApiError::conflict(
            "domain_taken",
            "that domain is already set up in OnHold; if it is yours and you did not add it, \
             it is freed after two weeks unless its records are added",
        )
    };
    let mut tx = begin(state, ctx).await?;
    let existing = core_db::sender_domains::list(&mut tx).await?;
    if existing.len() >= MAX_DOMAINS {
        return Err(ApiError::conflict(
            "too_many_domains",
            format!("a workspace can have up to {MAX_DOMAINS} sending domains; remove one first"),
        ));
    }
    if core_db::sender_domains::taken(&mut tx, &domain).await? {
        return Err(taken(()));
    }
    tx.commit().await?;

    let created = cfg.resend.create(&domain).await.map_err(|e| match e {
        // Held at the provider by a domain removed here whose removal
        // there has not gone through yet.
        ProviderError::Refused {
            status: 403 | 409 | 422,
            ..
        } => taken(()),
        e => provider_error("The mail provider", e),
    })?;
    let id = Uuid::now_v7();
    let now = Utc::now();
    let records = serde_json::to_value(&created.records).map_err(ApiError::internal)?;
    let saved = async {
        let mut tx = begin(state, ctx).await?;
        let insert = core_db::sender_domains::insert(
            &mut tx,
            NewSenderDomain {
                id,
                domain: &domain,
                provider_id: &created.id,
                records: &records,
                local_part: &local_part,
                // Looked at straight away by the job below, which sets
                // the next time.
                next_check_at: None,
            },
        )
        .await;
        if let Err(e) = insert {
            return Err(if e.is_unique_violation(None) {
                taken(())
            } else {
                e.into()
            });
        }
        queue_check(&mut tx, ctx.workspace_id, id, now).await?;
        let saved = core_db::sender_domains::get(&mut tx, id)
            .await?
            .ok_or(ApiError::NotFound)?;
        record(
            &mut tx,
            ctx,
            Change {
                topic: "sender_domain.added",
                resource: ENTITY,
                object_id: Some(id),
                before: None,
                after: Some(audit_json(&saved)),
                event_extra: None,
            },
        )
        .await?;
        tx.commit().await?;
        Ok::<_, ApiError>(saved)
    }
    .await;
    match saved {
        Ok(saved) => Ok(SenderDomainView::from(&saved)),
        Err(e) => {
            // Not kept here, so not kept at the provider either.
            if let Err(err) = cfg.resend.delete(&created.id).await {
                tracing::warn!(domain = %domain, error = %err, "a sender domain could not be removed from the provider");
            }
            Err(e)
        }
    }
}

async fn queue_check(
    tx: &mut core_db::Tx,
    ws: WorkspaceId,
    id: Uuid,
    at: DateTime<Utc>,
) -> ApiResult<()> {
    let payload = json!({ "domain_id": id, "at": at });
    core_db::jobs::enqueue(
        tx,
        core_db::jobs::NewJob::new(Some(ws), JOB_CHECK, &payload).dedupe(&check_key(id, at)),
    )
    .await?;
    Ok(())
}

/// One check per domain and due time.
pub fn check_key(id: Uuid, at: DateTime<Utc>) -> String {
    format!("{id}@{}", at.to_rfc3339())
}

async fn queue_forget(tx: &mut core_db::Tx, ws: WorkspaceId, provider_id: &str) -> ApiResult<()> {
    let payload = json!({ "provider_id": provider_id });
    core_db::jobs::enqueue(
        tx,
        core_db::jobs::NewJob::new(Some(ws), JOB_FORGET, &payload).dedupe(provider_id),
    )
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct SenderDomainPatch {
    /// The part before the @ of the sending address.
    pub local_part: Option<String>,
    /// Send the workspace's email from this domain (once verified). Turning
    /// it on turns any other domain off.
    pub in_use: Option<bool>,
}

pub async fn update(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    patch: SenderDomainPatch,
    if_match: Option<i64>,
) -> ApiResult<SenderDomainView> {
    ctx.authorize(Permission::IntegrationManage)?;
    let mut tx = begin(state, ctx).await?;
    let before = core_db::sender_domains::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    super::check_version(before.version, if_match)?;
    let local_part = match &patch.local_part {
        Some(v) => clean_local_part(v)?,
        None => before.local_part.clone(),
    };
    let in_use = patch.in_use.unwrap_or(before.in_use);
    core_db::sender_domains::set_preferences(&mut tx, id, &local_part, in_use).await?;
    let after = core_db::sender_domains::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "sender_domain.updated",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(audit_json(&before)),
            after: Some(audit_json(&after)),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(SenderDomainView::from(&after))
}

/// Asks the provider to look for the records now; the outcome shows
/// within a minute.
pub async fn check_now(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<SenderDomainView> {
    ctx.authorize(Permission::IntegrationManage)?;
    let mut tx = begin(state, ctx).await?;
    let d = core_db::sender_domains::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    let cfg = configured(state)?;
    cfg.resend
        .verify(&d.provider_id)
        .await
        .map_err(|e| provider_error("The mail provider", e))?;
    // The provider takes a few seconds to look; the worker's sweep then
    // picks it up.
    let at = Utc::now() + Duration::seconds(15);
    let mut tx = begin(state, ctx).await?;
    core_db::sender_domains::set_next_check(&mut tx, id, Some(at)).await?;
    let after = core_db::sender_domains::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(SenderDomainView::from(&after))
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AutomaticSetup {
    /// Send the owner here to approve the records at their DNS provider.
    pub url: String,
}

/// The signed link that adds the records at the domain's DNS provider.
pub async fn automatic_setup(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<AutomaticSetup> {
    ctx.authorize(Permission::IntegrationManage)?;
    let mut tx = begin(state, ctx).await?;
    let d = core_db::sender_domains::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    let cfg = configured(state)?;
    let unavailable = || {
        ApiError::conflict(
            "automatic_setup_unavailable",
            "this domain's DNS provider cannot add the records itself; add them by hand",
        )
    };
    let (Some(dc), Some(provider)) = (&cfg.domain_connect, dns_provider_of(&d)) else {
        return Err(unavailable());
    };
    let values = template_values(&records_of(&d)).ok_or_else(unavailable)?;
    let mut groups = vec!["send"];
    if !d.dmarc {
        groups.push("dmarc");
    }
    let mut back = state.config.public_base_url.clone();
    back.set_path("/settings/email/domain");
    back.set_query(Some(&format!("checked={id}")));
    let pairs: Vec<(&str, &str)> = values.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let url = dc
        .apply_url(&provider, &pairs, &groups, back.as_str(), &id.to_string())
        .map_err(ApiError::internal)?;
    Ok(AutomaticSetup {
        url: url.to_string(),
    })
}

/// The template's values for the provider's records, when they are the
/// ones the template has room for.
fn template_values(records: &[DnsRecord]) -> Option<Vec<(&'static str, String)>> {
    let find = |purpose: &str, kind: &str| {
        records
            .iter()
            .find(|r| r.record == purpose && r.kind == kind)
    };
    let dkim = find("DKIM", "TXT")?;
    let mx = find("SPF", "MX")?;
    let spf = find("SPF", "TXT")?;
    let fits = dkim.name == TEMPLATE_DKIM_HOST
        && mx.name == TEMPLATE_SEND_HOST
        && spf.name == TEMPLATE_SEND_HOST
        && spf.value == TEMPLATE_SPF
        && records.len() == 3;
    fits.then(|| vec![("dkim", dkim.value.clone()), ("mx", mx.value.clone())])
}

/// Removes a domain. Email goes out from the platform's address again.
pub async fn remove(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::IntegrationManage)?;
    let mut tx = begin(state, ctx).await?;
    let before = core_db::sender_domains::lock(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    core_db::sender_domains::delete(&mut tx, id).await?;
    queue_forget(&mut tx, ctx.workspace_id, &before.provider_id).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "sender_domain.removed",
            resource: ENTITY,
            object_id: Some(id),
            before: Some(audit_json(&before)),
            after: None,
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Removes every domain of a workspace that is closing, so the domains are
/// free for whoever owns them next.
pub(crate) async fn remove_all(tx: &mut core_db::Tx, ctx: &Ctx) -> ApiResult<()> {
    for (id, provider_id) in core_db::sender_domains::delete_all(tx).await? {
        queue_forget(tx, ctx.workspace_id, &provider_id).await?;
        record(
            tx,
            ctx,
            Change {
                topic: "sender_domain.removed",
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

// The worker's side ------------------------------------------------------------------------

/// What a check did, for the job log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    Gone,
    Checked { status: String },
    Expired,
}

impl std::fmt::Display for CheckOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gone => write!(f, "the domain was removed"),
            Self::Checked { status } => write!(f, "the domain is {status}"),
            Self::Expired => write!(f, "never verified, so removed"),
        }
    }
}

/// When to look again: soon while the owner is likely adding the records,
/// then less often; daily once verified.
fn next_check(status: &str, created_at: DateTime<Utc>, now: DateTime<Utc>) -> DateTime<Utc> {
    if status != "pending" {
        return now + Duration::hours(24);
    }
    let age = now - created_at;
    now + (age / 4).clamp(Duration::minutes(1), Duration::hours(6))
}

fn status_from(p: &ProviderDomain) -> &'static str {
    if p.verified() {
        "verified"
    } else if p.failed() || p.status == "partially_failed" {
        "failed"
    } else {
        "pending"
    }
}

/// Asks the provider about a domain and keeps what it says, with whether a
/// DMARC policy covers it and, until one is found, its DNS provider's
/// Domain Connect. A domain never verified within [`UNVERIFIED_DAYS`] is
/// removed.
pub async fn check(state: &AppState, ws: WorkspaceId, id: Uuid) -> ApiResult<CheckOutcome> {
    let ctx = Ctx::system(ws);
    let cfg = configured(state)?;
    let mut tx = begin(state, &ctx).await?;
    let Some(d) = core_db::sender_domains::get(&mut tx, id).await? else {
        return Ok(CheckOutcome::Gone);
    };
    tx.commit().await?;
    let now = Utc::now();

    let found = match cfg.resend.get(&d.provider_id).await {
        Ok(p) => Some(p),
        // The provider no longer has it: it can only be added again.
        Err(ProviderError::Refused { status: 404, .. }) => None,
        Err(e) if e.is_temporary() => return Err(provider_error("The mail provider", e)),
        Err(e) => return Err(ApiError::internal(e)),
    };
    let status = found.as_ref().map_or("failed", status_from);
    if d.verified_at.is_none()
        && status != "verified"
        && now - d.created_at > Duration::days(UNVERIFIED_DAYS)
    {
        let mut tx = begin(state, &ctx).await?;
        if core_db::sender_domains::delete(&mut tx, id).await? {
            queue_forget(&mut tx, ws, &d.provider_id).await?;
            record(
                &mut tx,
                &ctx,
                Change {
                    topic: "sender_domain.removed",
                    resource: ENTITY,
                    object_id: Some(id),
                    before: Some(audit_json(&d)),
                    after: None,
                    event_extra: Some(json!({ "reason": "never_verified" })),
                },
            )
            .await?;
        }
        tx.commit().await?;
        return Ok(CheckOutcome::Expired);
    }

    // DNS answers only add detail: a failed lookup keeps what was known.
    let mut dns_provider = d.dns_provider.clone();
    let mut dmarc = d.dmarc;
    if status != "verified"
        && d.dns_provider.is_none()
        && let Some(dc) = &cfg.domain_connect
    {
        match dc.discover(&cfg.doh, &d.domain).await {
            Ok(p) => dns_provider = p.and_then(|p| serde_json::to_value(p).ok()),
            Err(e) => {
                tracing::info!(domain = %d.domain, error = %e, "looking for a domain's DNS provider failed");
            }
        }
    }
    match cfg.doh.has_dmarc(&d.domain).await {
        Ok(has) => dmarc = has,
        Err(e) => {
            tracing::info!(domain = %d.domain, error = %e, "looking for a DMARC record failed")
        }
    }

    let records = match &found {
        Some(p) => serde_json::to_value(&p.records).map_err(ApiError::internal)?,
        None => d.records.clone(),
    };
    let next = found
        .as_ref()
        .map(|_| next_check(status, d.created_at, now));
    let mut tx = begin(state, &ctx).await?;
    let Some(locked) = core_db::sender_domains::lock(&mut tx, id).await? else {
        return Ok(CheckOutcome::Gone);
    };
    // A check asked for in the meantime keeps its earlier time.
    let next = match (locked.next_check_at, next) {
        (Some(asked), Some(n)) if asked > now && asked < n => Some(asked),
        (_, n) => n,
    };
    core_db::sender_domains::set_checked(
        &mut tx,
        id,
        Checked {
            status,
            records: &records,
            dmarc,
            dns_provider: dns_provider.as_ref(),
            next_check_at: next,
        },
    )
    .await?;
    if status != locked.status {
        // The first domain verified is the one sent from.
        if status == "verified" && !core_db::sender_domains::any_in_use(&mut tx).await? {
            core_db::sender_domains::set_preferences(&mut tx, id, &locked.local_part, true).await?;
        }
        record(
            &mut tx,
            &ctx,
            Change {
                topic: "sender_domain.status_changed",
                resource: ENTITY,
                object_id: Some(id),
                before: Some(json!({ "status": locked.status })),
                after: Some(json!({ "status": status })),
                event_extra: Some(json!({ "domain": d.domain })),
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(CheckOutcome::Checked {
        status: status.to_owned(),
    })
}

/// Removes a domain from the provider's account.
pub async fn forget(state: &AppState, provider_id: &str) -> ApiResult<()> {
    let cfg = configured(state)?;
    cfg.resend
        .delete(provider_id)
        .await
        .map_err(|e| provider_error("The mail provider", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_are_cleaned() {
        for (raw, want) in [
            ("Acme.COM", "acme.com"),
            ("https://www.acme.com/about", "acme.com"),
            ("mail.acme.co.uk.", "mail.acme.co.uk"),
            ("bookings@acme.com", "acme.com"),
            ("bücher.de", "xn--bcher-kva.de"),
        ] {
            assert_eq!(clean_domain(raw).expect(raw), want);
        }
        for bad in [
            "acme",
            "127.0.0.1",
            "-acme.com",
            "acme..com",
            "acme.c0m",
            "",
            "a b.com",
        ] {
            assert!(clean_domain(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn local_parts_are_checked() {
        assert_eq!(clean_local_part(" Bookings ").expect("ok"), "bookings");
        for bad in ["", ".a", "a..b", "a b", "ä"] {
            assert!(clean_local_part(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn checks_slow_down_then_go_daily() {
        let t0 = Utc::now();
        assert_eq!(next_check("pending", t0, t0), t0 + Duration::minutes(1));
        assert_eq!(
            next_check("pending", t0, t0 + Duration::hours(2)),
            t0 + Duration::hours(2) + Duration::minutes(30)
        );
        assert_eq!(
            next_check("pending", t0, t0 + Duration::days(3)),
            t0 + Duration::days(3) + Duration::hours(6)
        );
        assert_eq!(next_check("verified", t0, t0), t0 + Duration::hours(24));
    }

    #[test]
    fn only_template_shaped_records_go_automatic() {
        let rec = |record: &str, name: &str, kind: &str, value: &str| DnsRecord {
            record: record.into(),
            name: name.into(),
            kind: kind.into(),
            value: value.into(),
            priority: None,
            status: String::new(),
        };
        let mut records = vec![
            rec("DKIM", "resend._domainkey", "TXT", "p=MIGf"),
            rec("SPF", "send", "MX", "feedback-smtp.eu-west-1.amazonses.com"),
            rec("SPF", "send", "TXT", TEMPLATE_SPF),
        ];
        let values = template_values(&records).expect("fits");
        assert_eq!(values[0], ("dkim", "p=MIGf".to_owned()));
        records[0].name = "other._domainkey".into();
        assert!(template_values(&records).is_none());
    }
}
