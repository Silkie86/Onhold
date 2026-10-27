//! Runs OCR on a scanned PDF's pages with Tesseract, then prints the text
//! and suggested fields.
//!
//! `cargo run -p core-pdf --example ocr_pdf -- scan.pdf`

use core_pdf::ocr::{TesseractCli, ocr_scanned_pages};
use core_pdf::suggest::{Options, suggest};
use core_pdf::{Limits, extract};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: ocr_pdf <file.pdf>")?;
    let bytes = std::fs::read(&path)?;
    let limits = Limits::default();
    let mut doc = extract(&bytes, &limits)?;
    let read = ocr_scanned_pages(&bytes, &mut doc, &TesseractCli::default(), &limits)?;
    eprintln!("OCR read {read} page(s)");
    for page in &doc.pages {
        println!("----- page {} -----\n{}", page.number, page.text);
    }
    let pages: Vec<&str> = doc.pages.iter().map(|p| p.text.as_str()).collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&suggest(&pages, &Options::default()))?
    );
    Ok(())
}
