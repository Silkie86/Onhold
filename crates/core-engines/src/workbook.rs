//! Reading Excel workbooks (`.xlsx`), as many apps export them, into CSV
//! text so they import exactly as a CSV would. The first visible sheet with
//! anything on it is read; dates become `2027-03-07` (or `2027-03-07
//! 19:30`), times `19:30`, and whole numbers lose their `.0`.
//!
//! A workbook is a zip of XML that the uploader controls, so it is checked
//! before it is read: its unpacked size is capped, the shared-string count
//! it declares (which the reader reserves room for) must be plausible, and
//! cells are streamed one by one, a cell beyond the import's rows or
//! columns refusing the file before anything grows to fit it.

use std::io::{Cursor, Read};

use calamine::{DataRef, Reader, SheetType, SheetVisible, Xlsx};

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

/// Reads a workbook's first visible non-empty sheet as CSV text.
pub fn to_csv(bytes: &[u8]) -> Result<String, String> {
    check_zip(bytes)?;
    let mut book: Xlsx<_> = Xlsx::new(Cursor::new(bytes)).map_err(|_| NOT_A_WORKBOOK.to_owned())?;
    let sheets: Vec<String> = {
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
    for name in sheets {
        if let Some(csv) = sheet_csv(&mut book, &name)? {
            return Ok(csv);
        }
    }
    Err("the spreadsheet is empty".into())
}

/// One sheet as CSV, streamed cell by cell; `None` when it is empty. Each
/// spreadsheet row is one CSV row, blank ones included, so row numbers
/// match what the sheet shows.
fn sheet_csv(book: &mut Xlsx<Cursor<&[u8]>>, name: &str) -> Result<Option<String>, String> {
    let mut cells = book
        .worksheet_cells_reader(name)
        .map_err(|e| format!("the sheet `{name}` could not be read: {e}"))?;
    // Rows are as long as their last cell; reading pads them to the header.
    let mut w = csv::WriterBuilder::new()
        .flexible(true)
        .from_writer(Vec::new());
    let mut row: Vec<String> = Vec::new();
    let mut at: Option<u32> = None;
    let mut written: u32 = 0;
    let mut any = false;
    let flush = |w: &mut csv::Writer<Vec<u8>>, row: &mut Vec<String>| -> Result<(), String> {
        if row.is_empty() {
            row.push(String::new());
        }
        w.write_record(row.iter()).map_err(|e| e.to_string())?;
        row.clear();
        Ok(())
    };
    while let Some(cell) = cells
        .next_cell()
        .map_err(|e| format!("the sheet `{name}` could not be read: {e}"))?
    {
        let (r, c) = cell.get_position();
        let text = cell_text(cell.get_value());
        if text.is_empty() {
            continue;
        }
        // One header row and the most data rows an import takes.
        if r as usize > MAX_ROWS {
            return Err(format!("the sheet has more than {MAX_ROWS} rows"));
        }
        if c as usize >= MAX_COLUMNS {
            return Err(format!("the sheet has more than {MAX_COLUMNS} columns"));
        }
        match at {
            Some(current) if r < current => {
                return Err(format!(
                    "the sheet `{name}` could not be read: rows out of order"
                ));
            }
            Some(current) if r == current => {}
            _ => {
                if at.is_some() {
                    flush(&mut w, &mut row)?;
                    written += 1;
                }
                // Blank rows in between (and above the first) keep their place.
                while written < r {
                    flush(&mut w, &mut row)?;
                    written += 1;
                }
                at = Some(r);
            }
        }
        let c = c as usize;
        if row.len() <= c {
            row.resize(c + 1, String::new());
        }
        row[c] = text;
        any = true;
    }
    if !any {
        return Ok(None);
    }
    flush(&mut w, &mut row)?;
    let bytes = w.into_inner().map_err(|e| e.to_string())?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|e| e.to_string())
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
