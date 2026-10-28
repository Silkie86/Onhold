//! The setup questionnaire: plain tick boxes ("what will you use OnHold
//! for?") whose answers decide a workspace's presets, modules, extras and
//! words. Answers are kept, so the form can be run again or edited at any
//! time; nothing is locked in, and switching something off only hides it.

use core_domain::workspace::{EXTRAS, MODULES, Perspective};
use serde::{Deserialize, Serialize};

use crate::presets::{self, Term, Vocabulary};

/// What ticking a feature switches on.
#[derive(Debug, Clone, Copy)]
enum Effect {
    Module(&'static str),
    Extra(&'static str),
}

struct Feature {
    key: &'static str,
    label: &'static str,
    hint: Option<&'static str>,
    /// What it lets them do, shown behind the choice's info button.
    details: &'static str,
    effect: Effect,
}

const FEATURES: &[Feature] = &[
    Feature {
        key: "bookings",
        label: "Take enquiries, hold dates and confirm bookings",
        hint: None,
        details: "Take enquiries from email or add them by hand, and move them from first contact to confirmed. Put dates on hold (first, second and so on) so nothing is double-booked, get warned about clashes, and keep each booking's times, fee, venue, contacts, travel and notes in one place, with tasks and reminders.",
        effect: Effect::Module("bookings"),
    },
    Feature {
        key: "calendar",
        label: "A calendar of what's on and who's free",
        hint: Some("Connect Google or Outlook calendars too"),
        details: "See every hold and confirmed booking on one calendar by month, week or list, and who is free. Connect Google or Outlook so bookings appear in your own calendars, and let talent share their personal calendars (busy times only) so clashes are caught automatically.",
        effect: Effect::Module("calendar"),
    },
    Feature {
        key: "email",
        label: "Read and answer email inside OnHold",
        hint: None,
        details: "Connect Gmail, Outlook or any mailbox and read and answer email inside OnHold. Conversations attach to the right contact and booking, you can turn an email into an enquiry, send texts and WhatsApp messages, use ready-made follow-ups, and send from your own domain with your signature.",
        effect: Effect::Module("messaging"),
    },
    Feature {
        key: "invoicing",
        label: "Send quotes and invoices, and get paid",
        hint: Some("Card payments and reports included"),
        details: "Send quotes that turn into bookings when accepted, raise invoices with VAT, take card payments online, record what's been paid and see what's late. Reports show what you're owed and earned by month, client and talent, and your books can sync to Xero or QuickBooks.",
        effect: Effect::Module("finance"),
    },
    Feature {
        key: "documents",
        label: "Contracts, e-signatures and paperwork",
        hint: None,
        details: "Make contracts, confirmations and advance sheets from a booking in your own look, send them for e-signature and keep everything signed in one place. Old PDFs can be imported and searched, including the words inside them.",
        effect: Effect::Module("documents"),
    },
    Feature {
        key: "events",
        label: "Plan events, tours and line-ups",
        hint: Some("Tours and runs of dates, with budget, crew and day sheets"),
        details: "Run tours and other projects: a run of dates with travel and hotels, a budget against the fees and buyouts, crew, and a day-by-day itinerary, showing whether each date and the whole run pays. Promoters and venues can run their events as projects the same way.",
        effect: Effect::Module("events"),
    },
    Feature {
        key: "portal",
        label: "Give my artists or clients their own login",
        hint: Some("Free for them; they see their dates and documents"),
        details: "Give the artists or people you represent a free login of their own. They see their dates, answer holds and offers, keep their availability and calendar up to date, read their documents and message you, without seeing the rest of your business.",
        effect: Effect::Module("portal"),
    },
    Feature {
        key: "commission",
        label: "Take commission and pay talent their share",
        hint: None,
        details: "Record the commission you take on each booking, decide who invoices the buyer, and see what each artist is owed after commission and costs. Settlements and statements show every artist their share.",
        effect: Effect::Extra("commission"),
    },
    Feature {
        key: "custom",
        label: "Shape it to how I work: custom fields, pipelines and imports",
        hint: None,
        details: "Add your own fields to contacts, artists, enquiries and bookings, change the stages enquiries move through, and import your existing spreadsheets, so OnHold fits how you already work.",
        effect: Effect::Extra("advanced"),
    },
];

/// Presets for people who sell their own time: who or what they book does
/// not rename their own list.
const SELF_EMPLOYED: &[&str] = &["solo", "creative"];

struct Kind {
    key: &'static str,
    label: &'static str,
    one: &'static str,
    many: &'static str,
    music: bool,
}

const KINDS: &[Kind] = &[
    Kind {
        key: "musicians",
        label: "Musicians and bands",
        one: "Artist",
        many: "Artists",
        music: true,
    },
    Kind {
        key: "djs",
        label: "DJs",
        one: "DJ",
        many: "DJs",
        music: true,
    },
    Kind {
        key: "producers",
        label: "Producers",
        one: "Producer",
        many: "Producers",
        music: true,
    },
    Kind {
        key: "speakers",
        label: "Speakers and hosts",
        one: "Speaker",
        many: "Speakers",
        music: false,
    },
    Kind {
        key: "comedians",
        label: "Comedians",
        one: "Comedian",
        many: "Comedians",
        music: false,
    },
    Kind {
        key: "performers",
        label: "Actors, models and other performers",
        one: "Performer",
        many: "Performers",
        music: false,
    },
    Kind {
        key: "creatives",
        label: "Photographers, designers and other creatives",
        one: "Creative",
        many: "Creatives",
        music: false,
    },
    Kind {
        key: "rooms",
        label: "Rooms and spaces",
        one: "Room",
        many: "Rooms",
        music: false,
    },
    Kind {
        key: "equipment",
        label: "Equipment and kit",
        one: "Item",
        many: "Kit",
        music: false,
    },
    Kind {
        key: "crew",
        label: "Crew",
        one: "Crew member",
        many: "Crew",
        music: false,
    },
];

struct Team {
    key: &'static str,
    label: &'static str,
    details: &'static str,
}

const TEAM: &[Team] = &[
    Team {
        key: "just_me",
        label: "Just me",
        details: "The app stays simple for one person: no assigning work to others, and every task is yours.",
    },
    Team {
        key: "small",
        label: "2 to 5 people",
        details: "Work as a team: assign tasks to each other and filter to what's yours, and each person can have their bookings in their own Google or Outlook calendar.",
    },
    Team {
        key: "large",
        label: "6 or more",
        details: "The same team features as 2 to 5 people, priced for a bigger team.",
    },
];
/// One tick box or choice.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Choice {
    pub key: String,
    pub label: String,
    pub hint: Option<String>,
    /// A fuller explanation of what it lets them do, behind an info button.
    pub details: String,
    /// Features worth ticking when this is ticked (a role's usual setup).
    pub suggests: Vec<String>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Question {
    /// The `Answers` field it fills.
    pub key: String,
    pub title: String,
    pub help: Option<String>,
    /// Tick any (true) or pick one (false).
    pub multiple: bool,
    pub choices: Vec<Choice>,
}

fn choice(
    key: &str,
    label: &str,
    hint: Option<&str>,
    details: String,
    suggests: Vec<String>,
) -> Choice {
    Choice {
        key: key.to_owned(),
        label: label.to_owned(),
        hint: hint.map(str::to_owned),
        details,
        suggests,
    }
}

/// What ticking a kind of talent does to the workspace's words.
fn kind_details(k: &Kind) -> String {
    format!(
        "Names your list of who you book {many}, and each one {one}, in the menus and forms. If you tick several kinds, or you run your own bookings, the word that suits your role is kept instead. It only changes words: nothing is switched on or off.",
        many = k.many,
        one = article(k.one),
    )
}

fn article(word: &str) -> String {
    let an = word
        .chars()
        .next()
        .is_some_and(|c| "AEIOU".contains(c.to_ascii_uppercase()));
    format!("{} {word}", if an { "an" } else { "a" })
}

/// The features a preset usually needs.
fn suggested_features(p: &presets::Preset) -> Vec<String> {
    FEATURES
        .iter()
        .filter(|f| match f.effect {
            Effect::Module(m) => p.modules.iter().any(|x| x == m),
            Effect::Extra(e) => p.extras.iter().any(|x| x == e),
        })
        .map(|f| f.key.to_owned())
        .collect()
}

/// The questions, in the order they are asked.
pub fn questions() -> Vec<Question> {
    vec![
        Question {
            key: "roles".into(),
            title: "What do you do?".into(),
            help: Some("Tick everything that fits. Lots of people wear several hats.".into()),
            multiple: true,
            choices: presets::all()
                .iter()
                .map(|p| {
                    choice(
                        p.key,
                        &p.name,
                        None,
                        p.description.clone(),
                        suggested_features(p),
                    )
                })
                .collect(),
        },
        Question {
            key: "features".into(),
            title: "What would you like OnHold to do for you?".into(),
            help: Some(
                "Contacts and your roster are always included. Anything you leave out stays out of your menus, and you can add it later."
                    .into(),
            ),
            multiple: true,
            choices: FEATURES
                .iter()
                .map(|f| choice(f.key, f.label, f.hint, f.details.into(), Vec::new()))
                .collect(),
        },
        Question {
            key: "talent_kinds".into(),
            title: "Who or what do you book?".into(),
            help: Some("Optional. We use it to name things the way you do.".into()),
            multiple: true,
            choices: KINDS
                .iter()
                .map(|k| choice(k.key, k.label, None, kind_details(k), Vec::new()))
                .collect(),
        },
        Question {
            key: "team".into(),
            title: "How many people will use OnHold?".into(),
            help: None,
            multiple: false,
            choices: TEAM
                .iter()
                .map(|t| choice(t.key, t.label, None, t.details.into(), Vec::new()))
                .collect(),
        },
    ]
}

/// What someone ticked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Answers {
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub talent_kinds: Vec<String>,
    /// `just_me`, `small` or `large`.
    #[serde(default)]
    pub team: Option<String>,
}

impl Answers {
    /// Drops repeated ticks, keeping the order they were ticked in.
    pub fn tidy(&mut self) {
        for list in [&mut self.roles, &mut self.features, &mut self.talent_kinds] {
            let mut seen = Vec::with_capacity(list.len());
            list.retain(|x| {
                let fresh = !seen.contains(x);
                if fresh {
                    seen.push(x.clone());
                }
                fresh
            });
        }
    }
}

/// What the answers set up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct Plan {
    pub presets: Vec<String>,
    pub modules: Vec<String>,
    pub extras: Vec<String>,
    /// `None` keeps the workspace's current perspective.
    pub perspective: Option<Perspective>,
    #[schema(value_type = Object)]
    pub vocabulary: Vocabulary,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field}: unknown choice `{value}`")]
pub struct BadAnswer {
    pub field: &'static str,
    pub value: String,
}

fn check(field: &'static str, given: &[String], known: &[&str]) -> Result<(), BadAnswer> {
    match given.iter().find(|g| !known.contains(&g.as_str())) {
        Some(v) => Err(BadAnswer {
            field,
            value: v.clone(),
        }),
        None => Ok(()),
    }
}

/// Turns answers into a setup. The roles' presets give the perspective and
/// words; the ticked features (not the presets) decide what is switched on,
/// so unticking something a role usually has is respected.
pub fn plan(a: &Answers) -> Result<Plan, BadAnswer> {
    let preset_keys: Vec<&str> = presets::all().iter().map(|p| p.key).collect();
    check("roles", &a.roles, &preset_keys)?;
    check(
        "features",
        &a.features,
        &FEATURES.iter().map(|f| f.key).collect::<Vec<_>>(),
    )?;
    check(
        "talent_kinds",
        &a.talent_kinds,
        &KINDS.iter().map(|k| k.key).collect::<Vec<_>>(),
    )?;
    if let Some(t) = &a.team {
        check(
            "team",
            std::slice::from_ref(t),
            &TEAM.iter().map(|t| t.key).collect::<Vec<_>>(),
        )?;
    }
    let combined = presets::combine(&a.roles).map_err(|e| BadAnswer {
        field: "roles",
        value: e.0,
    })?;

    let ticked = || {
        FEATURES
            .iter()
            .filter(|f| a.features.iter().any(|x| x == f.key))
    };
    let mut modules = vec!["crm".to_owned()];
    let mut extras = Vec::new();
    for f in ticked() {
        match f.effect {
            Effect::Module(m) => modules.push(m.to_owned()),
            Effect::Extra(e) => extras.push(e.to_owned()),
        }
    }
    let team = match a.team.as_deref() {
        Some(t) => t != "just_me",
        None => combined.extras.iter().any(|e| e == "team"),
    };
    if team {
        extras.push("team".into());
    }

    let mut vocabulary = combined.vocabulary;
    let own_time =
        !a.roles.is_empty() && a.roles.iter().all(|r| SELF_EMPLOYED.contains(&r.as_str()));
    let kinds: Vec<&Kind> = KINDS
        .iter()
        .filter(|k| a.talent_kinds.iter().any(|x| x == k.key))
        .collect();
    let word = match kinds.as_slice() {
        [one] => Some(Term {
            one: one.one.into(),
            many: one.many.into(),
        }),
        [_, _, ..] if kinds.iter().all(|k| k.music) => Some(Term {
            one: "Artist".into(),
            many: "Artists".into(),
        }),
        _ => None,
    };
    if let Some(word) = word
        && !own_time
    {
        presets::set_term(&mut vocabulary, "crm", "talent", word);
    }

    Ok(Plan {
        presets: combined.presets,
        modules: presets::in_order(MODULES, &modules),
        extras: presets::in_order(EXTRAS, &extras),
        perspective: combined.perspective,
        vocabulary,
    })
}

/// The answers a preset alone would give, to pre-fill the form for a
/// workspace set up from the command line.
pub fn answers_for(preset_keys: &[String]) -> Answers {
    let mut features: Vec<String> = Vec::new();
    for key in preset_keys {
        if let Some(p) = presets::find(key) {
            for f in suggested_features(p) {
                if !features.contains(&f) {
                    features.push(f);
                }
            }
        }
    }
    Answers {
        roles: preset_keys.to_vec(),
        features,
        ..Answers::default()
    }
}

/// The answers that would give what a workspace has switched on now, to
/// pre-fill the form for a workspace made before setups existed.
pub fn answers_from(modules: &[String], extras: &[String]) -> Answers {
    let on = |f: &Feature| match f.effect {
        Effect::Module(m) => modules.iter().any(|x| x == m),
        Effect::Extra(e) => extras.iter().any(|x| x == e),
    };
    let team = if extras.iter().any(|e| e == "team") {
        "small"
    } else {
        "just_me"
    };
    Answers {
        features: FEATURES
            .iter()
            .filter(|f| on(f))
            .map(|f| f.key.to_owned())
            .collect(),
        team: Some(team.into()),
        ..Answers::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answers(roles: &[&str], features: &[&str], kinds: &[&str], team: &str) -> Answers {
        let owned = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect();
        Answers {
            roles: owned(roles),
            features: owned(features),
            talent_kinds: owned(kinds),
            team: Some(team.into()),
        }
    }

    #[test]
    fn a_solo_artist_gets_a_small_app() {
        let p = plan(&answers(
            &["solo"],
            &["bookings", "calendar", "invoicing"],
            &["djs"],
            "just_me",
        ))
        .expect("valid");
        assert_eq!(p.modules, ["crm", "bookings", "calendar", "finance"]);
        assert!(p.extras.is_empty());
        // Their own list is not renamed after who they book.
        assert_eq!(p.vocabulary["crm"]["talent"].many, "My acts");
    }

    #[test]
    fn an_agency_gets_the_full_app() {
        let all: Vec<&str> = FEATURES.iter().map(|f| f.key).collect();
        let p = plan(&answers(&["agency"], &all, &["djs", "producers"], "large")).expect("valid");
        assert_eq!(p.modules, MODULES);
        assert_eq!(p.modules.len(), MODULES.len());
        assert_eq!(p.extras, ["team", "commission", "advanced"]);
        assert_eq!(p.vocabulary["crm"]["talent"].many, "Artists");
        assert_eq!(p.perspective, Some(Perspective::Sells));
    }

    #[test]
    fn unticked_features_stay_off_and_bad_keys_are_refused() {
        let p = plan(&answers(&["agency"], &[], &["speakers"], "small")).expect("valid");
        assert_eq!(p.modules, ["crm"]);
        assert_eq!(p.extras, ["team"]);
        assert_eq!(p.vocabulary["crm"]["talent"].one, "Speaker");
        assert!(plan(&answers(&["wizard"], &[], &[], "small")).is_err());
        assert!(plan(&answers(&[], &["teleport"], &[], "small")).is_err());
        assert!(plan(&answers(&[], &[], &[], "everyone")).is_err());
    }

    #[test]
    fn every_role_suggests_features_and_questions_match_answers() {
        let q = questions();
        assert_eq!(
            q.iter().map(|q| q.key.as_str()).collect::<Vec<_>>(),
            ["roles", "features", "talent_kinds", "team"]
        );
        assert!(q[0].choices.iter().all(|c| !c.suggests.is_empty()));
        // Every choice explains itself behind its info button.
        assert!(
            q.iter()
                .flat_map(|q| &q.choices)
                .all(|c| (40..600).contains(&c.details.len()))
        );
        assert_eq!(
            kind_details(&KINDS[9]),
            "Names your list of who you book Crew, and each one a Crew member, in the menus and forms. If you tick several kinds, or you run your own bookings, the word that suits your role is kept instead. It only changes words: nothing is switched on or off."
        );
        let a = answers_for(&["solo".into()]);
        assert!(a.features.contains(&"invoicing".to_owned()));
        assert!(!a.features.contains(&"commission".to_owned()));
    }

    #[test]
    fn an_older_workspace_keeps_what_it_has_and_repeats_are_dropped() {
        let owned = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let a = answers_from(
            &owned(&["crm", "bookings", "finance"]),
            &owned(&["team", "commission"]),
        );
        assert_eq!(a.features, ["bookings", "invoicing", "commission"]);
        assert_eq!(a.team.as_deref(), Some("small"));
        let p = plan(&a).expect("valid");
        assert_eq!(p.modules, ["crm", "bookings", "finance"]);
        assert_eq!(p.extras, ["team", "commission"]);

        let mut a = answers(
            &["solo", "solo"],
            &["email", "bookings", "email"],
            &[],
            "just_me",
        );
        a.tidy();
        assert_eq!(a.roles, ["solo"]);
        assert_eq!(a.features, ["email", "bookings"]);
    }
}
