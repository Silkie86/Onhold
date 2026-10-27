mod common;

use std::cell::RefCell;

use common::{PageSpec, pdf, t};
use core_pdf::ocr::{ImageFormat, Ocr, OcrError, ScanImage, ocr_scanned_pages, scan_images};
use core_pdf::{Limits, extract};

/// Records what it was given and answers with fixed text.
struct FakeOcr(RefCell<Vec<ScanImage>>);

impl Ocr for FakeOcr {
    fn recognise(&self, image: &ScanImage) -> Result<String, OcrError> {
        self.0.borrow_mut().push(image.clone());
        Ok(format!("Invoice No: SCAN-{}\x0c", image.page))
    }
}

fn scanned(filter: &str, bytes: Vec<u8>) -> Vec<u8> {
    pdf(vec![
        PageSpec {
            texts: vec![t(
                50.0,
                800.0,
                "A page with a real text layer, no OCR needed.",
            )],
            ..Default::default()
        },
        PageSpec {
            image: Some(bytes),
            image_filter: Some(filter),
            ..Default::default()
        },
    ])
}

#[test]
fn jpeg_scans_pass_through_untouched() {
    let jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 1, 2, 3, 0xff, 0xd9];
    let bytes = scanned("DCTDecode", jpeg.clone());
    let images = scan_images(&bytes, &Limits::default(), &[2]).expect("images");
    assert_eq!(
        images,
        [ScanImage {
            page: 2,
            format: ImageFormat::Jpeg,
            bytes: jpeg
        }]
    );
}

#[test]
fn raw_grey_samples_become_pgm() {
    let bytes = scanned("", (0..16).collect());
    let images = scan_images(&bytes, &Limits::default(), &[2]).expect("images");
    assert_eq!(images[0].format, ImageFormat::Pnm);
    assert_eq!(&images[0].bytes[..11], b"P5\n4 4\n255\n");
    assert_eq!(&images[0].bytes[11..], &(0..16).collect::<Vec<u8>>()[..]);
}

#[test]
fn ocr_fills_in_only_the_scanned_pages() {
    let bytes = scanned("DCTDecode", vec![0xff, 0xd8, 0xff, 0xd9]);
    let mut doc = extract(&bytes, &Limits::default()).expect("extract");
    assert!(doc.needs_ocr());
    let ocr = FakeOcr(RefCell::new(Vec::new()));
    let read = ocr_scanned_pages(&bytes, &mut doc, &ocr, &Limits::default()).expect("ocr");
    assert_eq!(read, 1);
    assert_eq!(ocr.0.borrow().len(), 1);
    assert!(
        doc.pages[0]
            .text
            .starts_with("A page with a real text layer")
    );
    assert!(!doc.pages[0].ocr);
    assert_eq!(doc.pages[1].text, "Invoice No: SCAN-2\n");
    assert!(doc.pages[1].ocr);
    assert!(!doc.needs_ocr());
    // Nothing left to do the second time.
    assert_eq!(
        ocr_scanned_pages(&bytes, &mut doc, &ocr, &Limits::default()).expect("ocr"),
        0
    );
}

#[test]
fn unsupported_images_are_skipped() {
    let bytes = scanned("JBIG2Decode", vec![0; 16]);
    assert!(
        scan_images(&bytes, &Limits::default(), &[2])
            .expect("images")
            .is_empty()
    );
}
