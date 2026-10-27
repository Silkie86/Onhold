mod common;

use common::{PageSpec, pdf, t, text_pdf};
use core_pdf::{ExtractError, Limits, extract};

#[test]
fn lays_out_lines_and_columns() {
    let bytes = text_pdf(vec![
        t(50.0, 800.0, "Acme Talent Ltd"),
        // Written out of order: layout goes by position, not drawing order.
        t(400.0, 700.0, "£1,200.00"),
        t(50.0, 700.0, "Total"),
        t(50.0, 715.0, "Invoice number: INV-1042"),
    ]);
    let doc = extract(&bytes, &Limits::default()).expect("extract");
    assert_eq!(doc.pages.len(), 1);
    let lines: Vec<&str> = doc.pages[0].text.lines().collect();
    assert_eq!(lines[0], "Acme Talent Ltd");
    // A blank line for the big vertical gap.
    assert_eq!(lines[1], "");
    assert_eq!(lines[2], "Invoice number: INV-1042");
    assert!(lines[3].starts_with("Total   "), "{:?}", lines[3]);
    assert!(lines[3].ends_with("£1,200.00"), "{:?}", lines[3]);
    assert!(!doc.pages[0].needs_ocr);
    assert!(!doc.truncated);
}

#[test]
fn reads_metadata() {
    let doc = extract(&text_pdf(vec![t(50.0, 800.0, "x")]), &Limits::default()).expect("extract");
    assert_eq!(doc.metadata.title.as_deref(), Some("Invoice INV-1042"));
    assert_eq!(doc.metadata.author.as_deref(), Some("Acme Talent Ltd"));
    assert_eq!(
        doc.metadata.created.as_deref(),
        Some("2024-03-07T14:30:00+00:00")
    );
}

#[test]
fn flags_scanned_pages() {
    let bytes = pdf(vec![
        PageSpec {
            texts: vec![t(50.0, 800.0, "This page has a proper text layer on it.")],
            ..Default::default()
        },
        PageSpec {
            texts: vec![t(500.0, 20.0, "2")],
            image: Some(vec![0xff; 16]),
            ..Default::default()
        },
    ]);
    let doc = extract(&bytes, &Limits::default()).expect("extract");
    assert_eq!(doc.pages.len(), 2);
    assert!(!doc.pages[0].needs_ocr);
    assert!(doc.pages[1].needs_ocr);
    assert!(doc.needs_ocr());
    assert!(doc.text().contains('\x0c'));
}

#[test]
fn enforces_page_and_text_limits() {
    let bytes = pdf((0..3)
        .map(|_| PageSpec {
            texts: vec![t(50.0, 800.0, "Some words on a page")],
            ..Default::default()
        })
        .collect());
    let few_pages = Limits {
        max_pages: 2,
        ..Limits::default()
    };
    assert_eq!(
        extract(&bytes, &few_pages),
        Err(ExtractError::TooManyPages(2))
    );
    let little_text = Limits {
        max_chars: 30,
        ..Limits::default()
    };
    let doc = extract(&bytes, &little_text).expect("extract");
    assert!(doc.truncated);
    assert!(doc.text().len() <= 30 + 4);
    assert_eq!(doc.pages[2].text, "");
}

#[test]
fn truncated_file_does_not_panic() {
    let bytes = text_pdf(vec![t(50.0, 800.0, "Hello")]);
    for cut in [20, bytes.len() / 2, bytes.len() - 10] {
        let _ = extract(&bytes[..cut], &Limits::default());
    }
}
