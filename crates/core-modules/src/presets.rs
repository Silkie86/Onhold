//! Vertical presets: seed bundles kept as data in `presets/*.toml`, so adding
//! a vertical means adding a file. Presets combine: a workspace can use
//! several at once (an artist who also runs a label and promotes nights) and
//! add more later. Everything they seed stays editable afterwards.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use core_domain::workspace::{EXTRAS, MODULES, Perspective};
use serde::{Deserialize, Serialize};

/// A word used in the app, in the singular and the plural.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Term {
    pub one: String,
    pub many: String,
}

/// Words per module, then per term: `crm.talent` = "Artist" / "Artists".
/// Set per module, not per workspace, so each area keeps its own words.
pub type Vocabulary = BTreeMap<String, BTreeMap<String, Term>>;

/// The terms the app knows how to rename, per module.
pub const TERMS: &[(&str, &str)] = &[
    ("crm", "talent"),
    ("bookings", "enquiry"),
    ("bookings", "booking"),
];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    #[serde(skip)]
    pub key: &'static str,
    pub name: String,
    /// What this setup gives, in a few plain sentences, for the setup form.
    pub description: String,
    pub perspective: Perspective,
    pub modules: Vec<String>,
    #[serde(default)]
    pub extras: Vec<String>,
    #[serde(default)]
    pub vocabulary: Vocabulary,
}

const FILES: &[(&str, &str)] = &[
    ("agency", include_str!("../presets/agency.toml")),
    ("manager", include_str!("../presets/manager.toml")),
    ("freelance", include_str!("../presets/freelance.toml")),
    ("solo", include_str!("../presets/solo.toml")),
    ("creative", include_str!("../presets/creative.toml")),
    ("label", include_str!("../presets/label.toml")),
    ("bureau", include_str!("../presets/bureau.toml")),
    (
        "entertainment",
        include_str!("../presets/entertainment.toml"),
    ),
    ("studio", include_str!("../presets/studio.toml")),
    ("production", include_str!("../presets/production.toml")),
    ("hire", include_str!("../presets/hire.toml")),
    ("promoter", include_str!("../presets/promoter.toml")),
    ("venue", include_str!("../presets/venue.toml")),
    ("tour", include_str!("../presets/tour.toml")),
    ("collective", include_str!("../presets/collective.toml")),
    ("tuition", include_str!("../presets/tuition.toml")),
];

/// Every preset, in a fixed order. The files are part of the build and
/// checked by the tests, so a broken one never ships.
pub fn all() -> &'static [Preset] {
    static ALL: OnceLock<Vec<Preset>> = OnceLock::new();
    ALL.get_or_init(|| {
        FILES
            .iter()
            .map(|(key, text)| {
                let mut p: Preset = toml::from_str(text)
                    .unwrap_or_else(|e| panic!("presets/{key}.toml is not valid: {e}"));
                p.key = key;
                p
            })
            .collect()
    })
}

pub fn find(key: &str) -> Option<&'static Preset> {
    all().iter().find(|p| p.key == key)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown preset `{0}`")]
pub struct UnknownPreset(pub String);

/// What a set of presets adds up to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Combined {
    /// Preset keys in the catalogue's order, without repeats.
    pub presets: Vec<String>,
    pub modules: Vec<String>,
    pub extras: Vec<String>,
    /// `None` when no preset was given.
    pub perspective: Option<Perspective>,
    pub vocabulary: Vocabulary,
}

/// Merges presets: modules and extras add up; where perspectives differ the
/// workspace becomes `both`; a word is kept only where the presets that set
/// it agree (an artist who is also a promoter keeps the plain words rather
/// than one side's).
pub fn combine<S: AsRef<str>>(keys: &[S]) -> Result<Combined, UnknownPreset> {
    for k in keys {
        if find(k.as_ref()).is_none() {
            return Err(UnknownPreset(k.as_ref().to_owned()));
        }
    }
    let chosen: Vec<&Preset> = all()
        .iter()
        .filter(|p| keys.iter().any(|k| k.as_ref() == p.key))
        .collect();
    let mut out = Combined {
        presets: chosen.iter().map(|p| p.key.to_owned()).collect(),
        modules: in_order(MODULES, chosen.iter().flat_map(|p| &p.modules)),
        extras: in_order(EXTRAS, chosen.iter().flat_map(|p| &p.extras)),
        ..Combined::default()
    };
    for p in &chosen {
        out.perspective = Some(match out.perspective {
            None => p.perspective,
            Some(seen) if seen == p.perspective => seen,
            Some(_) => Perspective::Both,
        });
    }
    for (module, term) in TERMS {
        let mut words = chosen
            .iter()
            .filter_map(|p| p.vocabulary.get(*module).and_then(|m| m.get(*term)));
        if let Some(first) = words.next()
            && words.all(|w| w == first)
        {
            set_term(&mut out.vocabulary, module, term, first.clone());
        }
    }
    Ok(out)
}

pub fn set_term(vocabulary: &mut Vocabulary, module: &str, term: &str, word: Term) {
    vocabulary
        .entry(module.to_owned())
        .or_default()
        .insert(term.to_owned(), word);
}

/// The known names from `wanted`, in `known`'s order, once each.
pub fn in_order<'a>(known: &[&str], wanted: impl IntoIterator<Item = &'a String>) -> Vec<String> {
    let wanted: Vec<&String> = wanted.into_iter().collect();
    known
        .iter()
        .filter(|k| wanted.iter().any(|w| w == *k))
        .map(|k| (*k).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_file_is_valid() {
        assert_eq!(all().len(), FILES.len());
        for p in all() {
            assert!(!p.name.is_empty(), "{}", p.key);
            assert!(p.modules.iter().any(|m| m == "crm"), "{} keeps crm", p.key);
            core_domain::workspace::validate_modules(&p.modules).expect(p.key);
            core_domain::workspace::validate_extras(&p.extras).expect(p.key);
            for (module, terms) in &p.vocabulary {
                for term in terms.keys() {
                    assert!(
                        TERMS.contains(&(module.as_str(), term.as_str())),
                        "{}: unknown term {module}.{term}",
                        p.key
                    );
                }
            }
        }
    }

    #[test]
    fn presets_combine() {
        let solo = combine(&["solo"]).expect("known");
        assert_eq!(solo.perspective, Some(Perspective::Sells));
        assert!(solo.extras.is_empty());
        assert_eq!(solo.vocabulary["bookings"]["booking"].one, "Gig");

        // An artist who also promotes nights: both sides, every module of
        // either, and plain words where the two disagree.
        let both = combine(&["promoter", "solo"]).expect("known");
        assert_eq!(both.presets, ["solo", "promoter"]);
        assert_eq!(both.perspective, Some(Perspective::Both));
        assert!(both.modules.iter().any(|m| m == "events"));
        assert_eq!(both.extras, ["team"]);
        assert!(
            !both
                .vocabulary
                .get("crm")
                .is_some_and(|m| m.contains_key("talent"))
        );
        assert_eq!(both.vocabulary["bookings"]["booking"].one, "Gig");

        assert_eq!(combine::<&str>(&[]).expect("none").perspective, None);
        assert_eq!(
            combine(&["solo", "wrestling"]),
            Err(UnknownPreset("wrestling".into()))
        );
    }
}
