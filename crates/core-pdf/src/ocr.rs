//! OCR for scanned PDFs, with Tesseract.
//!
//! A scanned page is a picture of text: one big image and no text layer.
//! We take that image out of the PDF as it is stored (JPEG, JPEG 2000, fax
//! G3/G4 or raw pixels), wrap it in a file format Tesseract reads (raw
//! pixels become PNM, fax data becomes TIFF), and run the `tesseract`
//! command on it. Nothing is rendered, so no PDF renderer is needed; pages
//! whose text is drawn as vector outlines are out of reach, and stay
//! flagged as needing OCR.
//!
//! Tesseract is optional: where it is not installed, [`TesseractCli::available`]
//! says so and imports keep the PDF without searchable text.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use pdf_extract::{Dictionary, Document, Object, ObjectId};

use crate::extract::{ExtractError, Extracted, Limits, guarded, load};

/// The most pixels in one image we will hand to OCR (about A4 at 600 dpi,
/// twice over).
pub const MAX_PIXELS: u64 = 80_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Jpeg,
    Jpeg2000,
    /// PBM, PGM or PPM, built from raw pixels.
    Pnm,
    /// A TIFF wrapping fax-compressed data.
    Tiff,
}

impl ImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Jpeg2000 => "jp2",
            ImageFormat::Pnm => "pnm",
            ImageFormat::Tiff => "tif",
        }
    }
}

/// A page's scan, in a format OCR tools read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanImage {
    pub page: u32,
    pub format: ImageFormat,
    pub bytes: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum OcrError {
    #[error("Tesseract is not installed")]
    NotInstalled,
    #[error("OCR took longer than {0:?}")]
    Timeout(Duration),
    #[error("OCR failed: {0}")]
    Failed(String),
}

/// Something that turns an image into text.
pub trait Ocr {
    fn recognise(&self, image: &ScanImage) -> Result<String, OcrError>;
}

/// Runs the `tesseract` command.
#[derive(Debug, Clone)]
pub struct TesseractCli {
    pub program: PathBuf,
    /// Tesseract language codes joined by `+`, e.g. `eng` or `eng+fra`.
    pub languages: String,
    pub timeout: Duration,
}

impl Default for TesseractCli {
    fn default() -> Self {
        Self {
            program: "tesseract".into(),
            languages: "eng".into(),
            timeout: Duration::from_secs(120),
        }
    }
}

impl TesseractCli {
    /// Whether the program runs at all.
    pub fn available(&self) -> bool {
        Command::new(&self.program)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

impl Ocr for TesseractCli {
    fn recognise(&self, image: &ScanImage) -> Result<String, OcrError> {
        let path = std::env::temp_dir().join(format!(
            "onhold-ocr-{}.{}",
            uuid::Uuid::now_v7(),
            image.format.extension()
        ));
        std::fs::write(&path, &image.bytes).map_err(|e| OcrError::Failed(e.to_string()))?;
        let result = run(self, &path);
        let _ = std::fs::remove_file(&path);
        result
    }
}

fn run(t: &TesseractCli, path: &std::path::Path) -> Result<String, OcrError> {
    let mut child = Command::new(&t.program)
        .arg(path)
        .arg("stdout")
        .args(["-l", &t.languages, "--psm", "3"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => OcrError::NotInstalled,
            _ => OcrError::Failed(e.to_string()),
        })?;
    // Read output on threads so a full pipe cannot stall the child.
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(o) = stdout.as_mut() {
            let _ = o.read_to_string(&mut s);
        }
        s
    });
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
            Ok(None) if started.elapsed() > t.timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(OcrError::Timeout(t.timeout));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(OcrError::Failed(e.to_string())),
        }
    };
    let text = out.join().unwrap_or_default();
    let message = err.join().unwrap_or_default();
    if status.success() {
        Ok(text)
    } else {
        Err(OcrError::Failed(message.trim().chars().take(500).collect()))
    }
}

/// Runs OCR on every page [`crate::extract`] flagged as scanned, replacing
/// its text. Pages whose image we cannot convert are left as they were.
/// Returns how many pages were read.
pub fn ocr_scanned_pages(
    bytes: &[u8],
    doc: &mut Extracted,
    ocr: &dyn Ocr,
    limits: &Limits,
) -> Result<usize, OcrError> {
    let wanted: Vec<u32> = doc
        .pages
        .iter()
        .filter(|p| p.needs_ocr && !p.ocr)
        .map(|p| p.number)
        .collect();
    if wanted.is_empty() {
        return Ok(0);
    }
    let images =
        scan_images(bytes, limits, &wanted).map_err(|e| OcrError::Failed(e.to_string()))?;
    let mut done = 0;
    for image in images {
        let text = ocr.recognise(&image)?;
        if let Some(page) = doc.pages.iter_mut().find(|p| p.number == image.page) {
            page.text = text.replace('\x0c', "").trim_end().to_owned() + "\n";
            page.ocr = true;
            done += 1;
        }
    }
    Ok(done)
}

/// The largest image on each of the given pages, converted for OCR. Pages
/// whose image is in a form we cannot convert are skipped.
pub fn scan_images(
    bytes: &[u8],
    limits: &Limits,
    pages: &[u32],
) -> Result<Vec<ScanImage>, ExtractError> {
    let doc = load(bytes, limits)?;
    let page_ids = doc.get_pages();
    let mut out = Vec::new();
    for &page in pages {
        let Some(&page_id) = page_ids.get(&page) else {
            continue;
        };
        let image = guarded(|| Ok(largest_image(&doc, page_id).and_then(|id| convert(&doc, id))))
            .ok()
            .flatten();
        if let Some((format, bytes)) = image {
            out.push(ScanImage {
                page,
                format,
                bytes,
            });
        }
    }
    Ok(out)
}

fn largest_image(doc: &Document, page_id: ObjectId) -> Option<ObjectId> {
    doc.get_page_images(page_id)
        .ok()?
        .into_iter()
        .max_by_key(|i| i.width.saturating_mul(i.height))
        .map(|i| i.id)
}

fn int(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<i64> {
    match dict.get(key).ok()? {
        Object::Reference(id) => doc.get_object(*id).ok()?.as_i64().ok(),
        o => o.as_i64().ok(),
    }
}

fn name<'a>(doc: &'a Document, o: &'a Object) -> Option<&'a [u8]> {
    match o {
        Object::Reference(id) => doc.get_object(*id).ok()?.as_name().ok(),
        Object::Array(a) => a.first().and_then(|o| o.as_name().ok()),
        o => o.as_name().ok(),
    }
}

fn convert(doc: &Document, id: ObjectId) -> Option<(ImageFormat, Vec<u8>)> {
    let stream = doc.get_object(id).ok()?.as_stream().ok()?;
    let dict = &stream.dict;
    let width = u32::try_from(int(doc, dict, b"Width")?).ok()?;
    let height = u32::try_from(int(doc, dict, b"Height")?).ok()?;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return None;
    }
    let filters: Vec<Vec<u8>> = match dict.get(b"Filter").ok() {
        Some(Object::Array(a)) => a
            .iter()
            .filter_map(|f| f.as_name().ok().map(<[u8]>::to_vec))
            .collect(),
        Some(Object::Name(n)) => vec![n.clone()],
        _ => Vec::new(),
    };
    let bpc = int(doc, dict, b"BitsPerComponent").unwrap_or(8);
    let image_mask = matches!(dict.get(b"ImageMask"), Ok(Object::Boolean(true)));
    let color = dict
        .get(b"ColorSpace")
        .ok()
        .and_then(|c| name(doc, c))
        .unwrap_or(b"DeviceGray");
    let last = filters.last().map(Vec::as_slice);
    // Image codecs come last in the chain; text encodings or compression
    // before them (`[/ASCII85Decode /DCTDecode]`) are undone first.
    let encoded = || -> Option<Vec<u8>> {
        if filters.len() <= 1 {
            return Some(stream.content.clone());
        }
        let mut outer = stream.clone();
        let prefix: Vec<Object> = filters[..filters.len() - 1]
            .iter()
            .map(|f| Object::Name(f.clone()))
            .collect();
        outer.dict.set("Filter", Object::Array(prefix));
        outer.dict.remove(b"DecodeParms");
        outer.decompressed_content().ok()
    };
    match last {
        Some(b"DCTDecode") => Some((ImageFormat::Jpeg, encoded()?)),
        Some(b"JPXDecode") => Some((ImageFormat::Jpeg2000, encoded()?)),
        Some(b"CCITTFaxDecode") => {
            let parms = match dict.get(b"DecodeParms").ok() {
                Some(Object::Dictionary(d)) => Some(d),
                Some(Object::Array(a)) => a.first().and_then(|d| d.as_dict().ok()),
                Some(Object::Reference(r)) => doc.get_dictionary(*r).ok(),
                _ => None,
            };
            let k = parms.and_then(|p| int(doc, p, b"K")).unwrap_or(0);
            let black_is_1 =
                parms.is_some_and(|p| matches!(p.get(b"BlackIs1"), Ok(Object::Boolean(true))));
            Some((
                ImageFormat::Tiff,
                ccitt_tiff(width, height, k, black_is_1, &encoded()?),
            ))
        }
        None | Some(b"FlateDecode" | b"LZWDecode" | b"ASCII85Decode" | b"ASCIIHexDecode") => {
            let raw = if filters.is_empty() {
                stream.content.clone()
            } else {
                stream.decompressed_content().ok()?
            };
            let channels = match color {
                _ if image_mask => 1,
                b"DeviceGray" | b"CalGray" => 1,
                b"DeviceRGB" | b"CalRGB" => 3,
                b"DeviceCMYK" => 4,
                _ => return None,
            };
            pnm(width, height, channels, bpc, image_mask, &raw).map(|b| (ImageFormat::Pnm, b))
        }
        _ => None,
    }
}

/// Raw PDF samples as PNM: 1-bit grey becomes PBM, 8-bit grey PGM, and
/// 8-bit RGB or CMYK PPM.
pub(crate) fn pnm(
    width: u32,
    height: u32,
    channels: u32,
    bpc: i64,
    mask: bool,
    raw: &[u8],
) -> Option<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    match (channels, bpc) {
        (1, 1) => {
            let row = w.div_ceil(8);
            let data = raw.get(..row * h)?;
            let mut out = format!("P4\n{width} {height}\n").into_bytes();
            // PBM has 1 for black; PDF grey has 0 for black, while a stencil
            // mask paints its 0 samples (in the fill colour, black on scans).
            out.extend(data.iter().map(|b| if mask { *b } else { !b }));
            Some(out)
        }
        (1, 8) => {
            let mut out = format!("P5\n{width} {height}\n255\n").into_bytes();
            out.extend_from_slice(raw.get(..w * h)?);
            Some(out)
        }
        (3, 8) => {
            let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
            out.extend_from_slice(raw.get(..w * h * 3)?);
            Some(out)
        }
        (4, 8) => {
            let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
            for px in raw.get(..w * h * 4)?.as_chunks::<4>().0 {
                let k = 255 - u32::from(px[3]);
                for c in &px[..3] {
                    out.push(((255 - u32::from(*c)) * k / 255) as u8);
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// Wraps CCITT fax data in a one-strip TIFF. `k` is the PDF's K: negative
/// for Group 4, zero for Group 3 one-dimensional, positive for Group 3
/// two-dimensional.
pub(crate) fn ccitt_tiff(
    width: u32,
    height: u32,
    k: i64,
    black_is_1: bool,
    data: &[u8],
) -> Vec<u8> {
    let compression: u16 = if k < 0 { 4 } else { 3 };
    // PDF's fax data has no end-of-line codes or byte alignment by default,
    // which TIFF's T4Options bit 2 ("fill bits") leaves off too; bit 0 marks
    // two-dimensional coding.
    let t4_options: u32 = if k > 0 { 1 } else { 0 };
    let photometric: u16 = if black_is_1 { 1 } else { 0 };
    let mut entries: Vec<(u16, u16, u32, u32)> = vec![
        (256, 4, 1, width),                  // ImageWidth
        (257, 4, 1, height),                 // ImageLength
        (258, 3, 1, 1),                      // BitsPerSample
        (259, 3, 1, u32::from(compression)), // Compression
        (262, 3, 1, u32::from(photometric)), // PhotometricInterpretation
        (273, 4, 1, 0),                      // StripOffsets, set below
        (277, 3, 1, 1),                      // SamplesPerPixel
        (278, 4, 1, height),                 // RowsPerStrip
        (279, 4, 1, data.len() as u32),      // StripByteCounts
    ];
    if compression == 3 {
        entries.push((292, 4, 1, t4_options)); // T4Options
    }
    entries.sort_by_key(|e| e.0);
    let ifd_len = 2 + entries.len() * 12 + 4;
    let data_offset = (8 + ifd_len) as u32;
    let mut out = Vec::with_capacity(data_offset as usize + data.len());
    out.extend_from_slice(b"II*\0");
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, kind, count, value) in entries {
        let value = if tag == 273 { data_offset } else { value };
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        if kind == 3 {
            out.extend_from_slice(&(value as u16).to_le_bytes());
            out.extend_from_slice(&[0, 0]);
        } else {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(data);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pbm_inverts_grey_but_not_masks() {
        let raw = [0b1010_0000, 0b0000_0000];
        let p = pnm(3, 2, 1, 1, false, &raw).expect("pbm");
        assert_eq!(&p[..8], b"P4\n3 2\n\x5f");
        assert_eq!(p[8], 0xff);
        let m = pnm(3, 2, 1, 1, true, &raw).expect("pbm");
        assert_eq!(m[7], 0b1010_0000);
    }

    #[test]
    fn pnm_needs_enough_samples() {
        assert!(pnm(4, 4, 1, 8, false, &[0; 15]).is_none());
        assert!(pnm(4, 4, 1, 8, false, &[0; 16]).is_some());
        assert!(pnm(2, 1, 3, 16, false, &[0; 12]).is_none());
        let cmyk = pnm(1, 1, 4, 8, false, &[0, 255, 255, 0]).expect("ppm");
        assert_eq!(&cmyk[cmyk.len() - 3..], &[255, 0, 0]);
    }

    #[test]
    fn tiff_header() {
        let t = ccitt_tiff(1700, 2200, -1, false, &[1, 2, 3]);
        assert_eq!(&t[..4], b"II*\0");
        let count = u16::from_le_bytes([t[8], t[9]]) as usize;
        assert_eq!(count, 9);
        // The strip offset points at the data.
        let entry = |i: usize| &t[10 + i * 12..10 + (i + 1) * 12];
        let offsets = (0..count)
            .map(entry)
            .find(|e| e[..2] == 273u16.to_le_bytes())
            .expect("tag");
        let off = u32::from_le_bytes([offsets[8], offsets[9], offsets[10], offsets[11]]) as usize;
        assert_eq!(&t[off..], &[1, 2, 3]);
        assert_eq!(ccitt_tiff(10, 10, 0, false, &[]).len(), 8 + 2 + 10 * 12 + 4);
    }

    #[test]
    fn missing_program_is_not_installed() {
        let t = TesseractCli {
            program: "onhold-no-such-tesseract".into(),
            ..TesseractCli::default()
        };
        assert!(!t.available());
        let image = ScanImage {
            page: 1,
            format: ImageFormat::Pnm,
            bytes: b"P5\n1 1\n255\n\0".to_vec(),
        };
        assert!(matches!(t.recognise(&image), Err(OcrError::NotInstalled)));
    }
}
