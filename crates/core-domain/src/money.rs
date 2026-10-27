//! Money: an integer count of a currency's minor units plus its ISO 4217
//! code. Never a float, in storage, on the wire or in arithmetic.

use serde::{Deserialize, Serialize};

use crate::workspace::{ValidationError, validate_currency};

/// `{ "minor": 125000, "currency": "GBP" }` is £1,250.00.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Money {
    /// Amount in the currency's smallest unit (pence, cents, yen).
    pub minor: i64,
    /// ISO 4217 code.
    pub currency: String,
}

impl Money {
    pub fn new(minor: i64, currency: &str) -> Result<Self, ValidationError> {
        validate_currency(currency).map_err(|e| ValidationError::new("currency", e.message))?;
        Ok(Self {
            minor,
            currency: currency.to_owned(),
        })
    }

    /// Adds two amounts of the same currency; `None` on a currency mismatch
    /// or overflow.
    pub fn checked_add(&self, other: &Money) -> Option<Money> {
        (self.currency == other.currency).then_some(())?;
        Some(Money {
            minor: self.minor.checked_add(other.minor)?,
            currency: self.currency.clone(),
        })
    }

    /// Formats for people, e.g. `£1,250.00` (en) or `1.250,00 €` (de).
    pub fn format(&self, locale: &str) -> String {
        format_minor(self.minor, &self.currency, locale)
    }
}

/// Digits after the decimal point for a currency. Most have two.
pub fn minor_unit_exponent(currency: &str) -> u32 {
    match currency {
        "BIF" | "CLP" | "DJF" | "GNF" | "ISK" | "JPY" | "KMF" | "KRW" | "PYG" | "RWF" | "UGX"
        | "UYI" | "VND" | "VUV" | "XAF" | "XOF" | "XPF" => 0,
        "BHD" | "IQD" | "JOD" | "KWD" | "LYD" | "OMR" | "TND" => 3,
        "CLF" | "UYW" => 4,
        _ => 2,
    }
}

fn symbol(currency: &str) -> Option<&'static str> {
    Some(match currency {
        "GBP" => "£",
        "EUR" => "€",
        "USD" => "$",
        "JPY" => "¥",
        "AUD" => "A$",
        "CAD" => "CA$",
        "NZD" => "NZ$",
        "INR" => "₹",
        "KRW" => "₩",
        "BRL" => "R$",
        "ZAR" => "R",
        "CNY" => "CN¥",
        "ILS" => "₪",
        "NGN" => "₦",
        "TRY" => "₺",
        _ => return None,
    })
}

struct Style {
    group: &'static str,
    decimal: &'static str,
    symbol_first: bool,
}

fn style(locale: &str) -> Style {
    let lang = locale.split(['-', '_']).next().unwrap_or("en");
    match lang {
        "de" if locale.ends_with("CH") => Style {
            group: "’",
            decimal: ".",
            symbol_first: true,
        },
        "de" | "es" | "it" | "nl" | "pt" | "da" | "id" | "tr" | "el" | "ro" | "hr" | "sl" => {
            Style {
                group: ".",
                decimal: ",",
                symbol_first: false,
            }
        }
        "fr" | "sv" | "nb" | "nn" | "no" | "fi" | "pl" | "cs" | "sk" | "ru" | "uk" | "hu"
        | "bg" | "lt" | "lv" | "et" => Style {
            group: "\u{202f}",
            decimal: ",",
            symbol_first: false,
        },
        _ => Style {
            group: ",",
            decimal: ".",
            symbol_first: true,
        },
    }
}

/// Formats `minor` units of `currency` for `locale`.
pub fn format_minor(minor: i64, currency: &str, locale: &str) -> String {
    let exp = minor_unit_exponent(currency);
    let style = style(locale);
    let abs = minor.unsigned_abs();
    let scale = 10u64.pow(exp);
    let (whole, frac) = (abs / scale, abs % scale);

    let digits = whole.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3 * 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push_str(style.group);
        }
        grouped.push(ch);
    }
    let number = if exp == 0 {
        grouped
    } else {
        format!(
            "{grouped}{}{frac:0width$}",
            style.decimal,
            width = exp as usize
        )
    };
    let sign = if minor < 0 { "-" } else { "" };
    match (symbol(currency), style.symbol_first) {
        (Some(sym), true) => format!("{sign}{sym}{number}"),
        (Some(sym), false) => format!("{sign}{number}\u{a0}{sym}"),
        (None, true) => format!("{sign}{currency}\u{a0}{number}"),
        (None, false) => format!("{sign}{number}\u{a0}{currency}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn formats_common_cases() {
        assert_eq!(format_minor(125_000, "GBP", "en-GB"), "£1,250.00");
        assert_eq!(format_minor(-5, "USD", "en-US"), "-$0.05");
        assert_eq!(format_minor(125_000, "EUR", "de-DE"), "1.250,00\u{a0}€");
        assert_eq!(format_minor(1_250, "JPY", "en"), "¥1,250");
        assert_eq!(format_minor(1_234_567, "KWD", "en"), "KWD\u{a0}1,234.567");
        assert_eq!(format_minor(100, "CHF", "de-CH"), "CHF\u{a0}1.00");
        assert_eq!(
            format_minor(i64::MIN, "GBP", "en"),
            "-£92,233,720,368,547,758.08"
        );
    }

    #[test]
    fn arithmetic_refuses_mixed_currencies() {
        let a = Money::new(100, "GBP").expect("valid");
        let b = Money::new(250, "GBP").expect("valid");
        let c = Money::new(1, "EUR").expect("valid");
        assert_eq!(a.checked_add(&b).map(|m| m.minor), Some(350));
        assert_eq!(a.checked_add(&c), None);
        assert!(Money::new(1, "XXX1").is_err());
    }

    proptest! {
        #[test]
        fn formatting_keeps_every_digit(minor in any::<i64>()) {
            let text = format_minor(minor, "GBP", "en-GB");
            let digits: String = text.chars().filter(char::is_ascii_digit).collect();
            let expected = minor.unsigned_abs().to_string();
            prop_assert_eq!(digits.trim_start_matches('0'), expected.trim_start_matches('0'));
            prop_assert_eq!(text.starts_with('-'), minor < 0);
        }

        #[test]
        fn addition_is_exact(a in -1_000_000_000_000i64..1_000_000_000_000, b in -1_000_000_000_000i64..1_000_000_000_000) {
            let x = Money { minor: a, currency: "EUR".into() };
            let y = Money { minor: b, currency: "EUR".into() };
            prop_assert_eq!(x.checked_add(&y).map(|m| m.minor), Some(a + b));
            prop_assert_eq!(x.checked_add(&y), y.checked_add(&x));
        }
    }
}
