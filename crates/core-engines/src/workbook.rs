//! Reading spreadsheet workbooks (Excel `.xlsx`, `.xls`, `.xlsb` and
//! OpenDocument `.ods`), as many apps export them, into CSV text so they
//! import exactly as a CSV would. The first sheet with anything on it is
//! read; dates become `2027-03-07` (or `2027-03-07 19:30`) and whole
//! numbers lose their `.0`.

use std::io::Cursor;

use calamine::{Data, Reader};

use crate::import::{MAX_COLUMNS, MAX_ROWS};

/// Whether the bytes are a workbook: a zip (`.xlsx`, `.ods`) or an old
/// Office file (`.xls`).
pub fn is_workbook(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(&[0xd0, 0xcf, 0x11, 0xe0])
}

fn cell_text(cell: &Data) -> String {
    match cell {
        Data::Empty | Data::Error(_) => String::new(),
        Data::String(s) => s.trim().to_owned(),
        Data::Int(n) => n.to_string(),
        Data::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{f:.0}"),
        Data::Float(f) => f.to_string(),
        Data::Bool(b) => if *b { "Yes" } else { "No" }.to_owned(),
        Data::DateTime(d) => match d.as_datetime() {
            Some(dt) if dt.time() == chrono::NaiveTime::MIN => dt.format("%Y-%m-%d").to_string(),
            Some(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
            None => String::new(),
        },
        Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
    }
}

/// Reads a workbook's first non-empty sheet as CSV text.
pub fn to_csv(bytes: &[u8]) -> Result<String, String> {
    let mut book = calamine::open_workbook_auto_from_rs(Cursor::new(bytes))
        .map_err(|_| "the file could not be read as a spreadsheet".to_owned())?;
    let names = book.sheet_names();
    let mut range = None;
    for name in &names {
        let r = book
            .worksheet_range(name)
            .map_err(|e| format!("the sheet `{name}` could not be read: {e}"))?;
        if r.cells().any(|(_, _, c)| !matches!(c, Data::Empty)) {
            range = Some(r);
            break;
        }
    }
    let range = range.ok_or_else(|| "the spreadsheet is empty".to_owned())?;
    if range.width() > MAX_COLUMNS {
        return Err(format!("the sheet has more than {MAX_COLUMNS} columns"));
    }
    // One header row and the most data rows an import takes.
    if range.height() > MAX_ROWS + 1 {
        return Err(format!("the sheet has more than {MAX_ROWS} rows"));
    }
    let mut w = csv::Writer::from_writer(Vec::new());
    for row in range.rows() {
        w.write_record(row.iter().map(cell_text))
            .map_err(|e| e.to_string())?;
    }
    let bytes = w.into_inner().map_err(|e| e.to_string())?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::parse_csv;

    #[test]
    fn a_workbook_reads_as_csv() {
        let bytes = include_bytes!("../tests/fixtures/roster.xlsx");
        assert!(is_workbook(bytes));
        assert!(!is_workbook(b"Name,Email\n"));
        let t = parse_csv(&to_csv(bytes).expect("reads")).expect("parses");
        assert_eq!(
            t.headers,
            ["Artist Name", "Genre", "Booking Fee", "Next show", "Active"]
        );
        assert_eq!(
            t.rows[0],
            ["DJ Kemi", "Afro house", "2500", "2027-03-07", "Yes"]
        );
        assert_eq!(
            t.rows[1],
            ["The Lanterns", "Folk", "1250.5", "2027-07-07 19:30", "No"]
        );
        // The empty row is passed over; text is trimmed.
        assert_eq!(t.rows[2][0], "Solo Sam");
        assert_eq!(t.row_numbers, [2, 3, 5]);
        assert!(to_csv(b"PK\x03\x04 not really").is_err());
    }
}
