//! Workspace settings and their validation.

use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, utoipa::ToSchema,
)]
#[sqlx(type_name = "workspace_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceStatus {
    Trial,
    Active,
    Suspended,
    Closed,
}

/// Whether the workspace represents talent (`sells`), books talent for its
/// own events (`buys`), or both. Changes vocabulary and defaults, never the
/// data model.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, utoipa::ToSchema,
)]
#[sqlx(type_name = "workspace_perspective", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Perspective {
    Sells,
    Buys,
    Both,
}

/// The switchable feature areas a workspace can enable. Switching one off
/// hides it; nothing in it is deleted.
pub const MODULES: &[&str] = &[
    "crm",
    "bookings",
    "calendar",
    "messaging",
    "documents",
    "events",
    "portal",
    "finance",
];

/// Finer switches inside the modules, so a one-person setup stays simple:
/// `team` (several people: assigning work to each other), `commission`
/// (taking a cut and paying talent their share) and `advanced` (custom
/// fields, pipelines and imports in the menu).
pub const EXTRAS: &[&str] = &["team", "commission", "advanced"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {message}")]
pub struct ValidationError {
    pub field: &'static str,
    pub message: String,
}

impl ValidationError {
    pub fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }
}

type Result<T> = std::result::Result<T, ValidationError>;

/// Lowercase letters, digits and inner hyphens, 1 to 48 characters.
pub fn validate_slug(field: &'static str, slug: &str) -> Result<()> {
    let ok = (1..=48).contains(&slug.len())
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !slug.starts_with('-')
        && !slug.ends_with('-');
    if ok {
        Ok(())
    } else {
        Err(ValidationError::new(
            field,
            "use 1 to 48 lowercase letters, digits or inner hyphens",
        ))
    }
}

pub fn validate_name(field: &'static str, name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 200 {
        return Err(ValidationError::new(field, "must be 1 to 200 characters"));
    }
    Ok(trimmed.to_owned())
}

pub fn validate_currency(code: &str) -> Result<()> {
    if ISO_4217.contains(&code) {
        Ok(())
    } else {
        Err(ValidationError::new(
            "default_currency",
            format!("`{code}` is not an ISO 4217 currency code"),
        ))
    }
}

pub fn validate_country(code: &str) -> Result<()> {
    if ISO_3166_ALPHA2.contains(&code) {
        Ok(())
    } else {
        Err(ValidationError::new(
            "country",
            format!("`{code}` is not an ISO 3166-1 alpha-2 country code"),
        ))
    }
}

pub fn validate_timezone(tz: &str) -> Result<()> {
    tz.parse::<chrono_tz::Tz>().map(|_| ()).map_err(|_| {
        ValidationError::new(
            "default_timezone",
            format!("`{tz}` is not an IANA timezone"),
        )
    })
}

/// A BCP 47 tag of the common shape: `en`, `en-GB`, `pt-BR`, `zh-Hant-TW`.
pub fn validate_locale(locale: &str) -> Result<()> {
    let mut parts = locale.split('-');
    let lang_ok = parts
        .next()
        .is_some_and(|l| (2..=3).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_lowercase()));
    let rest_ok =
        parts.all(|p| (2..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()));
    if lang_ok && rest_ok {
        Ok(())
    } else {
        Err(ValidationError::new(
            "locale",
            format!("`{locale}` is not a BCP 47 language tag"),
        ))
    }
}

pub fn validate_modules(modules: &[String]) -> Result<()> {
    match modules.iter().find(|m| !MODULES.contains(&m.as_str())) {
        Some(unknown) => Err(ValidationError::new(
            "enabled_modules",
            format!(
                "unknown module `{unknown}`; known modules are {}",
                MODULES.join(", ")
            ),
        )),
        None => Ok(()),
    }
}

pub fn validate_extras(extras: &[String]) -> Result<()> {
    match extras.iter().find(|m| !EXTRAS.contains(&m.as_str())) {
        Some(unknown) => Err(ValidationError::new(
            "extras",
            format!(
                "unknown extra `{unknown}`; known extras are {}",
                EXTRAS.join(", ")
            ),
        )),
        None => Ok(()),
    }
}

/// Active ISO 4217 codes (2026).
const ISO_4217: &[&str] = &[
    "AED", "AFN", "ALL", "AMD", "AOA", "ARS", "AUD", "AWG", "AZN", "BAM", "BBD", "BDT", "BHD",
    "BIF", "BMD", "BND", "BOB", "BRL", "BSD", "BTN", "BWP", "BYN", "BZD", "CAD", "CDF", "CHF",
    "CLP", "CNY", "COP", "CRC", "CUP", "CVE", "CZK", "DJF", "DKK", "DOP", "DZD", "EGP", "ERN",
    "ETB", "EUR", "FJD", "FKP", "GBP", "GEL", "GHS", "GIP", "GMD", "GNF", "GTQ", "GYD", "HKD",
    "HNL", "HTG", "HUF", "IDR", "ILS", "INR", "IQD", "IRR", "ISK", "JMD", "JOD", "JPY", "KES",
    "KGS", "KHR", "KMF", "KPW", "KRW", "KWD", "KYD", "KZT", "LAK", "LBP", "LKR", "LRD", "LSL",
    "LYD", "MAD", "MDL", "MGA", "MKD", "MMK", "MNT", "MOP", "MRU", "MUR", "MVR", "MWK", "MXN",
    "MYR", "MZN", "NAD", "NGN", "NIO", "NOK", "NPR", "NZD", "OMR", "PAB", "PEN", "PGK", "PHP",
    "PKR", "PLN", "PYG", "QAR", "RON", "RSD", "RUB", "RWF", "SAR", "SBD", "SCR", "SDG", "SEK",
    "SGD", "SHP", "SLE", "SOS", "SRD", "SSP", "STN", "SVC", "SYP", "SZL", "THB", "TJS", "TMT",
    "TND", "TOP", "TRY", "TTD", "TWD", "TZS", "UAH", "UGX", "USD", "UYU", "UZS", "VED", "VES",
    "VND", "VUV", "WST", "XAF", "XCD", "XCG", "XOF", "XPF", "YER", "ZAR", "ZMW", "ZWG",
];

const ISO_3166_ALPHA2: &[&str] = &[
    "AD", "AE", "AF", "AG", "AI", "AL", "AM", "AO", "AQ", "AR", "AS", "AT", "AU", "AW", "AX", "AZ",
    "BA", "BB", "BD", "BE", "BF", "BG", "BH", "BI", "BJ", "BL", "BM", "BN", "BO", "BQ", "BR", "BS",
    "BT", "BV", "BW", "BY", "BZ", "CA", "CC", "CD", "CF", "CG", "CH", "CI", "CK", "CL", "CM", "CN",
    "CO", "CR", "CU", "CV", "CW", "CX", "CY", "CZ", "DE", "DJ", "DK", "DM", "DO", "DZ", "EC", "EE",
    "EG", "EH", "ER", "ES", "ET", "FI", "FJ", "FK", "FM", "FO", "FR", "GA", "GB", "GD", "GE", "GF",
    "GG", "GH", "GI", "GL", "GM", "GN", "GP", "GQ", "GR", "GS", "GT", "GU", "GW", "GY", "HK", "HM",
    "HN", "HR", "HT", "HU", "ID", "IE", "IL", "IM", "IN", "IO", "IQ", "IR", "IS", "IT", "JE", "JM",
    "JO", "JP", "KE", "KG", "KH", "KI", "KM", "KN", "KP", "KR", "KW", "KY", "KZ", "LA", "LB", "LC",
    "LI", "LK", "LR", "LS", "LT", "LU", "LV", "LY", "MA", "MC", "MD", "ME", "MF", "MG", "MH", "MK",
    "ML", "MM", "MN", "MO", "MP", "MQ", "MR", "MS", "MT", "MU", "MV", "MW", "MX", "MY", "MZ", "NA",
    "NC", "NE", "NF", "NG", "NI", "NL", "NO", "NP", "NR", "NU", "NZ", "OM", "PA", "PE", "PF", "PG",
    "PH", "PK", "PL", "PM", "PN", "PR", "PS", "PT", "PW", "PY", "QA", "RE", "RO", "RS", "RU", "RW",
    "SA", "SB", "SC", "SD", "SE", "SG", "SH", "SI", "SJ", "SK", "SL", "SM", "SN", "SO", "SR", "SS",
    "ST", "SV", "SX", "SY", "SZ", "TC", "TD", "TF", "TG", "TH", "TJ", "TK", "TL", "TM", "TN", "TO",
    "TR", "TT", "TV", "TW", "TZ", "UA", "UG", "UM", "US", "UY", "UZ", "VA", "VC", "VE", "VG", "VI",
    "VN", "VU", "WF", "WS", "YE", "YT", "ZA", "ZM", "ZW",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        for ok in ["acme", "a", "acme-agency", "a1", "x".repeat(48).as_str()] {
            assert!(validate_slug("slug", ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "-acme",
            "acme-",
            "Acme",
            "acme_agency",
            "acme agency",
            "x".repeat(49).as_str(),
        ] {
            assert!(validate_slug("slug", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn codes_and_zones() {
        assert!(validate_currency("GBP").is_ok());
        assert!(validate_currency("gbp").is_err());
        assert!(validate_currency("XXX").is_err());
        assert!(validate_country("GB").is_ok());
        assert!(validate_country("UK").is_err());
        assert!(validate_timezone("Europe/London").is_ok());
        assert!(validate_timezone("Mars/Olympus").is_err());
        for ok in ["en", "en-GB", "pt-BR", "zh-Hant-TW"] {
            assert!(validate_locale(ok).is_ok(), "{ok}");
        }
        for bad in ["", "EN", "english", "en_GB", "en-"] {
            assert!(validate_locale(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn names_are_trimmed_and_bounded() {
        assert_eq!(validate_name("name", "  Acme  ").expect("valid"), "Acme");
        assert!(validate_name("name", "   ").is_err());
        assert!(validate_name("name", &"x".repeat(201)).is_err());
    }

    #[test]
    fn modules_must_be_known() {
        assert!(validate_modules(&["crm".into(), "portal".into()]).is_ok());
        assert!(validate_modules(&["crm".into(), "ticketing".into()]).is_err());
        assert!(validate_extras(&["team".into()]).is_ok());
        assert!(validate_extras(&["crm".into()]).is_err());
    }
}
