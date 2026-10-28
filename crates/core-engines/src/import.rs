//! Reading spreadsheets for import: parsing a CSV export (whatever its
//! separator), suggesting which column feeds which field, and turning the
//! text people type into spreadsheets (`£1,250.50`, `07/03/2027`, `Yes`)
//! into typed values. Validation proper is the field engine's job; this
//! only interprets.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::fields::{FieldDef, FieldType};

/// The most rows one import takes.
pub const MAX_ROWS: usize = 50_000;
/// The most columns one import takes.
pub const MAX_COLUMNS: usize = 200;

/// A parsed spreadsheet: its header row and data rows, every row as long as
/// the header.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// The spreadsheet row number of each row (the header is row 1), so
    /// problems point at the right line even after blank rows are skipped.
    pub row_numbers: Vec<usize>,
}

fn detect_delimiter(text: &str) -> u8 {
    first_line_delimiter(text).0
}

/// The separator the first line uses most, with how often it does.
fn first_line_delimiter(text: &str) -> (u8, usize) {
    // Count candidates on the first line, outside quotes.
    let mut counts = [(b',', 0usize), (b';', 0), (b'\t', 0), (b'|', 0)];
    let mut quoted = false;
    for b in text.bytes() {
        match b {
            b'"' => quoted = !quoted,
            b'\n' | b'\r' if !quoted => break,
            _ if !quoted => {
                if let Some(c) = counts.iter_mut().find(|(d, _)| *d == b) {
                    c.1 += 1;
                }
            }
            _ => {}
        }
    }
    // Ties go to the earlier (more common) separator.
    counts.iter().fold(
        (b',', 0),
        |best, &(d, n)| if n > best.1 { (d, n) } else { best },
    )
}

/// Parses a CSV export. The separator (comma, semicolon, tab or pipe) is
/// detected from the header row; a byte-order mark is ignored; notes some
/// apps put above the header (a line or two with one cell, as LinkedIn
/// does) are passed over; blank rows are skipped; blank headers become
/// `Column N` and repeated ones get a number.
pub fn parse_csv(text: &str) -> Result<Table, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    // Notes are rows of one cell with no separator at all; the header is
    // the first row with separators after them. A first row with separators
    // (even `Name,,`) is the header.
    let scan_delimiter = text
        .lines()
        .take(10)
        .map(first_line_delimiter)
        .find(|(_, n)| *n > 0)
        .map_or(b',', |(d, _)| d);
    let reader = || {
        csv::ReaderBuilder::new()
            .delimiter(scan_delimiter)
            .has_headers(false)
            .flexible(true)
            .from_reader(text.as_bytes())
    };
    let preamble = {
        let mut skip = 0;
        for (i, record) in reader().records().take(10).enumerate() {
            match record {
                Ok(r) if r.len() == 1 => {}
                Ok(_) => {
                    skip = i;
                    break;
                }
                Err(_) => break,
            }
        }
        skip
    };
    let (body, header_row) = if preamble == 0 {
        (text, 1)
    } else {
        // Cut at the start of the header row, as the reader saw it.
        let mut reader = reader();
        let mut record = csv::StringRecord::new();
        let mut offset = 0;
        for _ in 0..preamble {
            if !reader.read_record(&mut record).unwrap_or(false) {
                break;
            }
            offset = usize::try_from(reader.position().byte()).unwrap_or(0);
        }
        let body = text.get(offset..).unwrap_or(text);
        // Blank lines are rows in a spreadsheet, though the reader skips
        // them, so they count towards where the header sits.
        let blank_above = text
            .get(..offset)
            .unwrap_or("")
            .lines()
            .filter(|l| l.trim().is_empty())
            .count()
            + body.lines().take_while(|l| l.trim().is_empty()).count();
        (body, preamble + blank_above + 1)
    };
    let delimiter = detect_delimiter(body);
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(body.as_bytes());
    let mut records = reader.records();
    let header = match records.next() {
        Some(Ok(h)) => h,
        Some(Err(e)) => return Err(format!("the header row could not be read: {e}")),
        None => return Err("the file is empty".into()),
    };
    if header.len() > MAX_COLUMNS {
        return Err(format!("the file has more than {MAX_COLUMNS} columns"));
    }
    let mut headers: Vec<String> = Vec::with_capacity(header.len());
    for (i, h) in header.iter().enumerate() {
        let base = match h.trim() {
            "" => format!("Column {}", i + 1),
            h => h.to_owned(),
        };
        let mut name = base.clone();
        let mut n = 2;
        while headers.iter().any(|x| x.eq_ignore_ascii_case(&name)) {
            name = format!("{base} ({n})");
            n += 1;
        }
        headers.push(name);
    }
    if headers.iter().all(|h| h.starts_with("Column ")) {
        return Err("the first row should name the columns".into());
    }
    // Spreadsheet row numbers count the notes above the header too.
    let first = header_row + 1;
    let mut rows = Vec::new();
    let mut row_numbers = Vec::new();
    for (i, record) in records.enumerate() {
        let record = record.map_err(|e| format!("row {} could not be read: {e}", i + first))?;
        if record.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        if rows.len() == MAX_ROWS {
            return Err(format!("the file has more than {MAX_ROWS} rows"));
        }
        let mut row: Vec<String> = record
            .iter()
            .take(headers.len())
            .map(|c| c.trim().to_owned())
            .collect();
        row.resize(headers.len(), String::new());
        rows.push(row);
        row_numbers.push(i + first);
    }
    Ok(Table {
        headers,
        rows,
        row_numbers,
    })
}

/// Lower-case letters and digits only, for comparing headers with field names.
pub fn normalize_header(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// A field a column can feed, with the header spellings that suggest it.
#[derive(Debug, Clone)]
pub struct Target {
    pub key: String,
    pub label: String,
    pub synonyms: Vec<String>,
    /// Spellings that suggest it only when no other column does
    /// (`Agency` names an organisation on a sheet without a `Venue`).
    pub weak_synonyms: Vec<String>,
    /// Several columns may feed it.
    pub many: bool,
}

/// A header with its numbering taken out, so `E-mail 2 - Value` reads as
/// `emailvalue` and `Phone 3` as `phone`.
pub fn unnumbered_header(s: &str) -> String {
    normalize_header(s)
        .chars()
        .filter(|c| !c.is_ascii_digit())
        .collect()
}

/// The target each header most likely feeds, matching the key, the label
/// or a synonym once punctuation and case are ignored, then again with
/// numbering ignored (`Email 2`), then a weak synonym for a field nothing
/// else feeds. A field only one column may feed goes to the closest match,
/// and among equals to the first column.
pub fn suggest_mapping(headers: &[String], targets: &[Target]) -> Vec<Option<String>> {
    let strong = |h: &str| {
        targets
            .iter()
            .find(|t| normalize_header(&t.key) == h || normalize_header(&t.label) == h)
            .or_else(|| {
                targets
                    .iter()
                    .find(|t| t.synonyms.iter().any(|s| normalize_header(s) == h))
            })
    };
    let weak = |h: &str| {
        targets
            .iter()
            .find(|t| t.weak_synonyms.iter().any(|s| normalize_header(s) == h))
    };
    // Each header's best match and how close it is: 0 as written, 1 with
    // numbering ignored, 2 weak.
    let matches: Vec<Option<(&Target, u8)>> = headers
        .iter()
        .map(|h| {
            let exact = normalize_header(h);
            if exact.is_empty() {
                return None;
            }
            let bare = unnumbered_header(h);
            let numbered = bare != exact && !bare.is_empty();
            strong(&exact)
                .map(|t| (t, 0))
                .or_else(|| numbered.then(|| strong(&bare)).flatten().map(|t| (t, 1)))
                .or_else(|| {
                    weak(&exact)
                        .or_else(|| numbered.then(|| weak(&bare)).flatten())
                        .map(|t| (t, 2))
                })
        })
        .collect();
    let mut out: Vec<Option<String>> = vec![None; headers.len()];
    let mut claimed: Vec<&str> = Vec::new();
    for strength in 0..=2u8 {
        for (i, m) in matches.iter().enumerate() {
            let Some((t, s)) = m else { continue };
            if *s != strength {
                continue;
            }
            let taken = claimed.contains(&t.key.as_str());
            // A weak match never joins a field a closer match feeds.
            if taken && (!t.many || strength == 2) {
                continue;
            }
            out[i] = Some(t.key.clone());
            if strength < 2 {
                claimed.push(&t.key);
            }
        }
    }
    out
}

/// Clears all but the first column feeding a field that takes one column.
pub fn once_each(mut columns: Vec<Option<String>>, targets: &[Target]) -> Vec<Option<String>> {
    let mut seen: Vec<String> = Vec::new();
    for c in &mut columns {
        let Some(key) = c else { continue };
        let many = targets
            .iter()
            .find(|t| &t.key == key)
            .is_some_and(|t| t.many);
        if many {
            continue;
        }
        if seen.contains(key) {
            *c = None;
        } else {
            seen.push(key.clone());
        }
    }
    columns
}

/// How to read day/month order in dates like `07/03/2027`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DateOrder {
    /// 07/03/2027 is 7 March.
    #[default]
    Dmy,
    /// 07/03/2027 is 3 July.
    Mdy,
    /// 2027/03/07.
    Ymd,
}

impl DateOrder {
    /// The usual order for a locale: month first in the US and a few
    /// others, year first in East Asia, day first elsewhere.
    pub fn for_locale(locale: &str) -> Self {
        let lower = locale.to_ascii_lowercase();
        let region = lower.split(['-', '_']).nth(1).unwrap_or("");
        let language = lower.split(['-', '_']).next().unwrap_or("");
        if matches!(region, "us" | "ph" | "fm" | "mh" | "pw") {
            DateOrder::Mdy
        } else if matches!(language, "zh" | "ja" | "ko" | "hu" | "lt")
            || matches!(region, "cn" | "jp" | "kr" | "tw")
        {
            DateOrder::Ymd
        } else {
            DateOrder::Dmy
        }
    }
}

/// How to read cell text into typed values.
#[derive(Debug, Clone)]
pub struct Reading {
    pub date_order: DateOrder,
    /// For money cells that name no currency.
    pub default_currency: String,
    /// For date-times without an offset.
    pub timezone: chrono_tz::Tz,
}

/// Reads a number written with either `.` or `,` as the decimal mark and
/// either (or spaces, or `'`) as thousands separators. A lone separator
/// followed by exactly three digits is read as thousands (`1,250`).
pub fn parse_number(s: &str) -> Option<f64> {
    let t: String = s
        .trim()
        .chars()
        .filter(|c| !matches!(c, ' ' | '\u{a0}' | '\u{202f}' | '\''))
        .collect();
    if t.is_empty() {
        return None;
    }
    let (neg, body) = if let Some(inner) = t.strip_prefix('(').and_then(|x| x.strip_suffix(')')) {
        (true, inner.to_owned())
    } else if let Some(rest) = t.strip_prefix('-') {
        (true, rest.to_owned())
    } else {
        (false, t.strip_prefix('+').unwrap_or(&t).to_owned())
    };
    if body.is_empty()
        || !body
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ',')
    {
        return None;
    }
    let last_dot = body.rfind('.');
    let last_comma = body.rfind(',');
    let decimal = match (last_dot, last_comma) {
        (Some(d), Some(c)) => Some(if d > c { '.' } else { ',' }),
        (Some(p), None) | (None, Some(p)) => {
            let sep = body.as_bytes()[p] as char;
            let count = body.matches(sep).count();
            let after = body.len() - p - 1;
            // `1,250` and `1.250.000` group thousands; `12,5` and `3.14` are decimals.
            if count > 1 || (after == 3 && p > 0 && &body[..p] != "0") {
                None
            } else {
                Some(sep)
            }
        }
        (None, None) => None,
    };
    let mut clean = String::with_capacity(body.len());
    for c in body.chars() {
        if Some(c) == decimal {
            clean.push('.');
        } else if c.is_ascii_digit() {
            clean.push(c);
        }
    }
    let n: f64 = clean.parse().ok()?;
    Some(if neg { -n } else { n })
}

// Longer symbols first, so `R$` is not read as `$`.
const SYMBOLS: &[(&str, &str)] = &[
    ("US$", "USD"),
    ("NZ$", "NZD"),
    ("HK$", "HKD"),
    ("A$", "AUD"),
    ("C$", "CAD"),
    ("R$", "BRL"),
    ("zł", "PLN"),
    ("£", "GBP"),
    ("€", "EUR"),
    ("$", "USD"),
    ("¥", "JPY"),
    ("₹", "INR"),
    ("₩", "KRW"),
    ("₦", "NGN"),
];

/// Reads an amount such as `£1,250.50`, `1.250,50 EUR` or `USD 300`. The
/// currency comes from a code or symbol in the text, else `default`.
pub fn parse_money(s: &str, default: &str) -> Result<core_domain::Money, String> {
    let mut text = s.trim().to_owned();
    let mut currency: Option<String> = None;
    // A three-letter code at either end.
    let upper = text.to_ascii_uppercase();
    for part in [upper.get(..3), upper.get(upper.len().saturating_sub(3)..)]
        .into_iter()
        .flatten()
    {
        if part.len() == 3
            && part.bytes().all(|b| b.is_ascii_uppercase())
            && core_domain::workspace::validate_currency(part).is_ok()
        {
            currency = Some(part.to_owned());
            text = upper.replacen(part, "", 1);
            break;
        }
    }
    if currency.is_none() {
        for (sym, code) in SYMBOLS {
            if text.contains(sym) {
                currency = Some((*code).to_owned());
                text = text.replacen(sym, "", 1);
                break;
            }
        }
    }
    let currency = currency.unwrap_or_else(|| default.to_owned());
    let amount = parse_number(&text).ok_or_else(|| format!("`{s}` is not an amount"))?;
    let exp = core_domain::money::minor_unit_exponent(&currency);
    let minor = (amount * 10f64.powi(exp as i32)).round();
    if !minor.is_finite() || minor.abs() > 9.0e15 {
        return Err(format!("`{s}` is too large"));
    }
    core_domain::Money::new(minor as i64, &currency).map_err(|e| e.message)
}

/// Reads a date: ISO (`2027-03-07`), or day, month and year separated by
/// `/`, `-` or `.` in the given order, with two-digit years read as 20xx.
pub fn parse_date(s: &str, order: DateOrder) -> Option<NaiveDate> {
    let s = s.trim();
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(d);
    }
    let parts: Vec<&str> = s.split(['/', '-', '.']).map(str::trim).collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty() || p.len() > 4) {
        return None;
    }
    let nums: Vec<u32> = parts
        .iter()
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    let year = |i: usize| -> Option<i32> {
        let y = nums[i] as i32;
        match parts[i].len() {
            4 => Some(y),
            2 => Some(2000 + y),
            _ => None,
        }
    };
    let (y, m, d) = if parts[0].len() == 4 {
        (year(0)?, nums[1], nums[2])
    } else {
        match order {
            DateOrder::Dmy | DateOrder::Ymd => (year(2)?, nums[1], nums[0]),
            DateOrder::Mdy => (year(2)?, nums[0], nums[1]),
        }
    };
    NaiveDate::from_ymd_opt(y, m, d)
}

/// Reads a date and time: RFC 3339, or a date as [`parse_date`] reads it
/// followed by `HH:MM` or `HH:MM:SS`, in `tz`. A bare date is midnight.
pub fn parse_datetime(
    s: &str,
    order: DateOrder,
    tz: chrono_tz::Tz,
) -> Option<chrono::DateTime<Utc>> {
    let s = s.trim();
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    let (date_part, time_part) = match s.split_once(['T', ' ']) {
        Some((d, t)) => (d, Some(t.trim())),
        None => (s, None),
    };
    let date = parse_date(date_part, order)?;
    let time = match time_part {
        None | Some("") => NaiveTime::MIN,
        Some(t) => NaiveTime::parse_from_str(t, "%H:%M:%S")
            .or_else(|_| NaiveTime::parse_from_str(t, "%H:%M"))
            .ok()?,
    };
    tz.from_local_datetime(&NaiveDateTime::new(date, time))
        .earliest()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Reads yes/no in the forms spreadsheets use.
pub fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_lowercase().as_str() {
        "yes" | "y" | "true" | "t" | "1" | "x" | "✓" | "on" => Some(true),
        "no" | "n" | "false" | "f" | "0" | "off" | "-" => Some(false),
        _ => None,
    }
}

/// Splits a cell holding several values: `a; b`, `a, b`, `a | b`,
/// `a ::: b` (as Google Contacts writes them) or one per line.
pub fn split_list(s: &str) -> Vec<String> {
    if s.contains(":::") {
        return s
            .split(":::")
            .flat_map(|p| p.split('\n'))
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect();
    }
    let sep: &[char] = if s.contains(';') {
        &[';', '\n']
    } else if s.contains('|') {
        &['|', '\n']
    } else {
        &[',', '\n']
    };
    s.split(sep)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A label or group from another app as a tag: Google's own groups
/// (`* myContacts`, `* starred`) are dropped and the `* ` before its other
/// built-in groups is taken off.
pub fn clean_label(s: &str) -> Option<String> {
    let s = s.trim();
    // Mailchimp quotes each tag: `"Customer","VIP"`.
    let s = s
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(s)
        .trim();
    let s = match s.strip_prefix("* ") {
        Some(rest) if matches!(rest.trim(), "myContacts" | "starred") => return None,
        Some(rest) => rest.trim(),
        None => s,
    };
    (!s.is_empty()).then(|| s.to_owned())
}

/// Turns a cell into a custom field's value. Blank is `None` (no value).
/// The result still goes through the field engine's validation.
pub fn coerce(def: &FieldDef, raw: &str, reading: &Reading) -> Result<Option<Value>, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let find_option = |s: &str| {
        def.validation
            .options
            .iter()
            .find(|o| o.value.eq_ignore_ascii_case(s) || o.label.eq_ignore_ascii_case(s))
            .map(|o| o.value.clone())
            .ok_or_else(|| format!("`{s}` is not one of the options"))
    };
    let value = match def.field_type {
        FieldType::Text
        | FieldType::LongText
        | FieldType::Url
        | FieldType::Email
        | FieldType::Phone => json!(raw),
        FieldType::Number => {
            json!(parse_number(raw).ok_or_else(|| format!("`{raw}` is not a number"))?)
        }
        FieldType::Money => {
            let currency = def
                .validation
                .currency
                .as_deref()
                .unwrap_or(&reading.default_currency);
            let m = parse_money(raw, currency)?;
            json!({ "minor": m.minor, "currency": m.currency })
        }
        FieldType::Date => {
            let d = parse_date(raw, reading.date_order)
                .ok_or_else(|| format!("`{raw}` is not a date"))?;
            json!(d.format("%Y-%m-%d").to_string())
        }
        FieldType::Datetime => {
            let dt = parse_datetime(raw, reading.date_order, reading.timezone)
                .ok_or_else(|| format!("`{raw}` is not a date and time"))?;
            json!(dt.to_rfc3339_opts(SecondsFormat::AutoSi, true))
        }
        FieldType::Boolean => {
            json!(parse_bool(raw).ok_or_else(|| format!("`{raw}` is not yes or no"))?)
        }
        FieldType::Select => json!(find_option(raw)?),
        FieldType::MultiSelect => {
            let values: Vec<String> = split_list(raw)
                .iter()
                .map(|s| find_option(s))
                .collect::<Result<_, _>>()?;
            let mut unique = Vec::with_capacity(values.len());
            for v in values {
                if !unique.contains(&v) {
                    unique.push(v);
                }
            }
            json!(unique)
        }
        FieldType::Relation => json!(raw),
        FieldType::File => return Err("files cannot be imported from a spreadsheet".into()),
    };
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fields::{SelectOption, Validation};

    fn def(field_type: FieldType) -> FieldDef {
        FieldDef {
            key: "f".into(),
            label: "F".into(),
            field_type,
            required: false,
            default_value: None,
            validation: Validation {
                options: vec![
                    SelectOption {
                        value: "jazz".into(),
                        label: "Jazz".into(),
                        colour: None,
                    },
                    SelectOption {
                        value: "hip_hop".into(),
                        label: "Hip hop".into(),
                        colour: None,
                    },
                ],
                ..Validation::default()
            },
            archived: false,
        }
    }

    fn reading() -> Reading {
        Reading {
            date_order: DateOrder::Dmy,
            default_currency: "GBP".into(),
            timezone: chrono_tz::Europe::London,
        }
    }

    #[test]
    fn csv_separators_quotes_and_blank_rows() {
        let t = parse_csv("\u{feff}Name;Email;Notes\n\"Smith; Jo\";jo@example.com;\"line one\nline two\"\n;;\nAl;al@example.com\n")
            .expect("parses");
        assert_eq!(t.headers, ["Name", "Email", "Notes"]);
        assert_eq!(t.rows.len(), 2);
        assert_eq!(
            t.rows[0],
            ["Smith; Jo", "jo@example.com", "line one\nline two"]
        );
        assert_eq!(t.rows[1], ["Al", "al@example.com", ""]);
        // Al is on the sheet's fourth row: the header, Jo, the blank row, Al.
        assert_eq!(t.row_numbers, [2, 4]);

        let t = parse_csv("a\tb\n1\t2").expect("tabs");
        assert_eq!(t.rows, [["1", "2"]]);
        let t = parse_csv("Name,,Name\nx,y,z").expect("odd headers");
        assert_eq!(t.headers, ["Name", "Column 2", "Name (2)"]);
        assert!(parse_csv("").is_err());
        assert!(parse_csv(",,\n1,2,3").is_err());
    }

    #[test]
    fn headers_suggest_fields() {
        let targets = vec![
            Target {
                key: "name".into(),
                label: "Name".into(),
                synonyms: vec!["Full name".into()],
                weak_synonyms: vec![],
                many: true,
            },
            Target {
                key: "emails".into(),
                label: "Email".into(),
                synonyms: vec!["E-mail address".into()],
                weak_synonyms: vec![],
                many: true,
            },
            Target {
                key: "custom.genre".into(),
                label: "Genre".into(),
                synonyms: vec![],
                weak_synonyms: vec!["style".into()],
                many: false,
            },
        ];
        let headers: Vec<String> = ["FULL NAME", "e-mail address", "Genre", "Shoe size"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            suggest_mapping(&headers, &targets),
            [
                Some("name".into()),
                Some("emails".into()),
                Some("custom.genre".into()),
                None
            ]
        );
        // Numbered columns find their field; a one-column field goes to the
        // first column only.
        let headers: Vec<String> = ["Email 2", "Genre", "genre"].map(String::from).to_vec();
        assert_eq!(
            suggest_mapping(&headers, &targets),
            [Some("emails".into()), Some("custom.genre".into()), None]
        );
        // An exact match beats an earlier numbered one; a weak synonym only
        // counts when nothing else feeds the field.
        let headers: Vec<String> = ["Genre 2", "Genre", "Style"].map(String::from).to_vec();
        assert_eq!(
            suggest_mapping(&headers, &targets),
            [None, Some("custom.genre".into()), None]
        );
        let headers: Vec<String> = ["Style", "Name"].map(String::from).to_vec();
        assert_eq!(
            suggest_mapping(&headers, &targets),
            [Some("custom.genre".into()), Some("name".into())]
        );
    }

    #[test]
    fn notes_above_the_header_are_passed_over() {
        let t = parse_csv("Notes:\n\"When exporting your connection data, you may notice...\"\n\nFirst Name,Last Name,Email Address\nAda,Okafor,ada@example.com\n")
            .expect("parses");
        assert_eq!(t.headers, ["First Name", "Last Name", "Email Address"]);
        assert_eq!(t.rows, [["Ada", "Okafor", "ada@example.com"]]);
        // As a spreadsheet shows it: notes, note, blank row, header, Ada.
        assert_eq!(t.row_numbers, [5]);
        // Semicolons too, though the notes have none.
        let t = parse_csv("Notes:\nFirst;Last\nA;B\n").expect("semicolons");
        assert_eq!(t.headers, ["First", "Last"]);
        assert_eq!(t.row_numbers, [3]);
        // A header naming one column of several is still the header.
        let t = parse_csv("Name,,\nAda,ada@x.com,123\nBo,bo@x.com,456\n").expect("sparse header");
        assert_eq!(t.headers, ["Name", "Column 2", "Column 3"]);
        assert_eq!(t.rows.len(), 2);
        // One-column files keep their first row as the header.
        let t = parse_csv("Email\na@example.com\nb@example.com").expect("one column");
        assert_eq!(t.headers, ["Email"]);
        assert_eq!(t.rows.len(), 2);
    }

    #[test]
    fn numbers_in_every_local_style() {
        assert_eq!(parse_number("1,250.50"), Some(1250.5));
        assert_eq!(parse_number("1.250,50"), Some(1250.5));
        assert_eq!(parse_number("1 250,5"), Some(1250.5));
        assert_eq!(parse_number("1,250"), Some(1250.0));
        assert_eq!(parse_number("12,5"), Some(12.5));
        assert_eq!(parse_number("0,500"), Some(0.5));
        assert_eq!(parse_number("2.75"), Some(2.75));
        assert_eq!(parse_number("1.000.000"), Some(1_000_000.0));
        assert_eq!(parse_number("(40)"), Some(-40.0));
        assert_eq!(parse_number("-0.5"), Some(-0.5));
        assert_eq!(parse_number("1'000"), Some(1000.0));
        assert_eq!(parse_number("ten"), None);
        assert_eq!(parse_number(""), None);
    }

    #[test]
    fn money_with_symbols_and_codes() {
        let m = parse_money("£1,250.50", "EUR").expect("pounds");
        assert_eq!((m.minor, m.currency.as_str()), (125_050, "GBP"));
        let m = parse_money("1.250,50 EUR", "GBP").expect("euros");
        assert_eq!((m.minor, m.currency.as_str()), (125_050, "EUR"));
        let m = parse_money("usd 300", "GBP").expect("dollars");
        assert_eq!((m.minor, m.currency.as_str()), (30_000, "USD"));
        let m = parse_money("¥5000", "GBP").expect("yen");
        assert_eq!((m.minor, m.currency.as_str()), (5_000, "JPY"));
        let m = parse_money("R$ 10,00", "GBP").expect("reais");
        assert_eq!((m.minor, m.currency.as_str()), (1_000, "BRL"));
        let m = parse_money("750", "GBP").expect("default currency");
        assert_eq!((m.minor, m.currency.as_str()), (75_000, "GBP"));
        assert!(parse_money("lots", "GBP").is_err());
    }

    #[test]
    fn dates_follow_the_given_order() {
        let d = |y, m, d| NaiveDate::from_ymd_opt(y, m, d);
        assert_eq!(parse_date("2027-03-07", DateOrder::Mdy), d(2027, 3, 7));
        assert_eq!(parse_date("07/03/2027", DateOrder::Dmy), d(2027, 3, 7));
        assert_eq!(parse_date("07/03/2027", DateOrder::Mdy), d(2027, 7, 3));
        assert_eq!(parse_date("7.3.27", DateOrder::Dmy), d(2027, 3, 7));
        assert_eq!(parse_date("2027/03/07", DateOrder::Dmy), d(2027, 3, 7));
        assert_eq!(parse_date("31/02/2027", DateOrder::Dmy), None);
        assert_eq!(parse_date("next tuesday", DateOrder::Dmy), None);
        assert_eq!(DateOrder::for_locale("en-US"), DateOrder::Mdy);
        assert_eq!(DateOrder::for_locale("en-GB"), DateOrder::Dmy);
        assert_eq!(DateOrder::for_locale("ja"), DateOrder::Ymd);
        assert_eq!(DateOrder::for_locale("de"), DateOrder::Dmy);
    }

    #[test]
    fn datetimes_are_read_in_the_workspace_timezone() {
        let london = chrono_tz::Europe::London;
        // British Summer Time: 19:30 local is 18:30 UTC.
        let dt = parse_datetime("07/07/2027 19:30", DateOrder::Dmy, london).expect("parses");
        assert_eq!(dt.to_rfc3339(), "2027-07-07T18:30:00+00:00");
        let dt = parse_datetime("2027-07-07T19:30:00Z", DateOrder::Dmy, london).expect("rfc3339");
        assert_eq!(dt.to_rfc3339(), "2027-07-07T19:30:00+00:00");
        assert!(parse_datetime("07/07/2027 7pm", DateOrder::Dmy, london).is_none());
    }

    #[test]
    fn cells_become_custom_values() {
        let r = reading();
        assert_eq!(coerce(&def(FieldType::Text), "  ", &r), Ok(None));
        assert_eq!(
            coerce(&def(FieldType::Number), "1,250", &r),
            Ok(Some(json!(1250.0)))
        );
        assert_eq!(
            coerce(&def(FieldType::Money), "€90", &r),
            Ok(Some(json!({"minor": 9000, "currency": "EUR"})))
        );
        assert_eq!(
            coerce(&def(FieldType::Date), "07/03/2027", &r),
            Ok(Some(json!("2027-03-07")))
        );
        assert_eq!(
            coerce(&def(FieldType::Boolean), "Yes", &r),
            Ok(Some(json!(true)))
        );
        assert_eq!(
            coerce(&def(FieldType::Select), "HIP HOP", &r),
            Ok(Some(json!("hip_hop")))
        );
        assert_eq!(
            coerce(&def(FieldType::MultiSelect), "Jazz; hip_hop; jazz", &r),
            Ok(Some(json!(["jazz", "hip_hop"])))
        );
        assert!(coerce(&def(FieldType::Select), "Polka", &r).is_err());
        assert!(coerce(&def(FieldType::Boolean), "maybe", &r).is_err());
        assert!(coerce(&def(FieldType::File), "cv.pdf", &r).is_err());
    }

    #[test]
    fn lists_split_on_the_separator_used() {
        assert_eq!(split_list("a; b, c ;"), ["a", "b, c"]);
        assert_eq!(split_list("a, b"), ["a", "b"]);
        assert_eq!(split_list("a | b"), ["a", "b"]);
        assert_eq!(split_list("a\nb"), ["a", "b"]);
        assert_eq!(split_list("a@x.com ::: b@y.com"), ["a@x.com", "b@y.com"]);
        assert_eq!(clean_label("* myContacts"), None);
        assert_eq!(clean_label("* friends").as_deref(), Some("friends"));
        assert_eq!(clean_label(" VIP ").as_deref(), Some("VIP"));
        assert_eq!(clean_label("\"Customer\"").as_deref(), Some("Customer"));
    }
}
