//! Prints what the parser makes of a PDF: its text, then the suggested
//! fields as JSON.
//!
//! `cargo run -p core-pdf --example read_pdf -- invoice.pdf [templates.json]`

use core_pdf::suggest::{Options, suggest};
use core_pdf::{Limits, extract};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: read_pdf <file.pdf> [templates.json]")?;
    let bytes = std::fs::read(&path)?;
    let doc = extract(&bytes, &Limits::default())?;
    for page in &doc.pages {
        println!(
            "----- page {} {}-----",
            page.number,
            if page.needs_ocr { "(needs OCR) " } else { "" }
        );
        println!("{}", page.text);
    }
    let pages: Vec<&str> = doc.pages.iter().map(|p| p.text.as_str()).collect();
    let mut found = suggest(&pages, &Options::default());
    if let Some(path) = args.next() {
        let templates: Vec<core_pdf::templates::Template> =
            serde_json::from_str(&std::fs::read_to_string(path)?)?;
        let compiled =
            core_pdf::templates::compile_all(&templates).map_err(|e| format!("{e:?}"))?;
        core_pdf::templates::apply(&compiled, &pages, &Options::default(), &mut found);
    }
    println!("{}", serde_json::to_string_pretty(&doc.metadata)?);
    println!("{}", serde_json::to_string_pretty(&found)?);
    Ok(())
}
