//! Reading contact cards (vCard, `.vcf`), the format Apple Contacts,
//! iCloud, Android, Outlook and most phones export, into the same table a
//! spreadsheet gives, so they import the same way. Versions 2.1, 3.0 and 4.0
//! are read; photos and other attachments are left behind.

use crate::import::{MAX_ROWS, Table};

/// The columns a card becomes.
pub const HEADERS: [&str; 11] = [
    "Name",
    "Organisation",
    "Job title",
    "Email",
    "Phone",
    "Street",
    "City",
    "Region",
    "Postcode",
    "Country",
    "Notes",
];
/// Categories go last, so the list above stays in reading order.
const CATEGORIES: &str = "Categories";

/// Whether the text is contact cards rather than a spreadsheet.
pub fn is_vcard(text: &str) -> bool {
    text.trim_start_matches('\u{feff}')
        .trim_start()
        .get(..11)
        .is_some_and(|s| s.eq_ignore_ascii_case("BEGIN:VCARD"))
}

/// One property line: its name (group taken off, upper case), parameters
/// and raw value.
struct Property<'a> {
    name: String,
    params: Vec<&'a str>,
    value: String,
}

impl Property<'_> {
    fn quoted_printable(&self) -> bool {
        self.params.iter().any(|p| {
            p.eq_ignore_ascii_case("ENCODING=QUOTED-PRINTABLE")
                || p.eq_ignore_ascii_case("QUOTED-PRINTABLE")
        })
    }
}

/// Joins folded lines: a line starting with a space or tab carries on the
/// one before; in version 2.1, so does the line after one ending in a
/// quoted-printable `=`.
fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut soft_break = false;
    for raw in text.lines() {
        if soft_break && let Some(last) = lines.last_mut() {
            last.pop();
            last.push_str(raw);
        } else if let (Some(rest), Some(last)) = (
            raw.strip_prefix(' ').or_else(|| raw.strip_prefix('\t')),
            lines.last_mut(),
        ) {
            last.push_str(rest);
        } else {
            lines.push(raw.to_owned());
        }
        let line = lines.last().map(String::as_str).unwrap_or("");
        let head = line.split(':').next().unwrap_or("").to_ascii_uppercase();
        soft_break = head.contains("QUOTED-PRINTABLE") && line.ends_with('=');
    }
    lines
}

fn parse_line(line: &str) -> Option<Property<'_>> {
    // The name and parameters end at the first colon outside quotes.
    let mut quoted = false;
    let colon = line.char_indices().find_map(|(i, c)| match c {
        '"' => {
            quoted = !quoted;
            None
        }
        ':' if !quoted => Some(i),
        _ => None,
    })?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?;
    // `item1.EMAIL` is EMAIL in a group.
    let name = name
        .rsplit('.')
        .next()
        .unwrap_or(name)
        .trim()
        .to_ascii_uppercase();
    Some(Property {
        name,
        params: parts.collect(),
        value: value.to_owned(),
    })
}

fn decode_quoted_printable(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'='
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Splits a structured value (`N`, `ADR`, `ORG`) on its unescaped `;`
/// and unescapes each part.
fn components(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut part = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('n' | 'N') => part.push('\n'),
                Some(other) => part.push(other),
                None => {}
            },
            ';' => parts.push(std::mem::take(&mut part)),
            c => part.push(c),
        }
    }
    parts.push(part);
    parts.into_iter().map(|p| p.trim().to_owned()).collect()
}

fn text(value: &str) -> String {
    components(value).join(";").trim().to_owned()
}

#[derive(Default)]
struct Card {
    full_name: Option<String>,
    structured_name: Option<String>,
    organisation: Option<String>,
    title: Option<String>,
    emails: Vec<String>,
    phones: Vec<String>,
    address: Option<[String; 5]>,
    notes: Vec<String>,
    categories: Vec<String>,
}

impl Card {
    fn add(&mut self, p: &Property<'_>) {
        let value = if p.quoted_printable() {
            decode_quoted_printable(&p.value)
        } else {
            p.value.clone()
        };
        let set = |slot: &mut Option<String>, v: String| {
            if slot.is_none() && !v.is_empty() {
                *slot = Some(v);
            }
        };
        match p.name.as_str() {
            "FN" => set(&mut self.full_name, text(&value)),
            "N" => {
                // Family; given; additional; prefix; suffix.
                let c = components(&value);
                let get = |i: usize| c.get(i).map(String::as_str).unwrap_or("");
                let name = [get(3), get(1), get(2), get(0), get(4)]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                set(&mut self.structured_name, name);
            }
            "ORG" => {
                let c = components(&value);
                set(
                    &mut self.organisation,
                    c.into_iter().next().unwrap_or_default(),
                );
            }
            "TITLE" => set(&mut self.title, text(&value)),
            "EMAIL" => {
                let v = text(&value);
                if !v.is_empty() && !self.emails.contains(&v) {
                    self.emails.push(v);
                }
            }
            "TEL" => {
                let v = text(&value);
                let v = v.strip_prefix("tel:").unwrap_or(&v).to_owned();
                if !v.is_empty() && !self.phones.contains(&v) {
                    self.phones.push(v);
                }
            }
            "ADR" if self.address.is_none() => {
                // PO box; extended; street; city; region; postcode; country.
                let c = components(&value);
                let get = |i: usize| c.get(i).cloned().unwrap_or_default();
                let street = [get(0), get(1), get(2)]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(", ")
                    .replace('\n', ", ");
                let address = [street, get(3), get(4), get(5), get(6)];
                if address.iter().any(|s| !s.is_empty()) {
                    self.address = Some(address);
                }
            }
            "NOTE" => {
                let v = text(&value);
                if !v.is_empty() {
                    self.notes.push(v);
                }
            }
            "CATEGORIES" => {
                for c in value.split(',') {
                    let c = text(c);
                    if !c.is_empty() && !self.categories.contains(&c) {
                        self.categories.push(c);
                    }
                }
            }
            _ => {}
        }
    }

    fn row(self) -> Option<Vec<String>> {
        let name = self
            .full_name
            .or(self.structured_name)
            .or_else(|| self.organisation.clone())
            .or_else(|| self.emails.first().cloned())
            .unwrap_or_default();
        let [street, city, region, postcode, country] = self.address.unwrap_or_default();
        let row = vec![
            name,
            self.organisation.unwrap_or_default(),
            self.title.unwrap_or_default(),
            self.emails.join("; "),
            self.phones.join("; "),
            street,
            city,
            region,
            postcode,
            country,
            self.notes.join("\n"),
            self.categories.join("; "),
        ];
        row.iter().any(|c| !c.is_empty()).then_some(row)
    }
}

/// Reads contact cards into a table with the columns in [`HEADERS`] and
/// `Categories`. Each card is one row; row numbers count cards.
pub fn parse(text: &str) -> Result<Table, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows = Vec::new();
    let mut card: Option<Card> = None;
    let mut cards = 0usize;
    for line in unfold(text) {
        let Some(p) = parse_line(&line) else { continue };
        match p.name.as_str() {
            "BEGIN" if p.value.trim().eq_ignore_ascii_case("VCARD") => card = Some(Card::default()),
            "END" if p.value.trim().eq_ignore_ascii_case("VCARD") => {
                if let Some(c) = card.take() {
                    cards += 1;
                    if let Some(row) = c.row() {
                        if rows.len() == MAX_ROWS {
                            return Err(format!("the file has more than {MAX_ROWS} contacts"));
                        }
                        rows.push((cards + 1, row));
                    }
                }
            }
            _ => {
                if let Some(c) = card.as_mut() {
                    c.add(&p);
                }
            }
        }
    }
    if cards == 0 {
        return Err("no contact cards were found in the file".into());
    }
    let mut headers: Vec<String> = HEADERS.iter().map(|h| (*h).to_owned()).collect();
    headers.push(CATEGORIES.to_owned());
    let (row_numbers, rows) = rows.into_iter().unzip();
    Ok(Table {
        headers,
        rows,
        row_numbers,
    })
}

/// Writes a table back out as CSV, so cards are kept (and read again) the
/// way a spreadsheet is.
pub fn to_csv(table: &Table) -> Result<String, String> {
    let mut w = csv::Writer::from_writer(Vec::new());
    w.write_record(&table.headers).map_err(|e| e.to_string())?;
    for r in &table.rows {
        w.write_record(r).map_err(|e| e.to_string())?;
    }
    let bytes = w.into_inner().map_err(|e| e.to_string())?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::parse_csv;

    const APPLE: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nPRODID:-//Apple Inc.//macOS 15.0//EN\r\nN:Okafor;Ada;;Dr;\r\nFN:Dr Ada Okafor\r\nORG:Lagos Live;Bookings\r\nTITLE:Head of Music\r\nitem1.EMAIL;type=INTERNET;type=pref:ada@lagoslive.example\r\nEMAIL;type=INTERNET;type=HOME:ada@home.example\r\nTEL;type=CELL;type=VOICE;type=pref:+44 7700 900123\r\nitem2.ADR;type=WORK:;;1 Marina Road;Lagos;;101241;Nigeria\r\nNOTE:Met at Afro Nation\\, Portugal\\nLikes early calls\r\nCATEGORIES:Promoters,VIP\r\nPHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQSkZJRgABAQAAAQABAAD\r\n /2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8U\r\nEND:VCARD\r\nBEGIN:VCARD\r\nVERSION:2.1\r\nN;CHARSET=UTF-8;ENCODING=QUOTED-PRINTABLE:M=C3=BCller;J=C3=BCrgen;;;\r\nTEL;CELL:07700 900456\r\nEND:VCARD\r\nBEGIN:VCARD\r\nVERSION:4.0\r\nFN:\r\nORG:Warehouse Project\r\nEMAIL:info@whp.example\r\nNOTE;ENCODING=QUOTED-PRINTABLE:Line one=0D=0A=\r\nline two\r\nEND:VCARD\r\n";

    #[test]
    fn cards_become_rows() {
        assert!(is_vcard("\u{feff}\r\nbegin:vcard\r\n"));
        assert!(!is_vcard("Name,Email\n"));
        let t = parse(APPLE).expect("parses");
        assert_eq!(t.rows.len(), 3);
        assert_eq!(
            t.rows[0],
            [
                "Dr Ada Okafor",
                "Lagos Live",
                "Head of Music",
                "ada@lagoslive.example; ada@home.example",
                "+44 7700 900123",
                "1 Marina Road",
                "Lagos",
                "",
                "101241",
                "Nigeria",
                "Met at Afro Nation, Portugal\nLikes early calls",
                "Promoters; VIP",
            ]
        );
        // Version 2.1 with quoted-printable names; no FN, so N.
        assert_eq!(t.rows[1][0], "Jürgen Müller");
        assert_eq!(t.rows[1][4], "07700 900456");
        // A company card names itself after the organisation, and a soft
        // line break joins the note's lines.
        assert_eq!(t.rows[2][0], "Warehouse Project");
        assert_eq!(t.rows[2][10], "Line one\r\nline two");
        assert_eq!(t.row_numbers, [2, 3, 4]);
        // No photo data anywhere.
        assert!(t.rows.iter().flatten().all(|c| !c.contains("9j")));
    }

    #[test]
    fn cards_round_trip_through_csv() {
        let t = parse(APPLE).expect("parses");
        let again = parse_csv(&to_csv(&t).expect("writes")).expect("reads");
        assert_eq!(again.headers, t.headers);
        assert_eq!(again.rows[0], t.rows[0]);
        assert!(parse("BEGIN:VCALENDAR\r\nEND:VCALENDAR").is_err());
    }
}
