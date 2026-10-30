//! Reading the text layer of a PDF, page by page, laid out the way it looks
//! on paper.
//!
//! Glyph positions come from `pdf-extract`. We rebuild lines ourselves, the
//! way pdfplumber and pdftotext's `-layout` mode do: glyphs on the same
//! baseline make one line, read left to right, and a wide horizontal gap
//! becomes a run of spaces so a label and its value (`Total      £1,200.00`)
//! stay on one line but in visibly separate columns.

use std::panic::{AssertUnwindSafe, catch_unwind};

use pdf_extract::{Document, MediaBox, Object, OutputDev, OutputError, Transform};
use serde::Serialize;

/// Bounds on what one extraction will do, so a hostile or broken upload
/// cannot use unbounded memory.
#[derive(Debug, Clone)]
pub struct Limits {
    pub max_bytes: usize,
    pub max_pages: usize,
    /// Text kept across the whole document; the rest is dropped and
    /// [`Extracted::truncated`] is set.
    pub max_chars: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 50 * 1024 * 1024,
            max_pages: 500,
            max_chars: 2_000_000,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExtractError {
    #[error("the file is larger than {0} bytes")]
    TooLarge(usize),
    #[error("the file is not a PDF")]
    NotPdf,
    #[error("the PDF is password protected")]
    Encrypted,
    #[error("the PDF has more than {0} pages")]
    TooManyPages(usize),
    #[error("the PDF could not be read: {0}")]
    Unreadable(String),
}

/// What a PDF says about itself in its document information.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct Metadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    /// `CreationDate`, as an RFC 3339 date-time when it parses.
    pub created: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct Page {
    /// From 1.
    pub number: u32,
    pub text: String,
    /// Images are drawn on the page but there is (almost) no text: most
    /// likely a scan, which only OCR can read.
    pub needs_ocr: bool,
    /// The text came from OCR, not the PDF's own text layer.
    pub ocr: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct Extracted {
    pub pages: Vec<Page>,
    pub metadata: Metadata,
    /// Text was dropped to stay within [`Limits::max_chars`].
    pub truncated: bool,
}

impl Extracted {
    /// All pages' text, separated by form feeds as pdftotext does.
    pub fn text(&self) -> String {
        self.pages
            .iter()
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\x0c")
    }

    /// Whether any page looks scanned and has not been through OCR.
    pub fn needs_ocr(&self) -> bool {
        self.pages.iter().any(|p| p.needs_ocr && !p.ocr)
    }
}

/// Fewer printable characters than this on a page with images means the
/// page is probably a scan (a stray page number or header doesn't count).
const SCANNED_PAGE_MAX_CHARS: usize = 20;

/// Opens a PDF held in memory, checking the limits and refusing encrypted
/// files that need a password. Files encrypted with an empty user password
/// (common for "no printing" restrictions) open normally.
pub fn load(bytes: &[u8], limits: &Limits) -> Result<Document, ExtractError> {
    if bytes.len() > limits.max_bytes {
        return Err(ExtractError::TooLarge(limits.max_bytes));
    }
    // The header may sit after some junk, but within the first kilobyte.
    if !bytes[..bytes.len().min(1024)]
        .windows(5)
        .any(|w| w == b"%PDF-")
    {
        return Err(ExtractError::NotPdf);
    }
    let doc = guarded(|| {
        let mut doc =
            Document::load_mem(bytes).map_err(|e| ExtractError::Unreadable(e.to_string()))?;
        if doc.is_encrypted() && doc.decrypt("").is_err() {
            return Err(ExtractError::Encrypted);
        }
        Ok(doc)
    })?;
    let pages = doc.get_pages().len();
    if pages > limits.max_pages {
        return Err(ExtractError::TooManyPages(limits.max_pages));
    }
    Ok(doc)
}

/// Extracts every page's text and the document information.
///
/// Parsing is CPU-bound and can be slow on large files: callers on an async
/// runtime should run it on a blocking thread with a timeout.
pub fn extract(bytes: &[u8], limits: &Limits) -> Result<Extracted, ExtractError> {
    let doc = load(bytes, limits)?;
    let metadata = guarded(|| Ok(metadata(&doc))).unwrap_or_default();
    let mut pages = Vec::new();
    let mut budget = limits.max_chars;
    let mut truncated = false;
    for (number, id) in doc.get_pages() {
        // One bad page should not cost the others: a page that fails to
        // parse is kept, empty, and offered to OCR if it has images.
        let text = guarded(|| {
            let mut layout = Layout::default();
            pdf_extract::output_doc_page(&doc, &mut layout, number)
                .map_err(|e| ExtractError::Unreadable(e.to_string()))?;
            Ok(layout.render())
        })
        .unwrap_or_default();
        let mut text = text;
        if text.len() > budget {
            let mut cut = budget;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            truncated = true;
        }
        budget -= text.len();
        let printable = text.chars().filter(|c| !c.is_whitespace()).count();
        let has_images = guarded(|| {
            Ok(doc
                .get_page_images(id)
                .map(|i| !i.is_empty())
                .unwrap_or(false))
        })
        .unwrap_or(false);
        pages.push(Page {
            number,
            needs_ocr: has_images && printable < SCANNED_PAGE_MAX_CHARS,
            text,
            ocr: false,
        });
    }
    Ok(Extracted {
        pages,
        metadata,
        truncated,
    })
}

/// Runs `f`, turning a panic inside the PDF libraries (they have a few on
/// malformed input) into an error instead of taking the worker down.
pub(crate) fn guarded<T>(f: impl FnOnce() -> Result<T, ExtractError>) -> Result<T, ExtractError> {
    catch_unwind(AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(ExtractError::Unreadable("the PDF is malformed".into())))
}

fn metadata(doc: &Document) -> Metadata {
    let info = doc.trailer.get(b"Info").ok().and_then(|o| match o {
        Object::Reference(id) => doc.get_dictionary(*id).ok(),
        Object::Dictionary(d) => Some(d),
        _ => None,
    });
    let Some(info) = info else {
        return Metadata::default();
    };
    let get = |key: &[u8]| -> Option<String> {
        let o = info.get(key).ok()?;
        let o = match o {
            Object::Reference(id) => doc.get_object(*id).ok()?,
            o => o,
        };
        let s = decode_text_string(o.as_str().ok()?);
        let s = s.trim();
        (!s.is_empty()).then(|| s.to_owned())
    };
    Metadata {
        title: get(b"Title"),
        author: get(b"Author"),
        subject: get(b"Subject"),
        creator: get(b"Creator"),
        producer: get(b"Producer"),
        created: get(b"CreationDate").and_then(|d| parse_pdf_date(&d)),
    }
}

/// Decodes a PDF text string: UTF-16BE with a byte-order mark, UTF-8 with
/// one (PDF 2.0), else PDFDocEncoding, which matches Latin-1 for the
/// characters that matter here.
pub(crate) fn decode_text_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        String::from_utf16_lossy(&units)
    } else if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        bytes.iter().map(|&b| b as char).collect()
    }
}

/// Reads a PDF date (`D:20240307143000+01'00'`, any tail optional) as RFC
/// 3339.
pub(crate) fn parse_pdf_date(s: &str) -> Option<String> {
    let s = s.strip_prefix("D:").unwrap_or(s);
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    if digits.len() < 4 {
        return None;
    }
    let part = |from: usize, len: usize, default: u32| -> Option<u32> {
        match digits.get(from..from + len) {
            Some(p) => p.parse().ok(),
            None => Some(default),
        }
    };
    let year: i32 = digits[..4].parse().ok()?;
    let date = chrono::NaiveDate::from_ymd_opt(year, part(4, 2, 1)?, part(6, 2, 1)?)?;
    let time = chrono::NaiveTime::from_hms_opt(part(8, 2, 0)?, part(10, 2, 0)?, part(12, 2, 0)?)?;
    let tail = &s[digits.len()..];
    let offset_minutes = match tail.chars().next() {
        Some(sign @ ('+' | '-')) => {
            let nums: Vec<i32> = tail[1..]
                .split(|c: char| !c.is_ascii_digit())
                .filter(|p| !p.is_empty())
                .take(2)
                .filter_map(|p| p.parse().ok())
                .collect();
            let m = nums.first().copied().unwrap_or(0) * 60 + nums.get(1).copied().unwrap_or(0);
            if sign == '-' { -m } else { m }
        }
        _ => 0,
    };
    let offset = chrono::FixedOffset::east_opt(offset_minutes * 60)?;
    let dt = chrono::NaiveDateTime::new(date, time)
        .and_local_timezone(offset)
        .single()?;
    Some(dt.to_rfc3339())
}

#[derive(Debug, Clone)]
struct Glyph {
    x: f64,
    /// From the top of the page, so lines sort top to bottom.
    y: f64,
    end: f64,
    size: f64,
    text: String,
}

/// An [`OutputDev`] that records glyph positions for one page and lays them
/// out as lines.
#[derive(Default)]
struct Layout {
    height: f64,
    glyphs: Vec<Glyph>,
}

impl OutputDev for Layout {
    fn begin_page(
        &mut self,
        _page_num: u32,
        media_box: &MediaBox,
        _art_box: Option<(f64, f64, f64, f64)>,
    ) -> Result<(), OutputError> {
        self.height = media_box.ury - media_box.lly;
        self.glyphs.clear();
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), OutputError> {
        Ok(())
    }

    fn output_character(
        &mut self,
        trm: &Transform,
        width: f64,
        _spacing: f64,
        font_size: f64,
        char: &str,
    ) -> Result<(), OutputError> {
        // The text rendering matrix scales the font size; take the side of
        // the square with the same area, as pdf-extract does.
        let sx = font_size * trm.m11 + font_size * trm.m21;
        let sy = font_size * trm.m12 + font_size * trm.m22;
        let size = (sx * sy).abs().sqrt().max(1.0);
        let x = trm.m31;
        self.glyphs.push(Glyph {
            x,
            y: self.height - trm.m32,
            end: x + width * size,
            size,
            text: normalise(char),
        });
        Ok(())
    }

    fn begin_word(&mut self) -> Result<(), OutputError> {
        Ok(())
    }

    fn end_word(&mut self) -> Result<(), OutputError> {
        Ok(())
    }

    fn end_line(&mut self) -> Result<(), OutputError> {
        Ok(())
    }
}

/// Spells out ligatures and turns odd spaces into plain ones, so patterns
/// match `Office` whether or not the font used an `ﬃ` glyph.
fn normalise(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'ﬀ' => out.push_str("ff"),
            'ﬁ' => out.push_str("fi"),
            'ﬂ' => out.push_str("fl"),
            'ﬃ' => out.push_str("ffi"),
            'ﬄ' => out.push_str("ffl"),
            'ﬅ' | 'ﬆ' => out.push_str("st"),
            '\u{a0}' | '\u{2007}' | '\u{202f}' | '\u{2009}' | '\t' => out.push(' '),
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2212}' => out.push('-'),
            '\u{0}'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{ad}' => {}
            c => out.push(c),
        }
    }
    out
}

impl Layout {
    fn render(&self) -> String {
        let mut glyphs: Vec<&Glyph> = self.glyphs.iter().filter(|g| !g.text.is_empty()).collect();
        glyphs.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x.total_cmp(&b.x)));

        // Group into lines: a glyph joins the current line when its
        // baseline is within half a font size of the line's first glyph.
        let mut lines: Vec<Vec<&Glyph>> = Vec::new();
        for g in glyphs {
            match lines.last_mut() {
                Some(line) if (g.y - line[0].y).abs() <= 0.5 * g.size.min(line[0].size) => {
                    line.push(g);
                }
                _ => lines.push(vec![g]),
            }
        }

        let mut out = String::new();
        let mut last_y: Option<(f64, f64)> = None;
        for line in &mut lines {
            line.sort_by(|a, b| a.x.total_cmp(&b.x));
            let size = line.iter().map(|g| g.size).fold(0.0, f64::max);
            if let Some((y, prev_size)) = last_y {
                // A gap of more than two lines' height marks a new block.
                if line[0].y - y > 2.2 * size.max(prev_size) {
                    out.push('\n');
                }
            }
            last_y = Some((line[0].y, size));
            let mut end: Option<f64> = None;
            for g in line.iter() {
                if let Some(end) = end {
                    let gap = g.x - end;
                    if gap > 1.5 * g.size {
                        // A column break: at least three spaces, more for
                        // wider gaps so tables keep a rough alignment.
                        #[expect(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "the gap is positive and the count is clamped"
                        )]
                        let n = ((gap / (0.5 * g.size)) as usize).clamp(3, 40);
                        out.extend(std::iter::repeat_n(' ', n));
                    } else if gap > 0.15 * g.size && !out.ends_with(' ') && g.text != " " {
                        out.push(' ');
                    }
                }
                if !(g.text == " " && out.ends_with(' ')) {
                    out.push_str(&g.text);
                }
                end = Some(end.map_or(g.end, |e: f64| e.max(g.end)));
            }
            let trimmed = out.trim_end_matches(' ').len();
            out.truncate(trimmed);
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_dates() {
        assert_eq!(
            parse_pdf_date("D:20240307143000+01'00'").as_deref(),
            Some("2024-03-07T14:30:00+01:00")
        );
        assert_eq!(
            parse_pdf_date("D:2024").as_deref(),
            Some("2024-01-01T00:00:00+00:00")
        );
        assert_eq!(parse_pdf_date("D:20241307").as_deref(), None);
        assert_eq!(parse_pdf_date("yesterday"), None);
    }

    #[test]
    fn text_strings() {
        assert_eq!(decode_text_string(b"Plain"), "Plain");
        assert_eq!(decode_text_string(&[0xfe, 0xff, 0, b'H', 0, b'i']), "Hi");
        assert_eq!(decode_text_string(&[0xef, 0xbb, 0xbf, 0xc2, 0xa3]), "£");
    }

    #[test]
    fn ligatures_and_spaces() {
        assert_eq!(normalise("Oﬃce\u{a0}–"), "Office -");
    }

    #[test]
    fn rejects_non_pdf_and_oversize() {
        let limits = Limits::default();
        assert_eq!(extract(b"hello", &limits), Err(ExtractError::NotPdf));
        let small = Limits {
            max_bytes: 3,
            ..Limits::default()
        };
        assert_eq!(extract(b"%PDF-1.4", &small), Err(ExtractError::TooLarge(3)));
    }

    #[test]
    fn garbage_after_header_is_an_error_not_a_panic() {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        bytes.extend((0..5000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13).to_le_bytes()[0]));
        assert!(matches!(
            extract(&bytes, &Limits::default()),
            Err(ExtractError::Unreadable(_))
        ));
    }
}
