//! Finance set-up: who the workspace is on its invoices, how they are
//! numbered and paid, the tax codes it charges, and exchange rates into
//! its base currency (the workspace's default currency).

use chrono::{NaiveDate, Utc};
use core_db::Tx;
use core_domain::finance::{Rate, TaxKind, check_number_format, default_tax_codes};
use core_domain::{Ctx, Permission};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::crm::Check;
use super::{Change, begin, check_version, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

pub const INVOICING_MODELS: &[&str] = &["agency", "talent", "split"];

/// Today in the workspace's timezone.
pub(crate) async fn today(tx: &mut Tx, ctx: &Ctx) -> ApiResult<NaiveDate> {
    let tz: chrono_tz::Tz = core_db::workspaces::get(tx, ctx.workspace_id)
        .await?
        .and_then(|w| w.default_timezone.parse().ok())
        .unwrap_or(chrono_tz::UTC);
    Ok(Utc::now().with_timezone(&tz).date_naive())
}

/// The workspace's base currency, which reports are in.
pub(crate) async fn base_currency(tx: &mut Tx, ctx: &Ctx) -> ApiResult<String> {
    Ok(core_db::workspaces::get(tx, ctx.workspace_id)
        .await?
        .map_or_else(|| "GBP".into(), |w| w.default_currency.trim().to_owned()))
}

// Settings ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BankAccount {
    /// The currency invoices in it are paid in.
    pub currency: String,
    pub label: Option<String>,
    /// Account name, number, sort code, IBAN, SWIFT: as it should print.
    pub details: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FinanceSettings {
    /// The name invoices are issued in; default the workspace's name.
    pub legal_name: Option<String>,
    /// {line1, line2, city, region, postcode, country}
    #[schema(value_type = Object)]
    pub address: Value,
    /// VAT, GST or sales tax number.
    pub tax_number: Option<String>,
    pub company_number: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    /// e.g. `INV-{YYYY}-{0000}`: {YYYY} {YY} {MM}, {0000} (padded) or {N}.
    pub invoice_format: String,
    pub credit_note_format: String,
    pub payment_terms_days: i32,
    /// Who invoices the buyer: `agency` (the agency invoices and pays the
    /// talent out), `talent` (the talent invoices; the agency invoices
    /// them for commission) or `split` (deposit by the agency, balance by
    /// the talent).
    pub invoicing_model: String,
    pub bank_accounts: Vec<BankAccount>,
    pub late_fee_text: Option<String>,
    pub invoice_footer: Option<String>,
    /// Reports and conversions are in this currency.
    pub base_currency: String,
    pub version: i64,
}

fn settings_view(s: core_db::finance::Settings, base: String) -> FinanceSettings {
    FinanceSettings {
        legal_name: s.legal_name,
        address: s.address,
        tax_number: s.tax_number,
        company_number: s.company_number,
        email: s.email,
        phone: s.phone,
        invoice_format: s.invoice_format,
        credit_note_format: s.credit_note_format,
        payment_terms_days: s.payment_terms_days,
        invoicing_model: s.invoicing_model,
        bank_accounts: serde_json::from_value(s.bank_accounts).unwrap_or_default(),
        late_fee_text: s.late_fee_text,
        invoice_footer: s.invoice_footer,
        base_currency: base,
        version: s.version,
    }
}

pub(crate) async fn load_settings(tx: &mut Tx, ctx: &Ctx) -> ApiResult<FinanceSettings> {
    let s = core_db::finance::settings(tx).await?;
    let base = base_currency(tx, ctx).await?;
    Ok(settings_view(s, base))
}

pub async fn get_settings(state: &AppState, ctx: &Ctx) -> ApiResult<FinanceSettings> {
    ctx.authorize(Permission::InvoiceRead)?;
    let mut tx = begin(state, ctx).await?;
    let s = load_settings(&mut tx, ctx).await?;
    tx.commit().await?;
    Ok(s)
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct FinanceSettingsInput {
    pub legal_name: Option<String>,
    #[schema(value_type = Option<Object>)]
    pub address: Option<Value>,
    pub tax_number: Option<String>,
    pub company_number: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub invoice_format: Option<String>,
    pub credit_note_format: Option<String>,
    pub payment_terms_days: Option<i32>,
    pub invoicing_model: Option<String>,
    pub bank_accounts: Option<Vec<BankAccount>>,
    pub late_fee_text: Option<String>,
    pub invoice_footer: Option<String>,
}

const ADDRESS_KEYS: &[&str] = &["line1", "line2", "city", "region", "postcode", "country"];

fn check_address(check: &mut Check, field: &str, value: &Value) -> Value {
    let Some(obj) = value.as_object() else {
        check.issue(field, "must be an object");
        return json!({});
    };
    let mut out = serde_json::Map::new();
    for (k, v) in obj {
        if !ADDRESS_KEYS.contains(&k.as_str()) {
            check.issue(format!("{field}.{k}"), format!("use only: {}", ADDRESS_KEYS.join(", ")));
            continue;
        }
        match v {
            Value::Null => {}
            Value::String(s) if s.chars().count() <= 200 => {
                if !s.trim().is_empty() {
                    out.insert(k.clone(), json!(s.trim()));
                }
            }
            _ => check.issue(format!("{field}.{k}"), "must be text of at most 200 characters"),
        }
    }
    Value::Object(out)
}

/// Replaces the settings: fields left out are cleared, except the
/// numbering, terms and model, which keep their values.
pub async fn update_settings(
    state: &AppState,
    ctx: &Ctx,
    input: FinanceSettingsInput,
    if_match: Option<i64>,
) -> ApiResult<FinanceSettings> {
    ctx.authorize(Permission::FinanceConfig)?;
    let mut tx = begin(state, ctx).await?;
    let version = core_db::finance::lock_settings(&mut tx).await?;
    check_version(version, if_match)?;
    let before = load_settings(&mut tx, ctx).await?;
    let mut check = Check::default();
    let legal_name = check.optional("legal_name", input.legal_name.as_deref(), 200);
    let address = check_address(&mut check, "address", input.address.as_ref().unwrap_or(&json!({})));
    let tax_number = check.optional("tax_number", input.tax_number.as_deref(), 50);
    let company_number = check.optional("company_number", input.company_number.as_deref(), 50);
    let email = check.optional("email", input.email.as_deref(), 320);
    if let Some(e) = &email
        && !core_engines::fields::is_email(e)
    {
        check.issue("email", "is not an email address");
    }
    let phone = check.optional("phone", input.phone.as_deref(), 50);
    let invoice_format = input.invoice_format.unwrap_or(before.invoice_format.clone());
    if let Err(e) = check_number_format(&invoice_format) {
        check.issue("invoice_format", e);
    }
    let credit_note_format = input.credit_note_format.unwrap_or(before.credit_note_format.clone());
    if let Err(e) = check_number_format(&credit_note_format) {
        check.issue("credit_note_format", e);
    }
    let terms = input.payment_terms_days.unwrap_or(before.payment_terms_days);
    if !(0..=365).contains(&terms) {
        check.issue("payment_terms_days", "must be 0 to 365");
    }
    let model = input.invoicing_model.unwrap_or(before.invoicing_model.clone());
    if !INVOICING_MODELS.contains(&model.as_str()) {
        check.issue("invoicing_model", format!("must be one of: {}", INVOICING_MODELS.join(", ")));
    }
    let mut accounts = Vec::new();
    for (i, a) in input.bank_accounts.unwrap_or_default().iter().enumerate() {
        let Some(currency) = check.currency(&format!("bank_accounts[{i}].currency"), Some(&a.currency)) else {
            continue;
        };
        let label = check.optional(&format!("bank_accounts[{i}].label"), a.label.as_deref(), 100);
        let details = check.required(&format!("bank_accounts[{i}].details"), &a.details, 1000);
        accounts.push(BankAccount { currency, label, details });
    }
    if accounts.len() > 20 {
        check.issue("bank_accounts", "give at most 20 accounts");
    }
    let late_fee_text = check.long("late_fee_text", input.late_fee_text.as_deref(), 1000);
    let footer = check.long("invoice_footer", input.invoice_footer.as_deref(), 2000);
    check.finish()?;
    let accounts_json = serde_json::to_value(&accounts).map_err(ApiError::internal)?;
    core_db::finance::update_settings(
        &mut tx,
        core_db::finance::SettingsValues {
            legal_name: legal_name.as_deref(),
            address: &address,
            tax_number: tax_number.as_deref(),
            company_number: company_number.as_deref(),
            email: email.as_deref(),
            phone: phone.as_deref(),
            invoice_format: &invoice_format,
            credit_note_format: &credit_note_format,
            payment_terms_days: terms,
            invoicing_model: &model,
            bank_accounts: &accounts_json,
            late_fee_text: late_fee_text.as_deref(),
            invoice_footer: footer.as_deref(),
        },
        ctx.user_id(),
    )
    .await?;
    let after = load_settings(&mut tx, ctx).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "finance.settings_updated",
            resource: "finance_settings",
            object_id: None,
            before: serde_json::to_value(&before).ok(),
            after: serde_json::to_value(&after).ok(),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

/// Who an invoice is from, as printed: the workspace (from its finance
/// settings), or a talent invoicing in their own name (their billing
/// details: legal name, address, tax number).
pub(crate) async fn issuer_json(tx: &mut Tx, ctx: &Ctx, talent_id: Option<Uuid>) -> ApiResult<Value> {
    if let Some(t) = talent_id {
        let talent = core_db::talent::get(tx, t).await?.ok_or(ApiError::NotFound)?;
        let billing = sqlx::query_scalar!(
            r#"SELECT billing FROM talent WHERE workspace_id = app_workspace_id() AND id = $1"#,
            t
        )
        .fetch_one(&mut **tx)
        .await?;
        let get = |k: &str| billing.get(k).and_then(Value::as_str).map(str::to_owned);
        return Ok(json!({
            "kind": "talent",
            "talent_id": t,
            "name": get("legal_name").or(talent.legal_name).unwrap_or(talent.name),
            "address": billing.get("address").cloned().unwrap_or(json!({})),
            "tax_number": get("tax_number"),
            "company_number": get("company_number"),
            "email": get("email"),
            "phone": get("phone"),
        }));
    }
    let s = core_db::finance::settings(tx).await?;
    let w = core_db::workspaces::get(tx, ctx.workspace_id).await?.ok_or(ApiError::NotFound)?;
    Ok(json!({
        "kind": "workspace",
        "name": s.legal_name.unwrap_or(w.name),
        "address": s.address,
        "tax_number": s.tax_number,
        "company_number": s.company_number,
        "email": s.email,
        "phone": s.phone,
    }))
}

/// A talent's bank details for invoices they issue, from their billing.
pub(crate) async fn talent_bank(tx: &mut Tx, talent_id: Uuid, currency: &str) -> ApiResult<Option<Value>> {
    let billing = sqlx::query_scalar!(
        r#"SELECT billing FROM talent WHERE workspace_id = app_workspace_id() AND id = $1"#,
        talent_id
    )
    .fetch_optional(&mut **tx)
    .await?
    .unwrap_or(json!({}));
    let accounts: Vec<BankAccount> = billing
        .get("bank_accounts")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    Ok(accounts
        .into_iter()
        .find(|a| a.currency == currency)
        .map(|a| json!({ "label": a.label, "details": a.details })))
}

// Tax codes --------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TaxCodeView {
    pub id: Uuid,
    pub name: String,
    /// Hundredths of a percent: 2000 is 20%.
    pub rate_bps: i32,
    pub kind: TaxKind,
    /// Printed on invoices that use it.
    pub note: Option<String>,
    pub is_default: bool,
    pub position: i32,
    pub archived: bool,
    pub version: i64,
}

impl From<core_db::finance::TaxCode> for TaxCodeView {
    fn from(t: core_db::finance::TaxCode) -> Self {
        Self {
            id: t.id,
            name: t.name,
            rate_bps: t.rate_bps,
            kind: TaxKind::parse(&t.kind).unwrap_or(TaxKind::Standard),
            note: t.note,
            is_default: t.is_default,
            position: t.position,
            archived: t.archived_at.is_some(),
            version: t.version,
        }
    }
}

/// Gives a workspace the usual codes for its country the first time it
/// needs any.
pub(crate) async fn ensure_tax_codes(tx: &mut Tx, ctx: &Ctx) -> ApiResult<()> {
    if core_db::finance::has_tax_codes(tx).await? {
        return Ok(());
    }
    let country = core_db::workspaces::get(tx, ctx.workspace_id)
        .await?
        .map(|w| w.country.trim().to_owned())
        .unwrap_or_default();
    for (i, d) in default_tax_codes(&country).into_iter().enumerate() {
        core_db::finance::insert_tax_code(
            tx,
            core_domain::ids::new_id(),
            core_db::finance::TaxCodeValues {
                name: d.name,
                rate_bps: d.rate_bps,
                kind: d.kind.as_str(),
                note: d.note,
                is_default: d.is_default,
                position: i as i32,
            },
            None,
        )
        .await?;
    }
    Ok(())
}

pub async fn tax_codes(state: &AppState, ctx: &Ctx, include_archived: bool) -> ApiResult<Vec<TaxCodeView>> {
    ctx.authorize(Permission::InvoiceRead)?;
    let mut tx = begin(state, ctx).await?;
    ensure_tax_codes(&mut tx, ctx).await?;
    let rows = core_db::finance::tax_codes(&mut tx, include_archived).await?;
    tx.commit().await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct TaxCodeInput {
    pub name: String,
    pub rate_bps: i32,
    pub kind: Option<TaxKind>,
    pub note: Option<String>,
    #[serde(default)]
    pub is_default: bool,
    pub position: Option<i32>,
    /// Updates only: archive (true) or bring back (false).
    pub archived: Option<bool>,
}

fn check_tax(check: &mut Check, input: &TaxCodeInput) -> (String, TaxKind, Option<String>) {
    let name = check.required("name", &input.name, 100);
    let kind = input.kind.unwrap_or(TaxKind::Standard);
    if !(0..=10_000).contains(&input.rate_bps) {
        check.issue("rate_bps", "must be 0 to 10000 (0% to 100%)");
    }
    if kind.charges_nothing() && input.rate_bps != 0 {
        check.issue("rate_bps", "must be 0 for this kind of code");
    }
    let note = check.long("note", input.note.as_deref(), 500);
    (name, kind, note)
}

pub async fn create_tax_code(state: &AppState, ctx: &Ctx, input: TaxCodeInput) -> ApiResult<TaxCodeView> {
    ctx.authorize(Permission::FinanceConfig)?;
    let mut check = Check::default();
    let (name, kind, note) = check_tax(&mut check, &input);
    check.finish()?;
    let mut tx = begin(state, ctx).await?;
    ensure_tax_codes(&mut tx, ctx).await?;
    let id = core_domain::ids::new_id();
    core_db::finance::insert_tax_code(
        &mut tx,
        id,
        core_db::finance::TaxCodeValues {
            name: &name,
            rate_bps: input.rate_bps,
            kind: kind.as_str(),
            note: note.as_deref(),
            is_default: input.is_default,
            position: input.position.unwrap_or(100),
        },
        ctx.user_id(),
    )
    .await?;
    let view: TaxCodeView = core_db::finance::tax_code(&mut tx, id).await?.ok_or(ApiError::NotFound)?.into();
    record(
        &mut tx,
        ctx,
        Change {
            topic: "tax_code.created",
            resource: "tax_code",
            object_id: Some(id),
            before: None,
            after: serde_json::to_value(&view).ok(),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(view)
}

/// Changes a code. Invoices already made keep the rate they were made
/// with: each line records its own.
pub async fn update_tax_code(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    input: TaxCodeInput,
    if_match: Option<i64>,
) -> ApiResult<TaxCodeView> {
    ctx.authorize(Permission::FinanceConfig)?;
    let mut check = Check::default();
    let (name, kind, note) = check_tax(&mut check, &input);
    check.finish()?;
    let mut tx = begin(state, ctx).await?;
    let before: TaxCodeView = core_db::finance::tax_code(&mut tx, id).await?.ok_or(ApiError::NotFound)?.into();
    check_version(before.version, if_match)?;
    core_db::finance::update_tax_code(
        &mut tx,
        id,
        core_db::finance::TaxCodeValues {
            name: &name,
            rate_bps: input.rate_bps,
            kind: kind.as_str(),
            note: note.as_deref(),
            is_default: input.is_default,
            position: input.position.unwrap_or(before.position),
        },
        input.archived.unwrap_or(before.archived),
        ctx.user_id(),
    )
    .await?;
    let after: TaxCodeView = core_db::finance::tax_code(&mut tx, id).await?.ok_or(ApiError::NotFound)?.into();
    record(
        &mut tx,
        ctx,
        Change {
            topic: "tax_code.updated",
            resource: "tax_code",
            object_id: Some(id),
            before: serde_json::to_value(&before).ok(),
            after: serde_json::to_value(&after).ok(),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(after)
}

// Exchange rates ---------------------------------------------------------------------------

/// A conversion rate with where it came from.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FxQuote {
    pub from: String,
    pub to: String,
    /// Units of `to` per unit of `from`.
    pub rate: Rate,
    /// The day the rate is for.
    pub date: NaiveDate,
    /// same, ecb, booking or manual.
    pub source: String,
}

/// One euro buys this many of `currency` on the latest day at or before `on`.
async fn per_eur(tx: &mut Tx, currency: &str, on: NaiveDate) -> ApiResult<Option<(NaiveDate, Rate)>> {
    if currency == "EUR" {
        return Ok(Some((on, Rate::ONE)));
    }
    Ok(core_db::finance::rate_per_eur(tx, currency, on)
        .await?
        .and_then(|(d, r)| r.parse().ok().map(|r| (d, r))))
}

/// The reference rate from one currency to another on a day, from the
/// ECB's daily rates (crossed through the euro). `None` when there is no
/// rate within two weeks before that day.
pub(crate) async fn reference_rate(
    tx: &mut Tx,
    from: &str,
    to: &str,
    on: NaiveDate,
) -> ApiResult<Option<FxQuote>> {
    if from == to {
        return Ok(Some(FxQuote {
            from: from.into(),
            to: to.into(),
            rate: Rate::ONE,
            date: on,
            source: "same".into(),
        }));
    }
    let (Some((d1, a)), Some((d2, b))) = (per_eur(tx, from, on).await?, per_eur(tx, to, on).await?) else {
        return Ok(None);
    };
    Ok(Rate::cross(a, b).map(|rate| FxQuote {
        from: from.into(),
        to: to.into(),
        rate,
        date: d1.min(d2),
        source: "ecb".into(),
    }))
}

pub async fn rate(state: &AppState, ctx: &Ctx, from: &str, to: Option<&str>, on: Option<NaiveDate>) -> ApiResult<FxQuote> {
    ctx.authorize(Permission::InvoiceRead)?;
    let mut check = Check::default();
    let from = check.currency("from", Some(from));
    let to_checked = to.and_then(|t| check.currency("to", Some(t)));
    check.finish()?;
    let mut tx = begin(state, ctx).await?;
    let to = match to_checked {
        Some(t) => t,
        None => base_currency(&mut tx, ctx).await?,
    };
    let on = match on {
        Some(d) => d,
        None => today(&mut tx, ctx).await?,
    };
    let from = from.unwrap_or_default();
    let q = reference_rate(&mut tx, &from, &to, on).await?.ok_or_else(|| {
        ApiError::unprocessable(
            "no_rate",
            format!("there is no reference rate from {from} to {to} near {on}; give the rate yourself"),
        )
    })?;
    tx.commit().await?;
    Ok(q)
}
