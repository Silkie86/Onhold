//! Value types and arithmetic of money owed: an invoice's status, tax
//! grouped by code, invoice numbers, exchange rates and settlements.
//! Every amount is an integer of minor units; rounding is half away from
//! zero, once, at the step the paperwork shows.

use std::fmt;
use std::str::FromStr;

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::bookings::MAX_AMOUNT_MINOR;
use crate::money::minor_unit_exponent;

db_enum!(
    /// Where an invoice stands. Issued means numbered and frozen; part paid,
    /// paid, overdue and credited follow from the money allocated to it, and
    /// void takes it out of the books while keeping its number.
    InvoiceStatus, "invoice_status", [
        Draft => "draft",
        Issued => "issued",
        PartPaid => "part_paid",
        Paid => "paid",
        Overdue => "overdue",
        Credited => "credited",
        Void => "void",
    ]
);

impl InvoiceStatus {
    /// Issued and still owed money.
    pub fn is_open(self) -> bool {
        matches!(
            self,
            InvoiceStatus::Issued | InvoiceStatus::PartPaid | InvoiceStatus::Overdue
        )
    }

    /// In the books: issued, whatever has been paid.
    pub fn is_issued(self) -> bool {
        !matches!(self, InvoiceStatus::Draft | InvoiceStatus::Void)
    }

    /// The event emitted on entering this status.
    pub fn topic(self) -> &'static str {
        match self {
            InvoiceStatus::Draft => "invoice.created",
            InvoiceStatus::Issued => "invoice.issued",
            InvoiceStatus::PartPaid => "invoice.part_paid",
            InvoiceStatus::Paid => "invoice.paid",
            InvoiceStatus::Overdue => "invoice.overdue",
            InvoiceStatus::Credited => "invoice.credited",
            InvoiceStatus::Void => "invoice.voided",
        }
    }
}

/// The status an issued invoice has from what has been paid and credited
/// against it. Paid only when the allocations (and credit notes) reach the
/// total; overdue once a day has passed beyond the due date unpaid.
pub fn status_from_amounts(
    total: i64,
    paid: i64,
    credited: i64,
    due_date: Option<NaiveDate>,
    today: NaiveDate,
) -> InvoiceStatus {
    let settled = paid.saturating_add(credited);
    if settled >= total {
        if paid == 0 && credited > 0 {
            InvoiceStatus::Credited
        } else {
            InvoiceStatus::Paid
        }
    } else if due_date.is_some_and(|d| d < today) {
        InvoiceStatus::Overdue
    } else if settled > 0 {
        InvoiceStatus::PartPaid
    } else {
        InvoiceStatus::Issued
    }
}

/// `bps` hundredths of a percent of `amount` (2000 is 20%), rounded half
/// away from zero.
pub fn percent_of(amount: i64, bps: i64) -> Option<i64> {
    let raw = i128::from(amount).checked_mul(i128::from(bps))?;
    let rounded = if raw >= 0 {
        (raw + 5_000) / 10_000
    } else {
        (raw - 5_000) / 10_000
    };
    i64::try_from(rounded).ok()
}

/// What kind of tax a code is. Zero, exempt, reverse-charge and
/// out-of-scope codes charge nothing but print different wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaxKind {
    Standard,
    Reduced,
    Zero,
    Exempt,
    ReverseCharge,
    OutOfScope,
}

impl TaxKind {
    pub const ALL: &'static [TaxKind] = &[
        TaxKind::Standard,
        TaxKind::Reduced,
        TaxKind::Zero,
        TaxKind::Exempt,
        TaxKind::ReverseCharge,
        TaxKind::OutOfScope,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TaxKind::Standard => "standard",
            TaxKind::Reduced => "reduced",
            TaxKind::Zero => "zero",
            TaxKind::Exempt => "exempt",
            TaxKind::ReverseCharge => "reverse_charge",
            TaxKind::OutOfScope => "out_of_scope",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == s.trim())
    }

    /// Codes that charge no tax whatever their rate says.
    pub fn charges_nothing(self) -> bool {
        !matches!(self, TaxKind::Standard | TaxKind::Reduced)
    }
}

/// One invoice line's net amount and the tax code it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaxedAmount {
    pub amount_minor: i64,
    pub tax_code_id: Option<Uuid>,
    pub rate_bps: i32,
}

/// The tax on the lines sharing one code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaxGroup {
    pub tax_code_id: Option<Uuid>,
    pub rate_bps: i32,
    pub net_minor: i64,
    pub tax_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Totals {
    /// In the order the codes first appear on the invoice.
    pub groups: Vec<TaxGroup>,
    pub subtotal_minor: i64,
    pub tax_minor: i64,
    pub total_minor: i64,
}

/// Groups lines by tax code and works out the tax on each group's net
/// total, as VAT invoices show it: rounding once per rate, not per line.
/// `None` if the amounts overflow or pass [`MAX_AMOUNT_MINOR`].
pub fn totals(lines: &[TaxedAmount]) -> Option<Totals> {
    let mut groups: Vec<TaxGroup> = Vec::new();
    for line in lines {
        match groups
            .iter_mut()
            .find(|g| g.tax_code_id == line.tax_code_id && g.rate_bps == line.rate_bps)
        {
            Some(g) => g.net_minor = g.net_minor.checked_add(line.amount_minor)?,
            None => groups.push(TaxGroup {
                tax_code_id: line.tax_code_id,
                rate_bps: line.rate_bps,
                net_minor: line.amount_minor,
                tax_minor: 0,
            }),
        }
    }
    let mut subtotal: i64 = 0;
    let mut tax: i64 = 0;
    for g in &mut groups {
        g.tax_minor = percent_of(g.net_minor, i64::from(g.rate_bps))?;
        subtotal = subtotal.checked_add(g.net_minor)?;
        tax = tax.checked_add(g.tax_minor)?;
    }
    let total = subtotal.checked_add(tax)?;
    (total.abs() <= MAX_AMOUNT_MINOR).then_some(Totals {
        groups,
        subtotal_minor: subtotal,
        tax_minor: tax,
        total_minor: total,
    })
}

/// A tax code a new workspace starts with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultTaxCode {
    pub name: &'static str,
    pub rate_bps: i32,
    pub kind: TaxKind,
    pub note: Option<&'static str>,
    pub is_default: bool,
}

const fn code(
    name: &'static str,
    rate_bps: i32,
    kind: TaxKind,
    note: Option<&'static str>,
    is_default: bool,
) -> DefaultTaxCode {
    DefaultTaxCode {
        name,
        rate_bps,
        kind,
        note,
        is_default,
    }
}

/// Standard VAT rates in the EU, in hundredths of a percent (October 2026).
/// Workspaces edit their own copies; these only seed them.
const EU_STANDARD: &[(&str, i32)] = &[
    ("AT", 2000),
    ("BE", 2100),
    ("BG", 2000),
    ("CY", 1900),
    ("CZ", 2100),
    ("DE", 1900),
    ("DK", 2500),
    ("EE", 2400),
    ("ES", 2100),
    ("FI", 2550),
    ("FR", 2000),
    ("GR", 2400),
    ("HR", 2500),
    ("HU", 2700),
    ("IE", 2300),
    ("IT", 2200),
    ("LT", 2100),
    ("LU", 1700),
    ("LV", 2100),
    ("MT", 1800),
    ("NL", 2100),
    ("PL", 2300),
    ("PT", 2300),
    ("RO", 2100),
    ("SE", 2500),
    ("SI", 2200),
    ("SK", 2300),
];

const REVERSE_CHARGE_NOTE: &str =
    "Reverse charge: the customer accounts for the VAT on this supply.";

/// The tax codes a workspace in `country` starts with: the local rates
/// people need most, the zero and exempt cases, and (in the EU) reverse
/// charge for business customers in other member states. Countries whose
/// sales tax is left to the accounting system (the US) get a single
/// no-tax code.
pub fn default_tax_codes(country: &str) -> Vec<DefaultTaxCode> {
    use TaxKind::*;
    if let Some((_, rate)) = EU_STANDARD.iter().find(|(c, _)| *c == country) {
        return vec![
            code("VAT standard", *rate, Standard, None, true),
            code("Zero-rated", 0, Zero, None, false),
            code("Exempt", 0, Exempt, Some("Exempt from VAT."), false),
            code(
                "Reverse charge (EU business)",
                0,
                ReverseCharge,
                Some(REVERSE_CHARGE_NOTE),
                false,
            ),
            code(
                "Outside the EU",
                0,
                OutOfScope,
                Some("Outside the scope of EU VAT."),
                false,
            ),
        ];
    }
    match country {
        "GB" => vec![
            code("VAT 20%", 2000, Standard, None, true),
            code("VAT 5%", 500, Reduced, None, false),
            code("Zero-rated", 0, Zero, None, false),
            code("Exempt", 0, Exempt, Some("Exempt from VAT."), false),
            code(
                "Reverse charge",
                0,
                ReverseCharge,
                Some(REVERSE_CHARGE_NOTE),
                false,
            ),
            code(
                "Outside the scope of UK VAT",
                0,
                OutOfScope,
                Some("Outside the scope of UK VAT."),
                false,
            ),
        ],
        "AU" => vec![
            code("GST 10%", 1000, Standard, None, true),
            code("GST-free", 0, Zero, None, false),
        ],
        "NZ" => vec![
            code("GST 15%", 1500, Standard, None, true),
            code("Zero-rated", 0, Zero, None, false),
        ],
        "CA" => vec![
            code("GST 5%", 500, Standard, None, true),
            code("HST 13%", 1300, Standard, None, false),
            code("Zero-rated", 0, Zero, None, false),
        ],
        "CH" | "LI" => vec![
            code("VAT 8.1%", 810, Standard, None, true),
            code("VAT 2.6%", 260, Reduced, None, false),
            code("Exempt", 0, Exempt, None, false),
        ],
        "NO" => vec![
            code("VAT 25%", 2500, Standard, None, true),
            code("Exempt", 0, Exempt, None, false),
        ],
        _ => vec![code("No tax", 0, OutOfScope, None, true)],
    }
}

// Invoice numbers --------------------------------------------------------------

/// Checks a number format such as `INV-{YYYY}-{0000}`: plain characters
/// (letters, digits, `-`, `/`, `_`, `.`, spaces) and these placeholders:
/// `{YYYY}`, `{YY}`, `{MM}` (the issue date) and exactly one number,
/// `{0000}` (zero-padded to as many digits as there are zeros, 1 to 10) or
/// `{N}`.
pub fn check_number_format(format: &str) -> Result<(), String> {
    if format.trim().is_empty() || format.chars().count() > 60 {
        return Err("must be 1 to 60 characters".into());
    }
    let mut numbers = 0;
    let mut rest = format;
    while let Some(open) = rest.find('{') {
        let (plain, tail) = rest.split_at(open);
        check_plain(plain)?;
        let close = tail
            .find('}')
            .ok_or_else(|| "a `{` is never closed".to_owned())?;
        let token = &tail[1..close];
        match token {
            "YYYY" | "YY" | "MM" => {}
            "N" => numbers += 1,
            t if !t.is_empty() && t.len() <= 10 && t.bytes().all(|b| b == b'0') => numbers += 1,
            other => {
                return Err(format!(
                    "`{{{other}}}` is not a placeholder; use {{YYYY}}, {{YY}}, {{MM}}, {{0000}} or {{N}}"
                ));
            }
        }
        rest = &tail[close + 1..];
    }
    check_plain(rest)?;
    if numbers != 1 {
        return Err("put the number in exactly once, as {0000} or {N}".into());
    }
    Ok(())
}

fn check_plain(s: &str) -> Result<(), String> {
    match s
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '/' | '_' | '.' | ' ')))
    {
        Some(c) => Err(format!("`{c}` cannot be used in an invoice number")),
        None => Ok(()),
    }
}

/// The number as printed, from a format [`check_number_format`] accepts.
pub fn format_number(format: &str, number: i64, issued: NaiveDate) -> String {
    let mut out = String::new();
    let mut rest = format;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let tail = &rest[open..];
        let Some(close) = tail.find('}') else {
            out.push_str(tail);
            return out;
        };
        match &tail[1..close] {
            "YYYY" => out.push_str(&format!("{:04}", issued.year())),
            "YY" => out.push_str(&format!("{:02}", issued.year().rem_euclid(100))),
            "MM" => out.push_str(&format!("{:02}", issued.month())),
            "N" => out.push_str(&number.to_string()),
            zeros => out.push_str(&format!("{number:0width$}", width = zeros.len())),
        }
        rest = &tail[close + 1..];
    }
    out.push_str(rest);
    out
}

// Exchange rates ------------------------------------------------------------------

/// Decimal places kept in a rate, as in the database's `numeric(20, 10)`.
const RATE_SCALE: i128 = 10_000_000_000;

/// A positive exchange rate with up to ten decimal places, kept exactly.
/// On the wire it is a decimal string, `"1.1734"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, utoipa::ToSchema)]
#[schema(value_type = String, example = "1.1734")]
pub struct Rate(i128);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{0}` is not a positive rate with at most ten decimal places")]
pub struct BadRate(pub String);

impl Rate {
    pub const ONE: Rate = Rate(RATE_SCALE);

    /// The rate as ten-decimal-place units.
    pub fn scaled(self) -> i128 {
        self.0
    }

    /// Units of currency B per unit of A, given how many of each one euro
    /// buys: `b_per_eur / a_per_eur`, rounded to ten places.
    pub fn cross(a_per_eur: Rate, b_per_eur: Rate) -> Option<Rate> {
        let num = b_per_eur.0.checked_mul(RATE_SCALE)?;
        let q = div_round(num, a_per_eur.0)?;
        (q > 0).then_some(Rate(q))
    }
}

/// `n / d` rounded half away from zero, for a positive `d`.
fn div_round(n: i128, d: i128) -> Option<i128> {
    if d <= 0 {
        return None;
    }
    let half = d / 2;
    Some(if n >= 0 {
        n.checked_add(half)? / d
    } else {
        n.checked_sub(half)? / d
    })
}

impl FromStr for Rate {
    type Err = BadRate;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || BadRate(s.to_owned());
        let t = s.trim();
        let (whole, frac) = t.split_once('.').unwrap_or((t, ""));
        if whole.is_empty()
            || whole.len() > 10
            || !whole.bytes().all(|b| b.is_ascii_digit())
            || !frac.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(bad());
        }
        // Trailing zeros past ten places change nothing; other digits would.
        let frac = frac.trim_end_matches('0');
        if frac.len() > 10 {
            return Err(bad());
        }
        let whole: i128 = whole.parse().map_err(|_| bad())?;
        let frac_value: i128 = if frac.is_empty() {
            0
        } else {
            frac.parse::<i128>().map_err(|_| bad())? * 10i128.pow(10 - frac.len() as u32)
        };
        let value = whole * RATE_SCALE + frac_value;
        if value <= 0 {
            return Err(bad());
        }
        Ok(Rate(value))
    }
}

impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let whole = self.0 / RATE_SCALE;
        let frac = self.0 % RATE_SCALE;
        if frac == 0 {
            return write!(f, "{whole}");
        }
        let digits = format!("{frac:010}");
        write!(f, "{whole}.{}", digits.trim_end_matches('0'))
    }
}

impl Serialize for Rate {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Rate {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Converts `minor` units of `from` into `to` at `rate` (units of `to` per
/// unit of `from`), allowing for currencies with different minor units,
/// rounded half away from zero. `None` if the result overflows.
pub fn convert(minor: i64, from: &str, to: &str, rate: Rate) -> Option<i64> {
    let e_from = minor_unit_exponent(from);
    let e_to = minor_unit_exponent(to);
    let num = i128::from(minor)
        .checked_mul(rate.0)?
        .checked_mul(10i128.pow(e_to))?;
    let den = RATE_SCALE.checked_mul(10i128.pow(e_from))?;
    i64::try_from(div_round(num, den)?).ok()
}

// Settlements ------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settlement {
    pub commission_minor: i64,
    pub expenses_minor: i64,
    /// What the talent is owed; negative if costs exceed their share.
    pub net_minor: i64,
}

/// The talent's share of `gross` after `commission_bps` commission and
/// the agency's `expenses`.
pub fn settle(gross: i64, commission_bps: i32, expenses: &[i64]) -> Option<Settlement> {
    let commission = percent_of(gross, i64::from(commission_bps))?;
    let mut spent: i64 = 0;
    for e in expenses {
        spent = spent.checked_add(*e)?;
    }
    let net = gross.checked_sub(commission)?.checked_sub(spent)?;
    Some(Settlement {
        commission_minor: commission,
        expenses_minor: spent,
        net_minor: net,
    })
}

/// What a document is.
pub const DOCUMENT_KINDS: &[&str] = &[
    "contract",
    "confirmation",
    "advance_sheet",
    "invoice",
    "credit_note",
    "quote",
    "statement",
    "rider",
    "tech_spec",
    "stage_plot",
    "epk",
    "insurance",
    "passport",
    "visa",
    "receipt",
    "purchase_order",
    "misc",
];

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).expect("date")
    }

    #[test]
    fn status_follows_the_money() {
        let today = d(2026, 6, 10);
        use InvoiceStatus::*;
        assert_eq!(status_from_amounts(1000, 0, 0, Some(today), today), Issued);
        assert_eq!(
            status_from_amounts(1000, 0, 0, Some(d(2026, 6, 9)), today),
            Overdue
        );
        assert_eq!(status_from_amounts(1000, 400, 0, None, today), PartPaid);
        assert_eq!(
            status_from_amounts(1000, 400, 0, Some(d(2026, 1, 1)), today),
            Overdue
        );
        assert_eq!(status_from_amounts(1000, 600, 400, None, today), Paid);
        assert_eq!(status_from_amounts(1000, 0, 1000, None, today), Credited);
        assert_eq!(status_from_amounts(0, 0, 0, None, today), Paid);
        assert!(Overdue.is_open() && !Paid.is_open() && !Void.is_issued());
    }

    #[test]
    fn tax_is_rounded_once_per_code() {
        let vat = Some(Uuid::nil());
        let lines = [
            TaxedAmount {
                amount_minor: 333,
                tax_code_id: vat,
                rate_bps: 2000,
            },
            TaxedAmount {
                amount_minor: 333,
                tax_code_id: vat,
                rate_bps: 2000,
            },
            TaxedAmount {
                amount_minor: 1000,
                tax_code_id: None,
                rate_bps: 0,
            },
        ];
        let t = totals(&lines).expect("fits");
        // 20% of 666 is 133.2: one rounding, not 67 + 67.
        assert_eq!(t.groups.len(), 2);
        assert_eq!(t.groups[0].net_minor, 666);
        assert_eq!(t.groups[0].tax_minor, 133);
        assert_eq!(t.subtotal_minor, 1666);
        assert_eq!(t.tax_minor, 133);
        assert_eq!(t.total_minor, 1799);
        assert_eq!(percent_of(-250, 2000), Some(-50));
        assert_eq!(percent_of(25, 2000), Some(5));
        assert_eq!(percent_of(5, 1000), Some(1)); // 0.5 rounds up
        assert_eq!(
            totals(&[TaxedAmount {
                amount_minor: i64::MAX,
                tax_code_id: None,
                rate_bps: 2000
            }]),
            None
        );
    }

    #[test]
    fn countries_get_their_own_codes() {
        let gb = default_tax_codes("GB");
        assert_eq!(gb.iter().filter(|c| c.is_default).count(), 1);
        assert_eq!(gb[0].rate_bps, 2000);
        let de = default_tax_codes("DE");
        assert_eq!(de[0].rate_bps, 1900);
        assert!(de.iter().any(|c| c.kind == TaxKind::ReverseCharge));
        let us = default_tax_codes("US");
        assert_eq!(us.len(), 1);
        assert_eq!(us[0].rate_bps, 0);
        for country in ["GB", "FR", "AU", "NZ", "CA", "CH", "NO", "US", "JP"] {
            for c in default_tax_codes(country) {
                assert!(
                    !c.kind.charges_nothing() || c.rate_bps == 0,
                    "{country} {}",
                    c.name
                );
            }
        }
    }

    #[test]
    fn number_formats() {
        let day = d(2026, 3, 7);
        assert_eq!(format_number("INV-{YYYY}-{0000}", 42, day), "INV-2026-0042");
        assert_eq!(format_number("{YY}{MM}/{N}", 7, day), "2603/7");
        assert_eq!(format_number("A{000}", 12345, day), "A12345");
        assert!(check_number_format("INV-{YYYY}-{0000}").is_ok());
        assert!(check_number_format("{N}").is_ok());
        assert!(check_number_format("INV").is_err());
        assert!(check_number_format("{N}-{0000}").is_err());
        assert!(check_number_format("INV-{DD}-{N}").is_err());
        assert!(check_number_format("INV<{N}>").is_err());
        assert!(check_number_format("INV-{N").is_err());
    }

    #[test]
    fn rates_parse_and_print_exactly() {
        let r: Rate = "1.1734".parse().expect("rate");
        assert_eq!(r.to_string(), "1.1734");
        assert_eq!("160".parse::<Rate>().expect("rate").to_string(), "160");
        assert_eq!("0.0000000001".parse::<Rate>().expect("rate").scaled(), 1);
        assert_eq!(
            "1.50000000000000"
                .parse::<Rate>()
                .expect("rate")
                .to_string(),
            "1.5"
        );
        for bad in ["", "0", "-1", "1.2.3", "abc", "1.00000000001", ".5"] {
            assert!(bad.parse::<Rate>().is_err(), "{bad}");
        }
    }

    #[test]
    fn conversions_respect_minor_units() {
        let rate: Rate = "0.85".parse().expect("rate");
        // $100.00 at 0.85 is £85.00.
        assert_eq!(convert(10_000, "USD", "GBP", rate), Some(8_500));
        // ¥10,000 at 0.0053 is £53.00.
        let yen: Rate = "0.0053".parse().expect("rate");
        assert_eq!(convert(10_000, "JPY", "GBP", yen), Some(5_300));
        // £53.00 at 188.68 is ¥10,000 (10,000.04).
        let back: Rate = "188.68".parse().expect("rate");
        assert_eq!(convert(5_300, "GBP", "JPY", back), Some(10_000));
        assert_eq!(convert(1, "GBP", "GBP", Rate::ONE), Some(1));
        // From the ECB's per-euro rates: GBP 0.86, USD 1.10.
        let gbp: Rate = "0.86".parse().expect("rate");
        let usd: Rate = "1.10".parse().expect("rate");
        let usd_to_gbp = Rate::cross(usd, gbp).expect("cross");
        assert_eq!(usd_to_gbp.to_string(), "0.7818181818");
        assert_eq!(convert(10_000, "USD", "GBP", usd_to_gbp), Some(7_818));
    }

    #[test]
    fn settlements_take_commission_then_expenses() {
        let s = settle(250_000, 1500, &[5_000, 2_500]).expect("fits");
        assert_eq!(s.commission_minor, 37_500);
        assert_eq!(s.expenses_minor, 7_500);
        assert_eq!(s.net_minor, 205_000);
        assert_eq!(settle(100, 0, &[200]).map(|s| s.net_minor), Some(-100));
    }

    proptest! {
        #[test]
        fn totals_add_up(amounts in proptest::collection::vec((-10_000_000i64..10_000_000, 0usize..3), 0..20)) {
            let codes = [(None, 0), (Some(Uuid::from_u128(1)), 2000), (Some(Uuid::from_u128(2)), 550)];
            let lines: Vec<TaxedAmount> = amounts
                .iter()
                .map(|(a, c)| TaxedAmount { amount_minor: *a, tax_code_id: codes[*c].0, rate_bps: codes[*c].1 })
                .collect();
            let t = totals(&lines).expect("small amounts fit");
            prop_assert_eq!(t.subtotal_minor, amounts.iter().map(|(a, _)| a).sum::<i64>());
            prop_assert_eq!(t.total_minor, t.subtotal_minor + t.tax_minor);
            prop_assert_eq!(t.tax_minor, t.groups.iter().map(|g| g.tax_minor).sum::<i64>());
            for g in &t.groups {
                // Within half a minor unit of the exact tax.
                let exact = i128::from(g.net_minor) * i128::from(g.rate_bps);
                prop_assert!((i128::from(g.tax_minor) * 10_000 - exact).abs() <= 5_000);
            }
        }

        #[test]
        fn rates_round_trip(scaled in 1i128..1_000_000_000_000_000_000) {
            let r = Rate(scaled);
            prop_assert_eq!(r.to_string().parse::<Rate>().expect("parses"), r);
        }

        #[test]
        fn conversion_at_one_is_identity(minor in -1_000_000_000_000i64..1_000_000_000_000) {
            prop_assert_eq!(convert(minor, "EUR", "GBP", Rate::ONE), Some(minor));
        }

        #[test]
        fn settlement_parts_sum_to_gross(gross in 0i64..1_000_000_000_000, bps in 0i32..=10_000, e in 0i64..1_000_000) {
            let s = settle(gross, bps, &[e]).expect("fits");
            prop_assert_eq!(s.commission_minor + s.expenses_minor + s.net_minor, gross);
        }
    }
}
