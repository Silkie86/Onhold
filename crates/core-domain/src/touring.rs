//! Whether a date (or a whole run of dates) pays: income (fee and buyouts)
//! less commission and costs, and the warnings people should see before
//! they say yes.

use serde::Serialize;

/// Costs above this share of the income get a warning.
pub const HIGH_COST_PERCENT: i64 = 50;

/// What travel can be.
pub const TRAVEL_KINDS: &[&str] = &[
    "flight", "train", "bus", "car", "ferry", "taxi", "hotel", "other",
];

/// Income beside the fee.
pub const INCOME_CATEGORIES: &[&str] = &["buyout", "merchandise", "other"];

/// What a date costs, beside travel entered as travel.
pub const COST_CATEGORIES: &[&str] = &[
    "travel",
    "accommodation",
    "ground",
    "per_diem",
    "crew",
    "production",
    "visa",
    "marketing",
    "other",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WarningLevel {
    /// Worth a look.
    Caution,
    /// Losing money.
    Loss,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct Warning {
    pub level: WarningLevel,
    /// `loss`, `high_costs`, `no_income`, `unconverted`, `unpriced`.
    pub code: String,
    pub message: String,
}

impl Warning {
    pub fn new(level: WarningLevel, code: &str, message: impl Into<String>) -> Self {
        Self {
            level,
            code: code.to_owned(),
            message: message.into(),
        }
    }
}

/// Net and the warnings for income, commission and costs in one currency,
/// in minor units. `None` on overflow.
pub fn assess(income: i64, commission: i64, costs: i64) -> Option<(i64, Vec<Warning>)> {
    let net = income.checked_sub(commission)?.checked_sub(costs)?;
    let mut warnings = Vec::new();
    if income == 0 && costs > 0 {
        warnings.push(Warning::new(
            WarningLevel::Loss,
            "no_income",
            "There are costs but no fee or other income yet.",
        ));
    } else if net < 0 {
        warnings.push(Warning::new(
            WarningLevel::Loss,
            "loss",
            "Costs and commission are more than the fee and buyouts.",
        ));
    } else if income > 0
        && i128::from(costs) * 100 > i128::from(income) * i128::from(HIGH_COST_PERCENT)
    {
        let share = i128::from(costs) * 100 / i128::from(income);
        warnings.push(Warning::new(
            WarningLevel::Caution,
            "high_costs",
            format!("Costs take {share}% of the income."),
        ));
    }
    Some((net, warnings))
}

/// An IATA airport code ("LHR") from what was typed, if it is one.
pub fn airport_code(raw: &str) -> Option<String> {
    let s = raw.trim().to_ascii_uppercase();
    (s.len() == 3 && s.bytes().all(|b| b.is_ascii_uppercase())).then_some(s)
}

/// A flight or train number tidied: "ba1442" becomes "BA 1442". Anything
/// that does not look like a carrier code then digits is kept as typed.
pub fn transport_number(raw: &str) -> String {
    let s: String = raw
        .split_whitespace()
        .collect::<String>()
        .to_ascii_uppercase();
    for carrier_len in [2, 3] {
        if s.len() <= carrier_len || !s.is_char_boundary(carrier_len) {
            continue;
        }
        let (carrier, rest) = s.split_at(carrier_len);
        let digits = rest
            .strip_suffix(|c: char| c.is_ascii_uppercase())
            .unwrap_or(rest);
        // IATA carriers are two letters or digits (at least one letter);
        // ICAO and rail operators three letters.
        let carrier_ok = if carrier_len == 2 {
            carrier.bytes().all(|b| b.is_ascii_alphanumeric())
                && carrier.bytes().any(|b| b.is_ascii_uppercase())
        } else {
            carrier.bytes().all(|b| b.is_ascii_uppercase())
        };
        if carrier_ok
            && (1..=5).contains(&digits.len())
            && digits.bytes().all(|b| b.is_ascii_digit())
        {
            return format!("{carrier} {rest}");
        }
    }
    raw.trim().to_owned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_paying_date_has_no_warnings() {
        let (net, w) = assess(100_000, 20_000, 30_000).unwrap();
        assert_eq!(net, 50_000);
        assert!(w.is_empty());
    }

    #[test]
    fn heavy_costs_and_losses_are_flagged() {
        let (_, w) = assess(100_000, 10_000, 60_000).unwrap();
        assert_eq!(w[0].code, "high_costs");
        assert_eq!(w[0].message, "Costs take 60% of the income.");
        let (net, w) = assess(100_000, 20_000, 90_000).unwrap();
        assert_eq!(net, -10_000);
        assert_eq!(
            (w[0].level, w[0].code.as_str()),
            (WarningLevel::Loss, "loss")
        );
        let (_, w) = assess(0, 0, 5_000).unwrap();
        assert_eq!(w[0].code, "no_income");
        assert!(assess(0, 0, 0).unwrap().1.is_empty());
        assert!(assess(i64::MIN, 1, 0).is_none());
    }

    #[test]
    fn codes_and_numbers_are_tidied() {
        assert_eq!(airport_code(" lhr "), Some("LHR".into()));
        assert_eq!(airport_code("Heathrow"), None);
        assert_eq!(transport_number("ba1442"), "BA 1442");
        assert_eq!(transport_number("BA 1442"), "BA 1442");
        assert_eq!(transport_number("u2 8103"), "U2 8103");
        assert_eq!(transport_number("EZY8103"), "EZY 8103");
        assert_eq!(transport_number("ICE 597"), "ICE 597");
        assert_eq!(transport_number("Eurostar 9014"), "Eurostar 9014");
        assert_eq!(transport_number("1442"), "1442");
    }
}
