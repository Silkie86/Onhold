//! Reading Excel workbooks (`.xlsx`), as many apps export them, into CSV
//! text so they import exactly as a CSV would. The first visible sheet with
//! anything on it is read, and every other sheet with the same columns
//! (one sheet per region, say) after it; dates become `2027-03-07` (or
//! `2027-03-07 19:30`), times `19:30`, and whole numbers lose their `.0`.
//! Rows coloured in (green for booked, amber for talking…) keep their
//! colour as a column.
//!
//! A workbook is a zip of XML that the uploader controls, so it is checked
//! before it is read: its unpacked size is capped, the shared-string count
//! it declares (which the reader reserves room for) must be plausible, and
//! cells are streamed one by one, a cell beyond the import's rows or
//! columns refusing the file before anything grows to fit it.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use calamine::{DataRef, Reader, SheetType, SheetVisible, Xlsx};
use quick_xml::events::{BytesStart, Event};
use serde::Serialize;

use crate::import::{MAX_COLUMNS, MAX_ROWS};

/// The most a workbook may unpack to, all its parts together.
pub const MAX_UNPACKED_BYTES: u64 = 64 * 1024 * 1024;
/// The most entries a workbook may hold.
const MAX_ENTRIES: usize = 1_000;
/// The most shared strings a workbook may declare: more than its unpacked
/// size could hold.
const MAX_SHARED_STRINGS: usize = 4_000_000;

/// Whether the bytes are a zip, as an `.xlsx` is.
pub fn is_workbook(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
}

const NOT_A_WORKBOOK: &str = "the file could not be read as an Excel (.xlsx) workbook";

/// Checks the zip before the workbook reader opens it: what it unpacks to
/// is counted as it unpacks (declared sizes can lie), and the shared-string
/// table's declared count is looked at.
fn check_zip(bytes: &[u8]) -> Result<(), String> {
    let mut zip =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| NOT_A_WORKBOOK.to_owned())?;
    if zip.len() > MAX_ENTRIES {
        return Err(NOT_A_WORKBOOK.into());
    }
    let too_big = || {
        format!(
            "the workbook unpacks to more than {} MB; save it as CSV, or split it",
            MAX_UNPACKED_BYTES / 1024 / 1024
        )
    };
    let mut left = MAX_UNPACKED_BYTES;
    for i in 0..zip.len() {
        let entry = zip.by_index(i).map_err(|_| NOT_A_WORKBOOK.to_owned())?;
        let shared = entry
            .name()
            .is_ok_and(|n| n.to_ascii_lowercase().ends_with("sharedstrings.xml"));
        let mut limited = entry.take(left + 1);
        let read = if shared {
            // Only its start is needed to see the declared count.
            let mut head = vec![0u8; 4096];
            let n = read_up_to(&mut limited, &mut head).map_err(|_| NOT_A_WORKBOOK.to_owned())?;
            check_shared_count(&head[..n])?;
            n as u64
                + std::io::copy(&mut limited, &mut std::io::sink())
                    .map_err(|_| NOT_A_WORKBOOK.to_owned())?
        } else {
            std::io::copy(&mut limited, &mut std::io::sink())
                .map_err(|_| NOT_A_WORKBOOK.to_owned())?
        };
        if read > left {
            return Err(too_big());
        }
        left -= read;
    }
    Ok(())
}

fn read_up_to(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

fn check_shared_count(head: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(head);
    let Some(at) = text.find("uniqueCount=") else {
        return Ok(());
    };
    let digits: String = text[at + "uniqueCount=".len()..]
        .chars()
        .skip(1)
        .take_while(char::is_ascii_digit)
        .collect();
    match digits.parse::<usize>() {
        Ok(n) if n <= MAX_SHARED_STRINGS => Ok(()),
        _ => Err(NOT_A_WORKBOOK.into()),
    }
}

/// A number as a spreadsheet shows it: to 15 significant figures, without
/// trailing zeros.
fn number_text(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        let s = format!("{f:.0}");
        return if s == "-0" { "0".into() } else { s };
    }
    let magnitude = f.abs().log10().floor() as i32;
    let decimals = usize::try_from((14 - magnitude).clamp(0, 15)).unwrap_or(0);
    let s = format!("{f:.decimals$}");
    let s = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        s
    };
    if s == "-0" { "0".into() } else { s }
}

fn cell_text(cell: &DataRef<'_>) -> String {
    match cell {
        DataRef::Empty | DataRef::Error(_) => String::new(),
        DataRef::String(s) => s.trim().to_owned(),
        DataRef::SharedString(s) => s.trim().to_owned(),
        DataRef::Int(n) => n.to_string(),
        DataRef::Float(f) => number_text(*f),
        DataRef::Bool(b) => if *b { "Yes" } else { "No" }.to_owned(),
        DataRef::DateTime(d) => {
            let serial = d.as_f64();
            if d.is_duration() || (0.0..1.0).contains(&serial) {
                // A time of day (19:30) or a length of time (26:15).
                let minutes = (serial * 24.0 * 60.0).round() as i64;
                format!("{}:{:02}", minutes / 60, (minutes % 60).abs())
            } else {
                match d.as_datetime() {
                    Some(dt) if dt.time() == chrono::NaiveTime::MIN => {
                        dt.format("%Y-%m-%d").to_string()
                    }
                    Some(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
                    None => number_text(serial),
                }
            }
        }
        DataRef::DateTimeIso(s) | DataRef::DurationIso(s) => s.clone(),
    }
}

/// What reading a workbook gave: the CSV the import reads, and what
/// became of each sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workbook {
    pub csv: String,
    pub sheets: Vec<SheetRead>,
}

/// A sheet with anything on it, and whether its rows were read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SheetRead {
    pub name: String,
    /// Rows under its header.
    pub rows: usize,
    /// Its rows are in the import; a sheet whose columns are unlike the
    /// first sheet's (a summary, a list of notes) is left out.
    pub included: bool,
}

/// The column added for which sheet a row came from, when several are read.
pub const SHEET_COLUMN: &str = "Sheet";
/// The column added for a row's number in its own sheet, when several are
/// read, so a row can be found again.
pub const SHEET_ROW_COLUMN: &str = "Sheet row";
/// The column added for a row's fill colour, when rows are coloured.
pub const COLOUR_COLUMN: &str = "Colour";
/// The most sheets looked at; later ones are not read.
const MAX_SHEETS: usize = 50;
/// The rows above a header that may hold a title or notes, as a CSV may.
const MAX_PREAMBLE: usize = 10;

/// A sheet's header, found without keeping its rows.
struct Scan {
    name: String,
    /// The header's row index (from 0) and its cells.
    header_at: u32,
    header: Vec<String>,
    /// Rows with anything in them under the header.
    rows: usize,
}

/// Reads a workbook's first visible non-empty sheet as CSV text, with the
/// rows of every other sheet laid out the same way (same columns) after
/// it. With several sheets, `Sheet` and `Sheet row` columns say where each
/// row came from; rows filled with a colour get a `Colour` column
/// (`green`, `amber`, `red`, `blue`, `purple` or `grey`).
///
/// Sheets are read twice, cell by cell: once for their headers and sizes,
/// then the ones included straight into the CSV, so no sheet is ever held
/// whole.
pub fn read(bytes: &[u8]) -> Result<Workbook, String> {
    check_zip(bytes)?;
    let mut book: Xlsx<_> = Xlsx::new(Cursor::new(bytes)).map_err(|_| NOT_A_WORKBOOK.to_owned())?;
    let names: Vec<String> = {
        let all = book.sheets_metadata();
        let visible: Vec<String> = all
            .iter()
            .filter(|s| s.typ == SheetType::WorkSheet && s.visible == SheetVisible::Visible)
            .map(|s| s.name.clone())
            .collect();
        if visible.is_empty() {
            all.iter().map(|s| s.name.clone()).collect()
        } else {
            visible
        }
    };
    let mut found: Vec<Scan> = Vec::new();
    for name in names.into_iter().take(MAX_SHEETS) {
        match scan(&mut book, &name) {
            Ok(Some(s)) => found.push(s),
            Ok(None) => {}
            // Only the first sheet with anything on it must be readable;
            // another that is not is left out.
            Err(e) if found.is_empty() => return Err(e),
            Err(_) => {}
        }
    }
    let Some(first) = found.first() else {
        return Err("the spreadsheet is empty".into());
    };
    let first_header = header_keys(&first.header);
    let needed = first_header.len().min(2);
    let sheets: Vec<SheetRead> = found
        .iter()
        .enumerate()
        .map(|(i, s)| SheetRead {
            name: s.name.clone(),
            rows: s.rows,
            included: i == 0 || {
                let keys = header_keys(&s.header);
                keys.iter().filter(|k| first_header.contains(k)).count() >= needed
            },
        })
        .collect();
    let total: usize = sheets.iter().filter(|s| s.included).map(|s| s.rows).sum();
    if total > MAX_ROWS {
        return Err(format!(
            "the sheets have more than {MAX_ROWS} rows together"
        ));
    }
    let wanted: Vec<&Scan> = found
        .iter()
        .zip(&sheets)
        .filter(|(_, s)| s.included)
        .map(|(f, _)| f)
        .collect();
    // Fill colours are a nicety: a workbook whose styles cannot be read
    // still imports, without them.
    let colours = Styles::load(bytes).ok();
    let fills: Vec<HashMap<u32, &'static str>> = wanted
        .iter()
        .map(|s| {
            colours
                .as_ref()
                .and_then(|c| c.row_colours(bytes, &s.name, s.header_at).ok())
                .unwrap_or_default()
        })
        .collect();
    let csv = if let [only] = wanted.as_slice() {
        one_sheet_csv(&mut book, only, &fills[0])?
    } else {
        many_sheets_csv(&mut book, &wanted, &fills)?
    };
    Ok(Workbook { csv, sheets })
}

/// Reads a workbook into the CSV text an import reads.
pub fn to_csv(bytes: &[u8]) -> Result<String, String> {
    read(bytes).map(|w| w.csv)
}

/// A header row's names as compared between sheets.
fn header_keys(header: &[String]) -> Vec<String> {
    header
        .iter()
        .map(|h| crate::import::normalize_header(h))
        .filter(|h| !h.is_empty())
        .collect()
}

fn csv_writer() -> csv::Writer<Vec<u8>> {
    csv::WriterBuilder::new()
        .flexible(true)
        .from_writer(Vec::new())
}

fn finish_csv(w: csv::Writer<Vec<u8>>) -> Result<String, String> {
    let bytes = w.into_inner().map_err(|e| e.to_string())?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

/// Calls `each` with every row of a sheet that has anything in it, in
/// order, with its index (from 0); stops when `each` says so. Cells beyond
/// the import's rows or columns refuse the sheet.
fn each_row(
    book: &mut Xlsx<Cursor<&[u8]>>,
    name: &str,
    mut each: impl FnMut(u32, Vec<String>) -> Result<bool, String>,
) -> Result<(), String> {
    let unreadable = |e: calamine::XlsxError| format!("the sheet `{name}` could not be read: {e}");
    let mut cells = book.worksheet_cells_reader(name).map_err(unreadable)?;
    let mut current: Option<(u32, Vec<String>)> = None;
    while let Some(cell) = cells.next_cell().map_err(unreadable)? {
        let (r, c) = cell.get_position();
        let text = cell_text(cell.get_value());
        if text.is_empty() {
            continue;
        }
        // One header row and the most data rows an import takes.
        if r as usize > MAX_ROWS + MAX_PREAMBLE {
            return Err(format!("the sheet has more than {MAX_ROWS} rows"));
        }
        if c as usize >= MAX_COLUMNS {
            return Err(format!("the sheet has more than {MAX_COLUMNS} columns"));
        }
        match &mut current {
            Some((at, _)) if r < *at => {
                return Err(format!(
                    "the sheet `{name}` could not be read: rows out of order"
                ));
            }
            Some((at, _)) if r == *at => {}
            slot => {
                if let Some((at, row)) = slot.take()
                    && !each(at, row)?
                {
                    return Ok(());
                }
                *slot = Some((r, Vec::new()));
            }
        }
        if let Some((_, row)) = &mut current {
            let c = c as usize;
            if row.len() <= c {
                row.resize(c + 1, String::new());
            }
            row[c] = text;
        }
    }
    if let Some((at, row)) = current {
        each(at, row)?;
    }
    Ok(())
}

/// The header is the first row with two or more cells; rows of one cell
/// above it (a title, a note) are passed over, as in a CSV.
fn is_header(row: &[String]) -> bool {
    row.iter().filter(|c| !c.is_empty()).count() > 1
}

/// A sheet's header and how many rows it has under it; `None` when it is
/// empty.
fn scan(book: &mut Xlsx<Cursor<&[u8]>>, name: &str) -> Result<Option<Scan>, String> {
    let mut first: Option<(u32, Vec<String>)> = None;
    let mut header: Option<(u32, Vec<String>)> = None;
    let mut seen = 0usize;
    let mut rows = 0usize;
    each_row(book, name, |at, row| {
        if header.is_some() {
            rows += 1;
            return Ok(true);
        }
        seen += 1;
        if is_header(&row) {
            header = Some((at, row));
        } else if first.is_none() {
            first = Some((at, row));
        }
        // No header near the top: the first row is the header.
        if header.is_none() && seen > MAX_PREAMBLE {
            header = first.take();
            rows = seen - 1;
        }
        Ok(true)
    })?;
    // A sheet of single cells: its first row is the header.
    if header.is_none() {
        header = first.take();
        rows = seen.saturating_sub(1);
    }
    Ok(header.map(|(header_at, header)| Scan {
        name: name.to_owned(),
        header_at,
        header,
        rows,
    }))
}

/// One sheet, each of its rows one CSV row, blank ones included, so row
/// numbers match what the sheet shows.
fn one_sheet_csv(
    book: &mut Xlsx<Cursor<&[u8]>>,
    sheet: &Scan,
    fills: &HashMap<u32, &str>,
) -> Result<String, String> {
    let width = sheet.header.len();
    // Room for the colour, or none rather than refusing the sheet.
    let coloured = fills.keys().any(|r| *r > sheet.header_at) && width < MAX_COLUMNS;
    let mut w = csv_writer();
    let mut written: u32 = 0;
    each_row(book, &sheet.name, |r, mut cells| {
        while written < r {
            w.write_record([""]).map_err(|e| e.to_string())?;
            written += 1;
        }
        written += 1;
        if r < sheet.header_at {
            // A title or note above the header, a single cell.
            let note = cells
                .into_iter()
                .find(|c| !c.is_empty())
                .unwrap_or_default();
            w.write_record([note]).map_err(|e| e.to_string())?;
            return Ok(true);
        }
        if coloured {
            cells.resize(cells.len().max(width), String::new());
            if r == sheet.header_at {
                cells.push(COLOUR_COLUMN.into());
            } else {
                // Past the header's columns the colour would be lost.
                cells.truncate(width);
                cells.push(fills.get(&r).copied().unwrap_or_default().into());
            }
        }
        w.write_record(&cells).map_err(|e| e.to_string())?;
        Ok(true)
    })?;
    finish_csv(w)
}

/// Several sheets as one table: the columns of all of them (matched by
/// name, a name used twice in a sheet matching the second of that name
/// elsewhere), where each row came from, and the colour when rows are
/// coloured.
fn many_sheets_csv(
    book: &mut Xlsx<Cursor<&[u8]>>,
    sheets: &[&Scan],
    fills: &[HashMap<u32, &str>],
) -> Result<String, String> {
    let mut columns: Vec<String> = Vec::new();
    // (name, its nth use in a sheet) → column.
    let mut slots: HashMap<(String, usize), usize> = HashMap::new();
    // Per sheet, where each of its columns goes.
    let mut places: Vec<Vec<Option<usize>>> = Vec::new();
    for s in sheets {
        let mut place = Vec::with_capacity(s.header.len());
        let mut uses: HashMap<String, usize> = HashMap::new();
        for h in &s.header {
            let key = crate::import::normalize_header(h);
            if key.is_empty() {
                place.push(None);
                continue;
            }
            let n = uses.entry(key.clone()).or_insert(0);
            *n += 1;
            let at = *slots.entry((key, *n)).or_insert_with(|| {
                columns.push(h.clone());
                columns.len() - 1
            });
            if columns.len() + 2 > MAX_COLUMNS {
                return Err(format!(
                    "the sheets have more than {MAX_COLUMNS} columns together"
                ));
            }
            place.push(Some(at));
        }
        places.push(place);
    }
    let coloured = columns.len() + 3 <= MAX_COLUMNS
        && sheets
            .iter()
            .zip(fills)
            .any(|(s, f)| f.keys().any(|r| *r > s.header_at));
    let mut w = csv_writer();
    let mut header = columns.clone();
    header.push(SHEET_COLUMN.into());
    header.push(SHEET_ROW_COLUMN.into());
    if coloured {
        header.push(COLOUR_COLUMN.into());
    }
    w.write_record(&header).map_err(|e| e.to_string())?;
    for ((s, place), f) in sheets.iter().zip(&places).zip(fills) {
        each_row(book, &s.name, |r, cells| {
            if r <= s.header_at {
                return Ok(true);
            }
            let mut out = vec![String::new(); columns.len()];
            for (cell, at) in cells.into_iter().zip(place) {
                if let Some(at) = at {
                    out[*at] = cell;
                }
            }
            out.push(s.name.clone());
            out.push((r + 1).to_string());
            if coloured {
                out.push(f.get(&r).copied().unwrap_or_default().into());
            }
            w.write_record(&out).map_err(|e| e.to_string())?;
            Ok(true)
        })?;
    }
    finish_csv(w)
}

// Fill colours ---------------------------------------------------------------------

/// What the workbook's styles say about fills: the colour each cell style
/// fills with, and where each sheet's cells are kept.
struct Styles {
    /// By cell style (`s="…"`): the colour it fills with, if any.
    fills: Vec<Option<&'static str>>,
    /// Sheet name → its part in the zip.
    paths: HashMap<String, String>,
}

/// The palette `indexed="…"` colours refer to.
const INDEXED: [u32; 64] = [
    0x000000, 0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x000000,
    0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x800000, 0x008000,
    0x000080, 0x808000, 0x800080, 0x008080, 0xC0C0C0, 0x808080, 0x9999FF, 0x993366, 0xFFFFCC,
    0xCCFFFF, 0x660066, 0xFF8080, 0x0066CC, 0xCCCCFF, 0x000080, 0xFF00FF, 0xFFFF00, 0x00FFFF,
    0x800080, 0x800000, 0x008080, 0x0000FF, 0x00CCFF, 0xCCFFFF, 0xCCFFCC, 0xFFFF99, 0x99CCFF,
    0xFF99CC, 0xCC99FF, 0xFFCC99, 0x3366FF, 0x33CCCC, 0x99CC00, 0xFFCC00, 0xFF9900, 0xFF6600,
    0x666699, 0x969696, 0x003366, 0x339966, 0x003300, 0x333300, 0x993300, 0x993366, 0x333399,
    0x333333,
];

/// The name OnHold gives a fill colour: one of its status colours, or
/// `None` for white and near-white (no fill to speak of).
pub fn colour_name(rgb: u32) -> Option<&'static str> {
    let [_, r, g, b] = rgb.to_be_bytes();
    let (r, g, b) = (
        f64::from(r) / 255.0,
        f64::from(g) / 255.0,
        f64::from(b) / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let light = (max + min) / 2.0;
    let delta = max - min;
    if light > 0.94 {
        return None;
    }
    let saturation = if delta == 0.0 {
        0.0
    } else {
        delta / (1.0 - (2.0 * light - 1.0).abs())
    };
    if saturation < 0.15 || delta < 0.06 {
        return Some("grey");
    }
    let hue = if max == r {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    Some(match hue {
        h if !(15.0..330.0).contains(&h) => "red",
        h if h < 70.0 => "amber",
        h if h < 170.0 => "green",
        h if h < 260.0 => "blue",
        _ => "purple",
    })
}

/// A theme colour lightened or darkened by its tint, as Excel does.
fn tinted(rgb: u32, tint: f64) -> u32 {
    if tint == 0.0 {
        return rgb;
    }
    let [_, r, g, b] = rgb.to_be_bytes();
    let shade = |c: u8| -> u32 {
        let c = f64::from(c);
        let v = if tint < 0.0 {
            c * (1.0 + tint)
        } else {
            c + (255.0 - c) * tint
        };
        v.round().clamp(0.0, 255.0) as u32
    };
    (shade(r) << 16) | (shade(g) << 8) | shade(b)
}

/// An attribute's value; `r:id` matches whatever the relationships
/// namespace is called.
fn attr(e: &BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
        let key: &str = a.key.as_ref();
        let wanted =
            key == name || (name == "r:id" && key != "id" && key.rsplit(':').next() == Some("id"));
        wanted
            .then(|| {
                a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                    .ok()
                    .map(|v| v.into_owned())
            })
            .flatten()
    })
}

fn hex(s: &str) -> Option<u32> {
    let s = s.trim();
    // ARGB or RGB.
    let s = if s.len() == 8 { s.get(2..)? } else { s };
    u32::from_str_radix(s, 16).ok().filter(|_| s.len() == 6)
}

/// Reads one part of the workbook with `each` called on every start tag
/// (and end tag, with `true`), stopping early when it says so.
fn walk(
    bytes: &[u8],
    path: &str,
    mut each: impl FnMut(&BytesStart<'_>, bool, bool) -> bool,
) -> Result<(), String> {
    let mut zip =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| NOT_A_WORKBOOK.to_owned())?;
    let entry = zip.by_name(path).map_err(|_| format!("no part {path}"))?;
    let mut reader = quick_xml::Reader::from_reader(std::io::BufReader::new(entry));
    let mut buf = Vec::new();
    loop {
        let go_on = match reader
            .read_event_into(&mut buf)
            .map_err(|e| e.to_string())?
        {
            Event::Start(e) => each(&e, false, false),
            Event::Empty(e) => each(&e, false, true),
            Event::End(e) => each(
                &BytesStart::new(e.local_name().as_ref().to_owned()),
                true,
                false,
            ),
            Event::Eof => return Ok(()),
            _ => true,
        };
        if !go_on {
            return Ok(());
        }
        buf.clear();
    }
}

fn local(e: &BytesStart<'_>) -> String {
    e.local_name().as_ref().to_owned()
}

impl Styles {
    fn load(bytes: &[u8]) -> Result<Self, String> {
        // Which part holds each sheet.
        let mut targets: HashMap<String, String> = HashMap::new();
        walk(bytes, "xl/_rels/workbook.xml.rels", |e, end, _| {
            if !end
                && local(e) == "Relationship"
                && let (Some(id), Some(target)) = (attr(e, "Id"), attr(e, "Target"))
            {
                let path = match target.strip_prefix('/') {
                    Some(abs) => abs.to_owned(),
                    None => format!("xl/{target}"),
                };
                targets.insert(id, path);
            }
            true
        })?;
        let mut paths = HashMap::new();
        walk(bytes, "xl/workbook.xml", |e, end, _| {
            if !end
                && local(e) == "sheet"
                && let (Some(name), Some(id)) = (attr(e, "name"), attr(e, "r:id"))
                && let Some(path) = targets.get(&id)
            {
                paths.insert(name, path.clone());
            }
            true
        })?;
        // The theme's colours, in the order fills refer to them.
        let mut scheme: Vec<u32> = Vec::new();
        let mut in_scheme = false;
        let _ = walk(bytes, "xl/theme/theme1.xml", |e, end, _| {
            match (local(e).as_str(), end) {
                ("clrScheme", false) => in_scheme = true,
                ("clrScheme", true) => return false,
                ("srgbClr", false) if in_scheme => {
                    scheme.push(attr(e, "val").and_then(|v| hex(&v)).unwrap_or(0));
                }
                ("sysClr", false) if in_scheme => {
                    scheme.push(attr(e, "lastClr").and_then(|v| hex(&v)).unwrap_or(0));
                }
                _ => {}
            }
            true
        });
        // Fills refer to light 1 as 0 and dark 1 as 1, the other way round.
        if scheme.len() >= 4 {
            scheme.swap(0, 1);
            scheme.swap(2, 3);
        }
        let colour_of = |e: &BytesStart<'_>| -> Option<u32> {
            if let Some(rgb) = attr(e, "rgb") {
                return hex(&rgb);
            }
            if let Some(theme) = attr(e, "theme").and_then(|t| t.parse::<usize>().ok()) {
                let tint = attr(e, "tint")
                    .and_then(|t| t.parse::<f64>().ok())
                    .unwrap_or(0.0);
                return scheme.get(theme).map(|c| tinted(*c, tint));
            }
            attr(e, "indexed")
                .and_then(|i| i.parse::<usize>().ok())
                .and_then(|i| INDEXED.get(i).copied())
        };
        let mut fill_colours: Vec<Option<&'static str>> = Vec::new();
        let mut xf_fills: Vec<usize> = Vec::new();
        let (mut in_fills, mut in_xfs, mut solid) = (false, false, false);
        walk(bytes, "xl/styles.xml", |e, end, empty| {
            match (local(e).as_str(), end) {
                ("fills", false) => in_fills = true,
                ("fills", true) => in_fills = false,
                ("cellXfs", false) => in_xfs = true,
                ("cellXfs", true) => return false,
                ("fill", false) if in_fills => {
                    fill_colours.push(None);
                    if empty {
                        solid = false;
                    }
                }
                ("patternFill", false) if in_fills => {
                    solid = attr(e, "patternType").as_deref() == Some("solid");
                }
                ("fgColor", false) if in_fills && solid => {
                    if let Some(last) = fill_colours.last_mut() {
                        *last = colour_of(e).and_then(colour_name);
                    }
                }
                ("xf", false) if in_xfs => {
                    xf_fills.push(attr(e, "fillId").and_then(|f| f.parse().ok()).unwrap_or(0));
                }
                _ => {}
            }
            true
        })?;
        Ok(Styles {
            fills: xf_fills
                .into_iter()
                .map(|f| fill_colours.get(f).copied().flatten())
                .collect(),
            paths,
        })
    }

    fn fill(&self, style: Option<String>) -> Option<&'static str> {
        let s: usize = style?.parse().ok()?;
        self.fills.get(s).copied().flatten()
    }

    /// Each row's fill colour by row index (from 0), below `header`: the
    /// row's own fill, else the colour most of its filled cells share.
    fn row_colours(
        &self,
        bytes: &[u8],
        sheet: &str,
        header: u32,
    ) -> Result<HashMap<u32, &'static str>, String> {
        let path = self.paths.get(sheet).ok_or("no such sheet")?;
        let mut out = HashMap::new();
        let mut row: Option<u32> = None;
        let mut row_fill: Option<&'static str> = None;
        let mut counts: Vec<(&'static str, usize)> = Vec::new();
        let mut finish = |row: Option<u32>,
                          row_fill: Option<&'static str>,
                          counts: &mut Vec<(&'static str, usize)>| {
            let best = counts.iter().max_by_key(|(_, n)| *n).map(|(c, _)| *c);
            if let Some(r) = row.filter(|r| *r > header)
                && let Some(c) = row_fill.or(best)
            {
                out.insert(r, c);
            }
            counts.clear();
        };
        walk(bytes, path, |e, end, empty| {
            match (local(e).as_str(), end) {
                ("row", false) => {
                    let r = attr(e, "r")
                        .and_then(|r| r.parse::<u32>().ok())
                        .map(|r| r.saturating_sub(1))
                        .or_else(|| row.map(|r| r + 1))
                        .unwrap_or(0);
                    if r as usize > MAX_ROWS + 1 {
                        return false;
                    }
                    row = Some(r);
                    row_fill = (attr(e, "customFormat").as_deref() == Some("1"))
                        .then(|| self.fill(attr(e, "s")))
                        .flatten();
                    if empty {
                        finish(row, row_fill, &mut counts);
                    }
                }
                ("row", true) => finish(row, row_fill, &mut counts),
                ("c", false) => {
                    if let Some(c) = self.fill(attr(e, "s")) {
                        match counts.iter_mut().find(|(k, _)| *k == c) {
                            Some((_, n)) => *n += 1,
                            None => counts.push((c, 1)),
                        }
                    }
                }
                _ => {}
            }
            true
        })?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::parse_csv;
    use std::io::Write;

    #[test]
    fn a_workbook_reads_as_csv() {
        let bytes = include_bytes!("../tests/fixtures/roster.xlsx");
        assert!(is_workbook(bytes));
        assert!(!is_workbook(b"Name,Email\n"));
        let t = parse_csv(&to_csv(bytes).expect("reads")).expect("parses");
        assert_eq!(
            t.headers,
            [
                "Artist Name",
                "Genre",
                "Booking Fee",
                "Next show",
                "Active",
                "Set time"
            ]
        );
        assert_eq!(
            t.rows[0],
            [
                "DJ Kemi",
                "Afro house",
                "2500",
                "2027-03-07",
                "Yes",
                "19:30"
            ]
        );
        assert_eq!(
            t.rows[1],
            [
                "The Lanterns",
                "Folk",
                "1250.5",
                "2027-07-07 19:30",
                "No",
                ""
            ]
        );
        // The empty row is passed over but keeps the numbering; text is trimmed.
        assert_eq!(t.rows[2][0], "Solo Sam");
        assert_eq!(t.row_numbers, [2, 3, 5]);
        assert!(to_csv(b"PK\x03\x04 not really").is_err());
    }

    #[test]
    fn numbers_read_as_a_spreadsheet_shows_them() {
        assert_eq!(number_text(0.1 + 0.2), "0.3");
        assert_eq!(number_text(-0.0), "0");
        assert_eq!(number_text(1250.5), "1250.5");
        assert_eq!(number_text(3.0), "3");
    }

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in files {
            w.start_file(*name, opts).expect("starts");
            w.write_all(body).expect("writes");
        }
        w.finish().expect("finishes").into_inner()
    }

    #[test]
    fn hostile_workbooks_are_refused_before_they_are_read() {
        // Unpacks far past the cap from a small file.
        let bomb = vec![b'A'; (MAX_UNPACKED_BYTES + 1) as usize];
        let zipped = zip_of(&[("xl/worksheets/sheet1.xml", &bomb)]);
        assert!(zipped.len() < 1024 * 1024);
        assert!(to_csv(&zipped).expect_err("refused").contains("unpacks"));
        // Declares more strings than it could hold.
        let sst = br#"<?xml version="1.0"?><sst xmlns="x" count="1" uniqueCount="99999999999"><si><t>A</t></si></sst>"#;
        let zipped = zip_of(&[("xl/sharedStrings.xml", sst)]);
        assert!(to_csv(&zipped).is_err());
        // A cell far beyond the import's columns.
        let mut far = minimal_book_parts();
        far.push(("xl/worksheets/sheet1.xml", br#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Name</t></is></c><c r="XFD1" t="inlineStr"><is><t>x</t></is></c></row><row r="1048576"><c r="XFD1048576" t="inlineStr"><is><t>x</t></is></c></row></sheetData></worksheet>"#.as_slice()));
        let zipped = zip_of(&far);
        assert!(to_csv(&zipped).expect_err("refused").contains("columns"));
    }

    fn inline_row(r: u32, cells: &[(&str, Option<u32>)]) -> String {
        let mut out = format!(r#"<row r="{r}">"#);
        for (i, (text, style)) in cells.iter().enumerate() {
            let col = (b'A' + i as u8) as char;
            let s = style.map(|s| format!(r#" s="{s}""#)).unwrap_or_default();
            if text.is_empty() {
                out.push_str(&format!(r#"<c r="{col}{r}"{s}/>"#));
            } else {
                out.push_str(&format!(
                    r#"<c r="{col}{r}"{s} t="inlineStr"><is><t>{text}</t></is></c>"#
                ));
            }
        }
        out.push_str("</row>");
        out
    }

    fn sheet_xml(rows: &[String]) -> Vec<u8> {
        format!(
            r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{}</sheetData></worksheet>"#,
            rows.concat()
        )
        .into_bytes()
    }

    #[test]
    fn every_sheet_with_the_same_columns_is_read_with_its_colour() {
        let content_types = br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/worksheets/sheet3.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/></Types>"#;
        let workbook = br#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="UK &amp; Ireland" sheetId="1" r:id="rId1"/><sheet name="Europe" sheetId="2" r:id="rId2"/><sheet name="Notes" sheetId="3" r:id="rId3"/></sheets></workbook>"#;
        let rels = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="/xl/worksheets/sheet2.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet3.xml"/><Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;
        // Styles: 1 green (rgb), 2 amber (indexed 51), 3 red (theme accent 2,
        // lightened), 4 white, 5 a pattern that is not solid.
        let styles = br#"<?xml version="1.0"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fills count="7"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill><fill><patternFill patternType="solid"><fgColor rgb="FF00B050"/></patternFill></fill><fill><patternFill patternType="solid"><fgColor indexed="51"/></patternFill></fill><fill><patternFill patternType="solid"><fgColor theme="5" tint="0.39997558519241921"/></patternFill></fill><fill><patternFill patternType="solid"><fgColor rgb="FFFFFFFF"/></patternFill></fill><fill><patternFill patternType="darkGrid"><fgColor rgb="FF0000FF"/></patternFill></fill></fills><cellStyleXfs count="1"><xf fillId="2"/></cellStyleXfs><cellXfs count="6"><xf fillId="0"/><xf fillId="2"/><xf fillId="3"/><xf fillId="4"/><xf fillId="5"/><xf fillId="6"/></cellXfs><dxfs><dxf><fill><patternFill patternType="solid"><fgColor rgb="FF7030A0"/></patternFill></fill></dxf></dxfs></styleSheet>"#;
        let theme = br#"<?xml version="1.0"?><a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:themeElements><a:clrScheme name="Office"><a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1><a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="44546A"/></a:dk2><a:lt2><a:srgbClr val="E7E6E6"/></a:lt2><a:accent1><a:srgbClr val="4472C4"/></a:accent1><a:accent2><a:srgbClr val="C00000"/></a:accent2></a:clrScheme></a:themeElements></a:theme>"#;
        let uk = sheet_xml(&[
            inline_row(1, &[("Venue", Some(1)), ("City", Some(1)), ("Email", None)]),
            inline_row(
                2,
                &[
                    ("The Grand", Some(1)),
                    ("London", Some(1)),
                    ("a@grand.test", Some(1)),
                ],
            ),
            inline_row(3, &[("Owl Bar", Some(2)), ("Dublin", None), ("", None)]),
            inline_row(
                4,
                &[("Plain Hall", None), ("Leeds", Some(4)), ("", Some(5))],
            ),
        ]);
        let europe = sheet_xml(&[
            // Same columns in another order, with one more.
            inline_row(1, &[("City", None), ("Venue", None), ("Capacity", None)]),
            inline_row(
                2,
                &[("Paris", Some(3)), ("Le Club", Some(3)), ("400", Some(3))],
            ),
        ]);
        let notes = sheet_xml(&[
            inline_row(1, &[("Things to remember", None)]),
            inline_row(2, &[("Call the Grand in May", None)]),
        ]);
        let book = zip_of(&[
            ("[Content_Types].xml", content_types.as_slice()),
            ("_rels/.rels", br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.as_slice()),
            ("xl/workbook.xml", workbook.as_slice()),
            ("xl/_rels/workbook.xml.rels", rels.as_slice()),
            ("xl/styles.xml", styles.as_slice()),
            ("xl/theme/theme1.xml", theme.as_slice()),
            ("xl/worksheets/sheet1.xml", &uk),
            ("xl/worksheets/sheet2.xml", &europe),
            ("xl/worksheets/sheet3.xml", &notes),
        ]);
        let read = read(&book).expect("reads");
        assert_eq!(
            read.sheets,
            [
                SheetRead {
                    name: "UK & Ireland".into(),
                    rows: 3,
                    included: true
                },
                SheetRead {
                    name: "Europe".into(),
                    rows: 1,
                    included: true
                },
                SheetRead {
                    name: "Notes".into(),
                    rows: 1,
                    included: false
                },
            ]
        );
        let t = parse_csv(&read.csv).expect("parses");
        assert_eq!(
            t.headers,
            [
                "Venue",
                "City",
                "Email",
                "Capacity",
                "Sheet",
                "Sheet row",
                "Colour"
            ]
        );
        assert_eq!(
            t.rows[0],
            [
                "The Grand",
                "London",
                "a@grand.test",
                "",
                "UK & Ireland",
                "2",
                "green"
            ]
        );
        assert_eq!(
            t.rows[1],
            ["Owl Bar", "Dublin", "", "", "UK & Ireland", "3", "amber"]
        );
        // White and patterned fills are no colour.
        assert_eq!(
            t.rows[2],
            ["Plain Hall", "Leeds", "", "", "UK & Ireland", "4", ""]
        );
        assert_eq!(
            t.rows[3],
            ["Le Club", "Paris", "", "400", "Europe", "2", "red"]
        );
    }

    #[test]
    fn one_sheet_keeps_its_row_numbers_and_gains_its_colours() {
        let mut parts = minimal_book_parts();
        let styles = br#"<?xml version="1.0"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fills count="3"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill><fill><patternFill patternType="solid"><fgColor rgb="FF00B050"/></patternFill></fill></fills><cellXfs count="2"><xf fillId="0"/><xf fillId="2"/></cellXfs></styleSheet>"#;
        let sheet = sheet_xml(&[
            inline_row(1, &[("Venue", None)]),
            inline_row(2, &[("The Grand", None)]),
            inline_row(4, &[("Owl Bar", Some(1))]),
        ]);
        parts.push(("xl/styles.xml", styles.as_slice()));
        parts.push(("xl/worksheets/sheet1.xml", &sheet));
        let read = read(&zip_of(&parts)).expect("reads");
        let t = parse_csv(&read.csv).expect("parses");
        assert_eq!(t.headers, ["Venue", "Colour"]);
        assert_eq!(t.rows, [vec!["The Grand", ""], vec!["Owl Bar", "green"]]);
        assert_eq!(t.row_numbers, [2, 4]);
        assert_eq!(read.sheets.len(), 1);
    }

    #[test]
    fn fill_colours_get_onholds_names() {
        assert_eq!(colour_name(0x00B050), Some("green"));
        assert_eq!(colour_name(0xC6EFCE), Some("green"));
        assert_eq!(colour_name(0xFFC000), Some("amber"));
        assert_eq!(colour_name(0xFFFF00), Some("amber"));
        assert_eq!(colour_name(0xFF0000), Some("red"));
        assert_eq!(colour_name(0xFFC7CE), Some("red"));
        assert_eq!(colour_name(0x4472C4), Some("blue"));
        assert_eq!(colour_name(0x7030A0), Some("purple"));
        assert_eq!(colour_name(0xD9D9D9), Some("grey"));
        assert_eq!(colour_name(0xFFFFFF), None);
        assert_eq!(tinted(0xC00000, 0.5), 0xE08080);
    }

    /// A workbook of the given sheets, each `(name, sheet xml)`.
    fn book_of(sheets: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut types = String::from(
            r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>"#,
        );
        let mut list = String::new();
        let mut rels = String::from(
            r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        );
        let mut paths = Vec::new();
        for (i, (name, _)) in sheets.iter().enumerate() {
            let n = i + 1;
            types.push_str(&format!(r#"<Override PartName="/xl/worksheets/sheet{n}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#));
            list.push_str(&format!(
                r#"<sheet name="{name}" sheetId="{n}" r:id="rId{n}"/>"#
            ));
            rels.push_str(&format!(r#"<Relationship Id="rId{n}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{n}.xml"/>"#));
            paths.push(format!("xl/worksheets/sheet{n}.xml"));
        }
        types.push_str("</Types>");
        rels.push_str("</Relationships>");
        let workbook = format!(
            r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets>{list}</sheets></workbook>"#
        );
        let root = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
        let mut parts: Vec<(&str, &[u8])> = vec![
            ("[Content_Types].xml", types.as_bytes()),
            ("_rels/.rels", root.as_slice()),
            ("xl/workbook.xml", workbook.as_bytes()),
            ("xl/_rels/workbook.xml.rels", rels.as_bytes()),
        ];
        for (path, (_, xml)) in paths.iter().zip(sheets) {
            parts.push((path, xml));
        }
        zip_of(&parts)
    }

    #[test]
    fn a_title_above_the_header_is_passed_over() {
        let sheet = sheet_xml(&[
            inline_row(1, &[("Pitching 2027", None)]),
            inline_row(3, &[("Venue", None), ("City", None), ("Email", None)]),
            inline_row(
                4,
                &[
                    ("The Grand", None),
                    ("London", None),
                    ("a@grand.test", None),
                ],
            ),
        ]);
        let read = read(&book_of(&[("List", sheet)])).expect("reads");
        assert_eq!(read.sheets[0].rows, 1);
        let t = parse_csv(&read.csv).expect("parses");
        assert_eq!(t.headers, ["Venue", "City", "Email"]);
        assert_eq!(t.rows, [vec!["The Grand", "London", "a@grand.test"]]);
        assert_eq!(t.row_numbers, [4]);
    }

    #[test]
    fn sheets_join_column_by_column_and_odd_sheets_are_left_out() {
        let uk = sheet_xml(&[
            inline_row(1, &[("Venue", None), ("Notes", None), ("Notes", None)]),
            inline_row(
                2,
                &[("The Grand", None), ("called in May", None), ("", None)],
            ),
        ]);
        let eu = sheet_xml(&[
            inline_row(1, &[("Notes", None), ("Venue", None), ("Notes", None)]),
            inline_row(2, &[("first", None), ("Le Club", None), ("second", None)]),
        ]);
        // Far too wide to import, but not one of the list's sheets.
        let mut wide = String::from(r#"<row r="1">"#);
        for c in 0..MAX_COLUMNS + 5 {
            wide.push_str(&format!(
                r#"<c r="{}1" t="inlineStr"><is><t>d{c}</t></is></c>"#,
                column_name(c)
            ));
        }
        wide.push_str("</row>");
        let calendar = sheet_xml(&[wide]);
        let read =
            read(&book_of(&[("UK", uk), ("EU", eu), ("Calendar", calendar)])).expect("reads");
        assert_eq!(read.sheets.len(), 2, "{:?}", read.sheets);
        let t = parse_csv(&read.csv).expect("parses");
        assert_eq!(
            t.headers,
            ["Venue", "Notes", "Notes (2)", "Sheet", "Sheet row"]
        );
        assert_eq!(t.rows[0], ["The Grand", "called in May", "", "UK", "2"]);
        assert_eq!(t.rows[1], ["Le Club", "first", "second", "EU", "2"]);
    }

    fn column_name(mut c: usize) -> String {
        let mut out = Vec::new();
        loop {
            out.push(b'A' + (c % 26) as u8);
            if c < 26 {
                break;
            }
            c = c / 26 - 1;
        }
        out.reverse();
        String::from_utf8(out).expect("ascii")
    }

    #[test]
    fn odd_colour_values_are_no_colour() {
        assert_eq!(hex("a\u{e9}aaaaa"), None);
        assert_eq!(hex("FF00B050"), Some(0x00B050));
    }

    /// The parts a minimal workbook needs, but its sheet.
    fn minimal_book_parts() -> Vec<(&'static str, &'static [u8])> {
        vec![
            ("[Content_Types].xml", br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#.as_slice()),
            ("_rels/.rels", br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.as_slice()),
            ("xl/workbook.xml", br#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#.as_slice()),
            ("xl/_rels/workbook.xml.rels", br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#.as_slice()),
        ]
    }
}
