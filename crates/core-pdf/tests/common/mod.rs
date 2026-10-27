//! Builds small PDFs for tests, so fixtures are readable code rather than
//! binary files.

#![allow(dead_code)]

use pdf_extract::content::{Content, Operation};
use pdf_extract::{Document, Object, Stream, dictionary};

/// A run of text at (`x`, `y`) points from the bottom left, in Helvetica.
pub struct Text<'a> {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub text: &'a str,
}

pub fn t(x: f32, y: f32, text: &str) -> Text<'_> {
    Text {
        x,
        y,
        size: 10.0,
        text,
    }
}

/// One page's content: text runs, and optionally a 4×4 grey image covering
/// the page, stored with `image_filter` (DCTDecode unless given; its bytes
/// need not be a real JPEG, since only OCR would decode them).
#[derive(Default)]
pub struct PageSpec<'a> {
    pub texts: Vec<Text<'a>>,
    pub image: Option<Vec<u8>>,
    pub image_filter: Option<&'a str>,
}

fn win_ansi(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| match c {
            '£' => 0xa3,
            '€' => 0x80,
            c if (c as u32) < 0x80 => c as u8,
            _ => b'?',
        })
        .collect()
}

pub fn pdf(pages: Vec<PageSpec<'_>>) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let mut kids = Vec::new();
    for spec in pages {
        let mut ops = Vec::new();
        let mut xobjects = pdf_extract::Dictionary::new();
        if let Some(bytes) = spec.image {
            let filter = spec.image_filter.unwrap_or("DCTDecode");
            let mut dict = dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 4,
                "Height" => 4,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
            };
            // An empty filter name means raw samples.
            if !filter.is_empty() {
                dict.set("Filter", filter);
            }
            let image = Stream::new(dict, bytes);
            let image_id = doc.add_object(image);
            xobjects.set("Im1", image_id);
            ops.push(Operation::new("q", vec![]));
            ops.push(Operation::new(
                "cm",
                vec![
                    595.into(),
                    0.into(),
                    0.into(),
                    842.into(),
                    0.into(),
                    0.into(),
                ],
            ));
            ops.push(Operation::new("Do", vec!["Im1".into()]));
            ops.push(Operation::new("Q", vec![]));
        }
        for text in spec.texts {
            ops.push(Operation::new("BT", vec![]));
            ops.push(Operation::new("Tf", vec!["F1".into(), text.size.into()]));
            ops.push(Operation::new("Td", vec![text.x.into(), text.y.into()]));
            ops.push(Operation::new(
                "Tj",
                vec![Object::string_literal(win_ansi(text.text))],
            ));
            ops.push(Operation::new("ET", vec![]));
        }
        let content = Content { operations: ops };
        let content_id = doc.add_object(Stream::new(
            dictionary! {},
            content.encode().expect("encode content"),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id },
                "XObject" => xobjects,
            },
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
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    let info_id = doc.add_object(dictionary! {
        "Title" => Object::string_literal("Invoice INV-1042"),
        "Author" => Object::string_literal("Acme Talent Ltd"),
        "CreationDate" => Object::string_literal("D:20240307143000Z"),
    });
    doc.trailer.set("Info", info_id);
    let mut out = Vec::new();
    doc.save_to(&mut out).expect("save pdf");
    out
}

/// A one-page text PDF with each line at the given positions.
pub fn text_pdf(texts: Vec<Text<'_>>) -> Vec<u8> {
    pdf(vec![PageSpec {
        texts,
        ..Default::default()
    }])
}
