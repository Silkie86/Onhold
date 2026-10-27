//! Reports: revenue by month, payer and talent in the base currency, what
//! is owed and how late, cash in and out, and the enquiry funnel. Each
//! table can be downloaded as CSV.

use chrono::NaiveDate;
use core_domain::{Ctx, Money, Permission};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::begin;
use crate::AppState;
use crate::error::{ApiError, ApiResult};

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct Period {
    /// Default: the first of the month a year ago.
    pub from: Option<NaiveDate>,
    /// Default: today.
    pub to: Option<NaiveDate>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RevenueRow {
    /// `2026-10`, a payer's name or a talent's name.
    pub key: String,
    pub id: Option<Uuid>,
    /// Net of credit notes, tax included (by talent: net of tax).
    pub invoiced: Money,
    pub tax: Money,
    pub invoices: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OutstandingRow {
    pub invoice_id: Uuid,
    pub reference: Option<String>,
    pub payer: String,
    pub booking_id: Option<Uuid>,
    pub outstanding: Money,
    pub base_outstanding: Option<Money>,
    pub issue_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub days_overdue: i64,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Ageing {
    pub current: Money,
    pub days_1_30: Money,
    pub days_31_60: Money,
    pub days_61_90: Money,
    pub over_90: Money,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CashRow {
    pub currency: String,
    pub received_minor: i64,
    pub refunded_minor: i64,
    pub paid_out_minor: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FunnelReport {
    pub enquiries: i64,
    pub quoted: i64,
    pub won: i64,
    pub lost: i64,
    pub open: i64,
    /// Won out of enquiries decided (won or lost), 0 to 1.
    pub win_rate: Option<f64>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FinanceReport {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub base_currency: String,
    pub by_month: Vec<RevenueRow>,
    pub by_payer: Vec<RevenueRow>,
    pub by_talent: Vec<RevenueRow>,
    pub outstanding: Vec<OutstandingRow>,
    pub ageing: Ageing,
    pub cash: Vec<CashRow>,
    pub funnel: FunnelReport,
}

pub async fn finance(state: &AppState, ctx: &Ctx, p: &Period) -> ApiResult<FinanceReport> {
    ctx.authorize(Permission::ReportRead)?;
    let mut tx = begin(state, ctx).await?;
    let today = super::finance::today(&mut tx, ctx).await?;
    let to = p.to.unwrap_or(today);
    let from = p.from.unwrap_or_else(|| {
        use chrono::Datelike;
        NaiveDate::from_ymd_opt(to.year() - 1, to.month(), 1).unwrap_or(to)
    });
    if from > to {
        return Err(ApiError::invalid("from", "must not be after `to`"));
    }
    let base = super::finance::base_currency(&mut tx, ctx).await?;
    let money = |minor: i64| Money {
        minor,
        currency: base.clone(),
    };
    let rows = |r: Vec<core_db::reports::Row>| -> Vec<RevenueRow> {
        r.into_iter()
            .map(|r| RevenueRow {
                key: r.key,
                id: r.id,
                invoiced: money(r.invoiced_minor),
                tax: money(r.tax_minor),
                invoices: r.count,
            })
            .collect()
    };
    let by_month = rows(core_db::reports::revenue_by_month(&mut tx, from, to).await?);
    let by_payer = rows(core_db::reports::revenue_by_payer(&mut tx, from, to).await?);
    let by_talent = rows(core_db::reports::revenue_by_talent(&mut tx, from, to).await?);
    let mut ageing = [0i64; 5];
    let outstanding: Vec<OutstandingRow> = core_db::reports::outstanding(&mut tx)
        .await?
        .into_iter()
        .map(|o| {
            let days = o.due_date.map_or(0, |d| (today - d).num_days().max(0));
            let bucket = match days {
                0 => 0,
                1..=30 => 1,
                31..=60 => 2,
                61..=90 => 3,
                _ => 4,
            };
            ageing[bucket] += o.base_outstanding_minor.unwrap_or(0);
            let currency = o.currency.trim().to_owned();
            OutstandingRow {
                invoice_id: o.id,
                reference: o.reference,
                payer: o.payer,
                booking_id: o.booking_id,
                outstanding: Money {
                    minor: o.outstanding_minor,
                    currency,
                },
                base_outstanding: o.base_outstanding_minor.map(money),
                issue_date: o.issue_date,
                due_date: o.due_date,
                days_overdue: days,
                status: o.status,
            }
        })
        .collect();
    let cash = core_db::reports::cash(&mut tx, from, to)
        .await?
        .into_iter()
        .map(|c| CashRow {
            currency: c.currency.trim().to_owned(),
            received_minor: c.received_minor,
            refunded_minor: c.refunded_minor,
            paid_out_minor: c.paid_out_minor,
        })
        .collect();
    let f = core_db::reports::funnel(&mut tx, from, to).await?;
    tx.commit().await?;
    let decided = f.won + f.lost;
    Ok(FinanceReport {
        from,
        to,
        by_month,
        by_payer,
        by_talent,
        outstanding,
        ageing: Ageing {
            current: money(ageing[0]),
            days_1_30: money(ageing[1]),
            days_31_60: money(ageing[2]),
            days_61_90: money(ageing[3]),
            over_90: money(ageing[4]),
        },
        cash,
        funnel: FunnelReport {
            enquiries: f.enquiries,
            quoted: f.quoted,
            won: f.won,
            lost: f.lost,
            open: f.open,
            win_rate: (decided > 0).then(|| f.won as f64 / decided as f64),
        },
        base_currency: base,
    })
}

/// Minor units as a decimal amount for a spreadsheet (two places for most
/// currencies, none for yen and the like).
fn decimal(minor: i64, currency: &str) -> String {
    let places = core_domain::money::minor_unit_exponent(currency);
    if places == 0 {
        return minor.to_string();
    }
    let scale = 10i64.pow(places);
    let sign = if minor < 0 { "-" } else { "" };
    let abs = minor.unsigned_abs();
    format!(
        "{sign}{}.{:0width$}",
        abs / scale as u64,
        abs % scale as u64,
        width = places as usize
    )
}

/// One of the report's tables, or the invoice or payment list, as CSV.
pub async fn csv(state: &AppState, ctx: &Ctx, table: &str, p: &Period) -> ApiResult<String> {
    let mut w = csv::Writer::from_writer(Vec::new());
    let err = ApiError::internal;
    match table {
        "invoices" => {
            ctx.authorize(Permission::InvoiceRead)?;
            let page = super::invoices::list(
                state,
                ctx,
                &super::invoices::InvoiceFilter {
                    issued_from: p.from,
                    issued_to: p.to,
                    limit: Some(500),
                    ..Default::default()
                },
            )
            .await?;
            w.write_record([
                "reference",
                "kind",
                "status",
                "payer",
                "issue_date",
                "due_date",
                "currency",
                "subtotal",
                "tax",
                "total",
                "paid",
                "credited",
                "outstanding",
                "base_currency",
                "fx_rate",
                "base_total",
            ])
            .map_err(err)?;
            for i in page.items {
                let c = i.currency.clone();
                w.write_record([
                    i.reference.unwrap_or_default(),
                    i.kind,
                    i.status.as_str().to_owned(),
                    i.payer_name,
                    i.issue_date.map(|d| d.to_string()).unwrap_or_default(),
                    i.due_date.map(|d| d.to_string()).unwrap_or_default(),
                    c.clone(),
                    decimal(i.subtotal.minor, &c),
                    decimal(i.tax.minor, &c),
                    decimal(i.total.minor, &c),
                    decimal(i.paid.minor, &c),
                    decimal(i.credited.minor, &c),
                    decimal(i.outstanding.minor, &c),
                    i.base_currency.clone().unwrap_or_default(),
                    i.fx_rate.map(|r| r.to_string()).unwrap_or_default(),
                    i.base_total
                        .map(|m| decimal(m.minor, &m.currency))
                        .unwrap_or_default(),
                ])
                .map_err(err)?;
            }
        }
        "payments" => {
            ctx.authorize(Permission::InvoiceRead)?;
            let page = super::payments::list(
                state,
                ctx,
                &super::payments::PaymentFilter {
                    from: p.from,
                    to: p.to,
                    limit: Some(500),
                    ..Default::default()
                },
            )
            .await?;
            w.write_record([
                "paid_on",
                "kind",
                "method",
                "currency",
                "amount",
                "reference",
                "organisation",
                "talent",
                "voided",
            ])
            .map_err(err)?;
            for x in page.items {
                w.write_record([
                    x.paid_on.to_string(),
                    x.kind,
                    x.method,
                    x.amount.currency.clone(),
                    decimal(x.amount.minor, &x.amount.currency),
                    x.reference.unwrap_or_default(),
                    x.organisation_name.unwrap_or_default(),
                    x.talent_name.unwrap_or_default(),
                    x.voided_at.map(|_| "yes".to_owned()).unwrap_or_default(),
                ])
                .map_err(err)?;
            }
        }
        "revenue_by_month" | "revenue_by_payer" | "revenue_by_talent" | "outstanding" => {
            let r = finance(state, ctx, p).await?;
            if table == "outstanding" {
                w.write_record([
                    "reference",
                    "payer",
                    "status",
                    "due_date",
                    "days_overdue",
                    "currency",
                    "outstanding",
                    "base_outstanding",
                ])
                .map_err(err)?;
                for o in r.outstanding {
                    w.write_record([
                        o.reference.unwrap_or_default(),
                        o.payer,
                        o.status,
                        o.due_date.map(|d| d.to_string()).unwrap_or_default(),
                        o.days_overdue.to_string(),
                        o.outstanding.currency.clone(),
                        decimal(o.outstanding.minor, &o.outstanding.currency),
                        o.base_outstanding
                            .map(|m| decimal(m.minor, &m.currency))
                            .unwrap_or_default(),
                    ])
                    .map_err(err)?;
                }
            } else {
                let rows = match table {
                    "revenue_by_month" => r.by_month,
                    "revenue_by_payer" => r.by_payer,
                    _ => r.by_talent,
                };
                w.write_record(["key", "invoices", "currency", "invoiced", "tax"])
                    .map_err(err)?;
                for row in rows {
                    w.write_record([
                        row.key,
                        row.invoices.to_string(),
                        row.invoiced.currency.clone(),
                        decimal(row.invoiced.minor, &row.invoiced.currency),
                        decimal(row.tax.minor, &row.tax.currency),
                    ])
                    .map_err(err)?;
                }
            }
        }
        other => {
            return Err(ApiError::invalid(
                "table",
                format!(
                    "`{other}` is not a report; use invoices, payments, revenue_by_month, revenue_by_payer, revenue_by_talent or outstanding"
                ),
            ));
        }
    }
    let bytes = w
        .into_inner()
        .map_err(|e| ApiError::internal(e.to_string()))?;
    String::from_utf8(bytes).map_err(ApiError::internal)
}

#[cfg(test)]
mod tests {
    use super::decimal;

    #[test]
    fn decimals() {
        assert_eq!(decimal(123_456, "GBP"), "1234.56");
        assert_eq!(decimal(-5, "EUR"), "-0.05");
        assert_eq!(decimal(1500, "JPY"), "1500");
        assert_eq!(decimal(1_234, "BHD"), "1.234");
    }
}
