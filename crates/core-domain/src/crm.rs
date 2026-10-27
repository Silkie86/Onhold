//! Value types of the CRM: what kind of bookable a talent record is, and the
//! keys that duplicate checks compare.

use serde::{Deserialize, Serialize};

/// What a talent record books: a person, a group (act or crew), a space
/// (room, studio, stage) or equipment.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Default,
    Serialize,
    Deserialize,
    sqlx::Type,
    utoipa::ToSchema,
)]
#[sqlx(type_name = "talent_kind", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum TalentKind {
    #[default]
    Person,
    Act,
    Space,
    Equipment,
}

impl TalentKind {
    pub const ALL: [TalentKind; 4] = [
        TalentKind::Person,
        TalentKind::Act,
        TalentKind::Space,
        TalentKind::Equipment,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TalentKind::Person => "person",
            TalentKind::Act => "act",
            TalentKind::Space => "space",
            TalentKind::Equipment => "equipment",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|k| k.as_str().eq_ignore_ascii_case(s.trim()))
    }
}

/// The part of a phone number duplicate checks compare: its last nine
/// digits, so `+44 7700 900123`, `07700 900123` and `(0)7700-900123` match
/// whatever prefix or punctuation was typed. `None` for fewer than seven
/// digits, which is too little to call two numbers the same.
pub fn phone_key(phone: &str) -> Option<String> {
    let digits: Vec<char> = phone.chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 7 {
        return None;
    }
    let start = digits.len().saturating_sub(9);
    Some(digits[start..].iter().collect())
}

/// How emails are stored and compared: trimmed and lower-cased.
pub fn email_key(email: &str) -> String {
    email.trim().to_lowercase()
}

/// How names are compared for exact duplicates: lower-cased, with runs of
/// spaces and punctuation collapsed to one space.
pub fn name_key(name: &str) -> String {
    name.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_keys_ignore_prefixes_and_punctuation() {
        let k = phone_key("+44 7700 900123");
        assert_eq!(k.as_deref(), Some("700900123"));
        assert_eq!(phone_key("07700 900123"), k);
        assert_eq!(phone_key("(0)7700-900123"), k);
        assert_eq!(phone_key("0044 7700 900 123"), k);
        assert_eq!(phone_key("12345"), None);
        assert_eq!(phone_key("555 0199").as_deref(), Some("5550199"));
    }

    #[test]
    fn names_compare_without_case_or_punctuation() {
        assert_eq!(name_key("  The  Blue-Notes! "), "the blue notes");
        assert_eq!(name_key("O'Brien"), "o brien");
        assert_eq!(name_key("Zoë Ångström"), "zoë ångström");
    }

    #[test]
    fn talent_kinds_parse_loosely() {
        assert_eq!(TalentKind::parse(" Act "), Some(TalentKind::Act));
        assert_eq!(TalentKind::parse("EQUIPMENT"), Some(TalentKind::Equipment));
        assert_eq!(TalentKind::parse("band"), None);
    }
}
