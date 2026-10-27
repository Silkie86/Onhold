//! Reading imported PDFs (past contracts, invoices and the like) without AI.
//!
//! - [`extract`] pulls the text layer out of a PDF, laid out in lines, so its
//!   contents can be searched.
//! - [`ocr`] finds the scanned page images in PDFs that have no text layer
//!   and hands them to Tesseract, when it is installed.
//! - [`suggest`] looks for fields in that text with fixed patterns (invoice
//!   number, dates, totals, emails, parties), and [`templates`] lets a
//!   workspace add patterns for the issuers it sees often, in the manner of
//!   invoice2data. Everything found is a suggestion with the text it came
//!   from: the person confirms it before anything is saved.
//!
//! - [`write`] makes PDFs from documents' HTML (headless Chromium, or a
//!   plain-text layout when no browser is installed) and appends pages, such
//!   as a signing certificate, to an existing PDF.
//!
//! No database or HTTP here; the worker calls in.

pub mod extract;
pub mod ocr;
pub mod suggest;
pub mod templates;
pub mod write;

pub use extract::{ExtractError, Extracted, Limits, Page, extract};
