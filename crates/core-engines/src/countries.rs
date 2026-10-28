//! Countries, read from however people type them: "UK", "England", "Aus",
//! "NZ/Aus", "Melbourne", "Germnay". Each country has its calling code and a
//! default world region (the split workspaces start from and can change).
//!
//! The tables live in `data/` as plain text, so they are easy to read and
//! correct: `countries.tsv` (ISO 3166 codes with names, aliases, calling
//! codes and regions) and `cities.tsv` (well-known cities, for lists that
//! give a city where the country should be).

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Country {
    /// ISO 3166-1 alpha-2, upper case (and XK for Kosovo).
    pub code: &'static str,
    pub name: &'static str,
    /// Without the plus: "44".
    pub calling_code: &'static str,
    /// What national numbers start with before the area code ("0"), if anything.
    pub trunk_prefix: &'static str,
    /// The key of its region in [`DEFAULT_REGIONS`].
    pub region: &'static str,
}

/// The world split a workspace starts with: (key, name). Every country
/// belongs to exactly one.
pub const DEFAULT_REGIONS: &[(&str, &str)] = &[
    ("europe", "Europe (incl. UK)"),
    ("north_central_america", "North & Central America"),
    ("south_america", "South America"),
    ("oceania", "Oceania"),
    ("asia", "Asia"),
    ("africa", "Africa"),
];

/// How a country was recognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum How {
    /// Its code, name or a usual other name ("UK", "Holland").
    Name,
    /// A city in it ("Melbourne").
    City,
    /// A small misspelling of its name ("Germnay").
    Typo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub country: &'static Country,
    pub how: How,
    /// Other countries the text named too ("NZ/Aus" gives New Zealand, and
    /// Australia here), so the person can choose.
    pub others: Vec<&'static Country>,
}

struct Tables {
    countries: Vec<Country>,
    /// Lookup key → index into `countries`.
    names: HashMap<String, usize>,
    cities: HashMap<String, usize>,
    /// (key, index) of names long enough to forgive a typo in.
    typo_names: Vec<(String, usize)>,
}

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let mut countries = Vec::new();
        let mut names = HashMap::new();
        let mut typo_names = Vec::new();
        for line in include_str!("../data/countries.tsv").lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let cols: Vec<&'static str> = line.split('\t').collect();
            let [code, name, calling_code, trunk_prefix, region, aliases] = cols[..] else {
                panic!("countries.tsv: bad line {line:?}");
            };
            let i = countries.len();
            countries.push(Country {
                code,
                name,
                calling_code,
                trunk_prefix,
                region,
            });
            for n in std::iter::once(name)
                .chain(aliases.split('|'))
                .filter(|a| !a.is_empty())
            {
                let k = key(n);
                if k.chars().count() >= 4 {
                    typo_names.push((k.clone(), i));
                }
                names.entry(k).or_insert(i);
            }
            names.entry(code.to_lowercase()).or_insert(i);
        }
        let mut cities = HashMap::new();
        for line in include_str!("../data/cities.tsv").lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let (city, code) = line.split_once('\t').expect("cities.tsv: city<TAB>code");
            let i = countries
                .iter()
                .position(|c| c.code == code)
                .unwrap_or_else(|| panic!("cities.tsv: unknown country {code}"));
            cities.insert(key(city), i);
        }
        Tables {
            countries,
            names,
            cities,
            typo_names,
        }
    })
}

/// How names are compared: lower case, with every run of anything but
/// letters and digits as one space ("U.K." and "uk" differ; "u k" is both).
fn key(s: &str) -> String {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn all() -> &'static [Country] {
    &tables().countries
}

/// A country by its ISO code, in any case.
pub fn by_code(code: &str) -> Option<&'static Country> {
    let code = code.trim();
    all().iter().find(|c| c.code.eq_ignore_ascii_case(code))
}

/// Edit distance counting a swap of two neighbouring letters as one edit.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, v) in d[0].iter_mut().enumerate() {
        *v = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[a.len()][b.len()]
}

/// One name, without splitting: a country, then a city, then a near miss.
fn find_one(text: &str) -> Option<(usize, How)> {
    let t = tables();
    let mut k = key(text);
    if let Some(rest) = k.strip_prefix("the ") {
        k = rest.to_owned();
    }
    if k.is_empty() {
        return None;
    }
    if let Some(&i) = t.names.get(&k) {
        return Some((i, How::Name));
    }
    if let Some(&i) = t.cities.get(&k) {
        return Some((i, How::City));
    }
    let len = k.chars().count();
    if len < 5 {
        return None;
    }
    let allowed = if len >= 9 { 2 } else { 1 };
    let mut best: Option<(usize, usize)> = None;
    let mut tied = false;
    for (name, i) in &t.typo_names {
        if name.chars().count().abs_diff(len) > allowed {
            continue;
        }
        let d = distance(&k, name);
        if d > allowed {
            continue;
        }
        match best {
            Some((bd, bi)) if d == bd && bi != *i => tied = true,
            Some((bd, _)) if d >= bd => {}
            _ => {
                best = Some((d, *i));
                tied = false;
            }
        }
    }
    match best {
        Some((_, i)) if !tied => Some((i, How::Typo)),
        _ => None,
    }
}

/// The country a piece of text names, if it is clear enough to say.
///
/// The whole text is tried first ("Trinidad and Tobago"), then its parts
/// ("NZ/Aus", "London, UK", "Melbourne (Australia)"): a part naming a
/// country wins over one naming a city, and the countries the other parts
/// name are given as `others`.
pub fn find(text: &str) -> Option<Found> {
    let t = tables();
    if let Some((i, how)) = find_one(text) {
        return Some(Found {
            country: &t.countries[i],
            how,
            others: Vec::new(),
        });
    }
    let lowered = text
        .to_lowercase()
        .replace(" and ", "/")
        .replace(" or ", "/");
    let parts: Vec<(usize, How)> = lowered
        .split(['/', ',', ';', '&', '+', '|', '(', ')', '\\'])
        .filter_map(find_one)
        .collect();
    let rank = |h: How| match h {
        How::Name => 0,
        How::City => 1,
        How::Typo => 2,
    };
    let &(first, how) = parts.iter().min_by_key(|(_, h)| rank(*h))?;
    let mut others = Vec::new();
    for (i, _) in &parts {
        let c = &t.countries[*i];
        if *i != first && !others.contains(&c) {
            others.push(c);
        }
    }
    Some(Found {
        country: &t.countries[first],
        how,
        others,
    })
}

/// A phone number in international form ("+44 7700 900123"), when it can be
/// told for sure: it already starts with + or 00, or it is a national number
/// of `country` that starts with the country's trunk prefix (0), or a
/// ten-digit North American number. Spacing after the code is kept.
pub fn international_phone(phone: &str, country: Option<&str>) -> Option<String> {
    let p = phone.trim();
    if p.starts_with('+') {
        return Some(p.to_owned());
    }
    if let Some(rest) = p.strip_prefix("00") {
        let rest = rest.trim_start();
        return rest
            .starts_with(|c: char| c.is_ascii_digit() && c != '0')
            .then(|| format!("+{rest}"));
    }
    let c = by_code(country?)?;
    let digits: String = p.chars().filter(char::is_ascii_digit).collect();
    if c.calling_code == "1" {
        return (digits.len() == 10 && !digits.starts_with(['0', '1'])).then(|| format!("+1 {p}"));
    }
    if c.trunk_prefix != "0" || digits.len() < 7 {
        return None;
    }
    // Drop the leading 0, which some write as "(0)".
    let national = p.strip_prefix("(0)").or_else(|| p.strip_prefix('0'))?;
    Some(format!("+{} {}", c.calling_code, national.trim_start()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(text: &str) -> Option<&'static str> {
        find(text).map(|f| f.country.code)
    }

    #[test]
    fn every_country_has_a_known_region_and_a_calling_code() {
        let keys: Vec<&str> = DEFAULT_REGIONS.iter().map(|(k, _)| *k).collect();
        assert!(all().len() > 240);
        for c in all() {
            assert!(
                keys.contains(&c.region),
                "{} has region {}",
                c.code,
                c.region
            );
            assert_eq!(c.code.len(), 2);
        }
        assert_eq!(by_code("gb").expect("known").calling_code, "44");
        assert_eq!(
            by_code("US").expect("known").region,
            "north_central_america"
        );
        assert_eq!(by_code("br").expect("known").region, "south_america");
        assert_eq!(by_code("NZ").expect("known").region, "oceania");
        assert_eq!(by_code("GB").expect("known").region, "europe");
    }

    #[test]
    fn messy_country_names_are_recognised() {
        for (text, want) in [
            ("UK", "GB"),
            ("U.K.", "GB"),
            ("England", "GB"),
            ("scotland", "GB"),
            ("  Great Britain ", "GB"),
            ("GB", "GB"),
            ("gbr", "GB"),
            ("USA", "US"),
            ("United States of America", "US"),
            ("Aus", "AU"),
            ("NZ", "NZ"),
            ("Holland", "NL"),
            ("The Netherlands", "NL"),
            ("Deutschland", "DE"),
            ("Czech Republic", "CZ"),
            ("Czechia", "CZ"),
            ("South Korea", "KR"),
            ("Côte d'Ivoire", "CI"),
            ("Trinidad and Tobago", "TT"),
            ("Bosnia and Herzegovina", "BA"),
        ] {
            assert_eq!(code(text), Some(want), "{text}");
            assert_eq!(find(text).expect("known").how, How::Name, "{text}");
        }
    }

    #[test]
    fn cities_and_typos_are_recognised_and_said_so() {
        let melbourne = find("Melbourne").expect("known");
        assert_eq!((melbourne.country.code, melbourne.how), ("AU", How::City));
        assert_eq!(code("new york"), Some("US"));
        assert_eq!(code("München"), Some("DE"));
        let typo = find("Germnay").expect("known");
        assert_eq!((typo.country.code, typo.how), ("DE", How::Typo));
        assert_eq!(code("Austrailia"), Some("AU"));
        assert_eq!(code("Swizerland"), Some("CH"));
        assert_eq!(code("Portgual"), Some("PT"));
        // Austria and Australia are not mixed up.
        assert_eq!(code("Austria"), Some("AT"));
        assert_eq!(code("Australia"), Some("AU"));
    }

    #[test]
    fn several_countries_give_the_first_and_the_rest() {
        let f = find("NZ/Aus").expect("known");
        assert_eq!(f.country.code, "NZ");
        assert_eq!(f.others.iter().map(|c| c.code).collect::<Vec<_>>(), ["AU"]);
        let f = find("London, UK").expect("known");
        // The country part wins over the city part, and both are the same.
        assert_eq!((f.country.code, f.how), ("GB", How::Name));
        assert!(f.others.is_empty());
        let f = find("Melbourne (Aus)").expect("known");
        assert_eq!(f.country.code, "AU");
        let f = find("UK & Ireland").expect("known");
        assert_eq!(f.country.code, "GB");
        assert_eq!(f.others[0].code, "IE");
    }

    #[test]
    fn unclear_text_gives_nothing() {
        for text in [
            "",
            "  ",
            "Europe",
            "worldwide",
            "tbc",
            "n/a",
            "xyz",
            "abcde",
        ] {
            assert_eq!(code(text), None, "{text:?}");
        }
    }

    #[test]
    fn phones_become_international_only_when_certain() {
        assert_eq!(
            international_phone("+44 7700 900123", None).as_deref(),
            Some("+44 7700 900123")
        );
        assert_eq!(
            international_phone("0044 7700 900123", None).as_deref(),
            Some("+44 7700 900123")
        );
        assert_eq!(
            international_phone("07700 900123", Some("GB")).as_deref(),
            Some("+44 7700 900123")
        );
        assert_eq!(
            international_phone("(0)7700 900123", Some("gb")).as_deref(),
            Some("+44 7700 900123")
        );
        assert_eq!(
            international_phone("0412 345 678", Some("AU")).as_deref(),
            Some("+61 412 345 678")
        );
        assert_eq!(
            international_phone("(212) 555-0199", Some("US")).as_deref(),
            Some("+1 (212) 555-0199")
        );
        // No country, or a number that is not national: left alone.
        assert_eq!(international_phone("07700 900123", None), None);
        assert_eq!(international_phone("7700 900123", Some("GB")), None);
        assert_eq!(international_phone("555 0199", Some("US")), None);
        assert_eq!(international_phone("000 123", None), None);
    }
}
