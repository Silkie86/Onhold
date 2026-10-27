//! Making PDFs from a document's HTML.
//!
//! [`Chromium`] prints the HTML with headless Chrome or Chromium, so a
//! workspace's branding (fonts, colours, logo, layout) comes out as
//! designed. [`PlainText`] needs nothing installed: it lays the document's
//! text out in a fixed-width font, tables included, so a document can
//! always be made and read, if not prettily. The worker uses Chromium when
//! it can find it and falls back to plain text otherwise.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use pdf_extract::content::{Content, Operation};
use pdf_extract::{Document, Object, Stream, StringFormat, dictionary};

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("Chromium is not installed (set CHROMIUM_PATH)")]
    NotInstalled,
    #[error("printing took longer than {0:?}")]
    Timeout(Duration),
    #[error("printing failed: {0}")]
    Failed(String),
}

/// Turns a complete HTML document into PDF bytes.
pub trait Renderer: Send + Sync {
    fn render(&self, html: &str, title: &str) -> Result<Vec<u8>, RenderError>;
    /// `chromium` or `plain`, recorded with each version made.
    fn name(&self) -> &'static str;
}

// Plain text -----------------------------------------------------------------------

/// A4 in points, with the margins and the type the plain layout uses.
const PAGE_WIDTH: i64 = 595;
const PAGE_HEIGHT: i64 = 842;
const MARGIN: i64 = 48;
const FONT_SIZE: i64 = 9;
const LEADING: i64 = 12;
/// Courier is 0.6 em wide: 499 points of text width holds 92 characters.
const COLUMNS: usize = 92;

/// Lays a document's text out on A4 pages in Courier, with no fonts or
/// programs to install.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlainText;

impl Renderer for PlainText {
    fn render(&self, html: &str, title: &str) -> Result<Vec<u8>, RenderError> {
        let text = html2text::config::plain()
            .string_from_read(html.as_bytes(), COLUMNS)
            .map_err(|e| RenderError::Failed(e.to_string()))?;
        let lines: Vec<&str> = text.lines().collect();
        text_pdf(&lines, title)
    }

    fn name(&self) -> &'static str {
        "plain"
    }
}

/// Characters Courier can show through WinAnsiEncoding; anything else
/// becomes `?`.
fn win_ansi(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| match c {
            '\u{20}'..='\u{7e}' => c as u8,
            '\u{a0}'..='\u{ff}' => c as u32 as u8,
            '\u{202f}' | '\u{2009}' | '\u{2007}' => b' ',
            '€' => 0x80,
            '‚' => 0x82,
            '„' => 0x84,
            '…' => 0x85,
            '‘' => 0x91,
            '’' => 0x92,
            '“' => 0x93,
            '”' => 0x94,
            '•' => 0x95,
            '–' => 0x96,
            '—' => 0x97,
            '™' => 0x99,
            '\t' => b' ',
            _ => b'?',
        })
        .collect()
}

/// A PDF of `lines` in Courier, as many pages as they need.
pub fn text_pdf(lines: &[&str], title: &str) -> Result<Vec<u8>, RenderError> {
    let per_page = ((PAGE_HEIGHT - 2 * MARGIN) / LEADING) as usize;
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
        "Encoding" => "WinAnsiEncoding",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let mut kids = Vec::new();
    let chunks: Vec<&[&str]> = if lines.is_empty() {
        vec![&[]]
    } else {
        lines.chunks(per_page).collect()
    };
    for chunk in chunks {
        let mut ops = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), FONT_SIZE.into()]),
            Operation::new("TL", vec![LEADING.into()]),
            Operation::new(
                "Td",
                vec![MARGIN.into(), (PAGE_HEIGHT - MARGIN - FONT_SIZE).into()],
            ),
        ];
        for line in chunk {
            ops.push(Operation::new(
                "Tj",
                vec![Object::String(win_ansi(line), StringFormat::Literal)],
            ));
            ops.push(Operation::new("T*", vec![]));
        }
        ops.push(Operation::new("ET", vec![]));
        let content = Content { operations: ops }
            .encode()
            .map_err(|e| RenderError::Failed(e.to_string()))?;
        let content_id = doc.add_object(Stream::new(dictionary! {}, content));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), PAGE_WIDTH.into(), PAGE_HEIGHT.into()],
        });
        kids.push(page_id.into());
    }
    let count = kids.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => count,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    let info_id = doc.add_object(dictionary! {
        "Title" => Object::String(win_ansi(title), StringFormat::Literal),
        "Producer" => Object::string_literal("OnHold"),
    });
    doc.trailer.set("Root", catalog_id);
    doc.trailer.set("Info", info_id);
    doc.compress();
    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|e| RenderError::Failed(e.to_string()))?;
    Ok(out)
}

// Chromium -------------------------------------------------------------------------

/// Prints with headless Chrome, Chromium or Edge.
///
/// The page can reach nothing: every host name fails to resolve, and a
/// Content-Security-Policy put first in the page ([`with_policy`]) allows
/// only inline styles and `data:` images and fonts, so no script runs and
/// no file or address is loaded. Headless mode gives each print a fresh, temporary
/// profile of its own.
#[derive(Debug, Clone)]
pub struct Chromium {
    pub program: PathBuf,
    pub timeout: Duration,
    /// `--no-sandbox`, needed when running as root in a container.
    pub no_sandbox: bool,
}

impl Chromium {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            timeout: Duration::from_secs(90),
            no_sandbox: false,
        }
    }

    /// The first Chrome, Chromium or Edge found on this machine.
    pub fn find() -> Option<Self> {
        find_browser().map(Self::new)
    }
}

/// Where browsers usually are, by name on the `PATH` and by full path.
fn find_browser() -> Option<PathBuf> {
    const NAMES: &[&str] = &[
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
        "chrome",
        "msedge",
    ];
    const PATHS: &[&str] = &[
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ];
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        for name in NAMES {
            for candidate in [dir.join(name), dir.join(format!("{name}.exe"))] {
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    PATHS.iter().map(PathBuf::from).find(|p| p.is_file())
}

/// What a printed page may load: inline styles, and images and fonts
/// written into the page itself.
pub const PRINT_POLICY: &str =
    "default-src 'none'; style-src 'unsafe-inline' data:; img-src data:; font-src data:";

/// `html` with [`PRINT_POLICY`] as the first thing in its head, ahead of
/// anything the document says (policies only ever add restrictions).
pub fn with_policy(html: &str) -> String {
    let meta = format!("<meta http-equiv=\"Content-Security-Policy\" content=\"{PRINT_POLICY}\">");
    let lower = html.to_ascii_lowercase();
    let head = lower
        .match_indices("<head")
        .find(|(i, _)| {
            lower[i + 5..]
                .chars()
                .next()
                .is_some_and(|c| c == '>' || c.is_ascii_whitespace())
        })
        .and_then(|(i, _)| lower[i..].find('>').map(|j| i + j + 1));
    match head {
        Some(at) => format!("{}{meta}{}", &html[..at], &html[at..]),
        None => format!("{meta}{html}"),
    }
}

/// A directory removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("onhold-print-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn file_url(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let s = s.replace(' ', "%20").replace('#', "%23");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

impl Renderer for Chromium {
    fn render(&self, html: &str, _title: &str) -> Result<Vec<u8>, RenderError> {
        let failed = |e: std::io::Error| RenderError::Failed(e.to_string());
        let dir = TempDir::new().map_err(failed)?;
        let input = dir.0.join("document.html");
        let output = dir.0.join("document.pdf");
        std::fs::write(&input, with_policy(html)).map_err(failed)?;
        let mut cmd = Command::new(&self.program);
        cmd.arg("--headless")
            .arg("--disable-gpu")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-extensions")
            .arg("--disable-background-networking")
            .arg("--disable-sync")
            .arg("--disable-dev-shm-usage")
            .arg("--hide-scrollbars")
            .arg("--mute-audio")
            .arg("--host-resolver-rules=MAP * ~NOTFOUND")
            .arg("--no-pdf-header-footer")
            .arg(format!("--print-to-pdf={}", output.display()));
        if self.no_sandbox {
            cmd.arg("--no-sandbox");
        }
        cmd.arg(file_url(&input))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => RenderError::NotInstalled,
            _ => RenderError::Failed(e.to_string()),
        })?;
        let mut stderr = child.stderr.take();
        let err = std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(e) = stderr.as_mut() {
                let _ = e.read_to_string(&mut s);
            }
            s
        });
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() > self.timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(RenderError::Timeout(self.timeout));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => return Err(RenderError::Failed(e.to_string())),
            }
        };
        let message = err.join().unwrap_or_default();
        match std::fs::read(&output) {
            Ok(bytes) if status.success() && bytes.starts_with(b"%PDF") => Ok(bytes),
            _ => Err(RenderError::Failed(
                message
                    .lines()
                    .rfind(|l| !l.contains("dbus") && !l.trim().is_empty())
                    .unwrap_or("no PDF was written")
                    .chars()
                    .take(500)
                    .collect(),
            )),
        }
    }

    fn name(&self) -> &'static str {
        "chromium"
    }
}

// Appending pages --------------------------------------------------------------------

/// `original` with every page of `extra` added at the end, the original's
/// own pages untouched. Used to put a signing certificate after a document.
pub fn append_pages(original: &[u8], extra: &[u8]) -> Result<Vec<u8>, RenderError> {
    let bad = |e: pdf_extract::Error| RenderError::Failed(e.to_string());
    let mut doc = Document::load_mem(original).map_err(bad)?;
    if doc.is_encrypted() {
        return Err(RenderError::Failed(
            "the document is password-protected".into(),
        ));
    }
    let mut add = Document::load_mem(extra).map_err(bad)?;
    add.renumber_objects_with(doc.max_id + 1);
    let root_pages = doc
        .catalog()
        .and_then(|c| c.get(b"Pages"))
        .and_then(Object::as_reference)
        .map_err(bad)?;
    let new_pages: Vec<_> = add.get_pages().into_values().collect();
    for (id, object) in std::mem::take(&mut add.objects) {
        match object.type_name().unwrap_or(b"") {
            b"Catalog" | b"Pages" => {}
            b"Page" => {
                let mut dict = object.as_dict().map_err(bad)?.clone();
                dict.set("Parent", root_pages);
                doc.objects.insert(id, Object::Dictionary(dict));
            }
            _ => {
                doc.objects.insert(id, object);
            }
        }
    }
    doc.max_id = doc.max_id.max(add.max_id);
    let pages = doc
        .get_object_mut(root_pages)
        .and_then(Object::as_dict_mut)
        .map_err(bad)?;
    let mut kids = pages
        .get(b"Kids")
        .and_then(Object::as_array)
        .cloned()
        .unwrap_or_default();
    kids.extend(new_pages.iter().map(|id| Object::Reference(*id)));
    let count = pages.get(b"Count").and_then(Object::as_i64).unwrap_or(0) + new_pages.len() as i64;
    pages.set("Kids", kids);
    pages.set("Count", count);
    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|e| RenderError::Failed(e.to_string()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_pdfs_read_back() {
        let html = "<h1>Booking contract</h1><p>Fee: £1,250.00 (€1.450,00)</p>\
                    <table><tr><td>Load-in</td><td>17:00</td></tr></table>";
        let pdf = PlainText.render(html, "Contract").expect("renders");
        assert!(pdf.starts_with(b"%PDF-1.7"));
        let read = crate::extract(&pdf, &crate::Limits::default()).expect("reads");
        let text = read.text();
        assert!(text.contains("Booking contract"), "{text}");
        assert!(text.contains("£1,250.00"), "{text}");
        assert!(text.contains("€1.450,00"), "{text}");
        assert!(text.contains("Load-in"), "{text}");
        assert_eq!(read.metadata.title.as_deref(), Some("Contract"));
    }

    #[test]
    fn long_documents_take_several_pages() {
        let lines: Vec<String> = (1..=150).map(|i| format!("Line {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let pdf = text_pdf(&refs, "Long").expect("renders");
        let read = crate::extract(&pdf, &crate::Limits::default()).expect("reads");
        assert_eq!(read.pages.len(), 3);
        assert!(read.pages[2].text.contains("Line 150"));
    }

    #[test]
    fn pages_are_appended_after_the_original() {
        let original = text_pdf(&["The contract itself"], "Contract").expect("renders");
        let certificate = text_pdf(&["Signed by Ada"], "Certificate").expect("renders");
        let both = append_pages(&original, &certificate).expect("merges");
        let read = crate::extract(&both, &crate::Limits::default()).expect("reads");
        assert_eq!(read.pages.len(), 2);
        assert!(read.pages[0].text.contains("The contract itself"));
        assert!(read.pages[1].text.contains("Signed by Ada"));
        assert!(append_pages(b"not a pdf", &certificate).is_err());
    }

    #[test]
    fn the_print_policy_comes_first() {
        let out = with_policy("<!doctype html><HTML><Head lang=en><title>x</title></head></html>");
        assert!(
            out.starts_with("<!doctype html><HTML><Head lang=en><meta http-equiv"),
            "{out}"
        );
        let bare = with_policy("<p>hi</p>");
        assert!(bare.starts_with("<meta http-equiv"), "{bare}");
        // `<header>` is not the head.
        let header = with_policy("<header>x</header>");
        assert!(header.starts_with("<meta"), "{header}");
    }

    #[test]
    fn file_urls_work_on_every_system() {
        assert_eq!(
            file_url(Path::new("/tmp/a b/x.html")),
            "file:///tmp/a%20b/x.html"
        );
        assert_eq!(
            file_url(Path::new(r"C:\Temp\x.html")),
            "file:///C:/Temp/x.html"
        );
    }
}
