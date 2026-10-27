# Reading imported PDFs

Part of M5 (bulk import of past contracts and invoices). This is the
reading half, in the `core-pdf` crate: pure functions with no database or
HTTP. The import job and the confirmation screen come with M5.

No AI is involved anywhere (spec rule). Everything is an ordinary PDF
library plus fixed patterns, and every field found is a **suggestion**
that a person confirms before it is saved.

## What it does

1. **Text layer** (`core_pdf::extract`). Reads each page's text with
   [`pdf-extract`](https://crates.io/crates/pdf-extract) (pure Rust on top of
   `lopdf`, so it builds on Windows, macOS and Linux with nothing to
   install). Glyph positions are rebuilt into lines the way pdfplumber and
   `pdftotext -layout` do, so a label and its value stay on one line with a
   column gap between them (`Total      £2,280.00`). Also reads the PDF's
   title, author and creation date. This text is what makes the file
   searchable.
2. **Scans** (`core_pdf::ocr`). A page with images and almost no text is
   flagged `needs_ocr`. If Tesseract is installed, the page's scan is taken
   out of the PDF as stored (JPEG, JPEG 2000, fax G3/G4 wrapped as TIFF, or
   raw pixels written as PNM) and passed to the `tesseract` command. Nothing
   is rendered. Without Tesseract the PDF is still stored and attached, just
   not searchable.
3. **Suggestions** (`core_pdf::suggest`). Looks for labels (`Invoice No`,
   `Due date`, `Total due`, `VAT`, `Bill to`, `commences on`, …) and reads the
   value beside or below each with a parser for that kind of value: dates
   (`14 March 2024`, `13/04/2024`, `April 6, 2024`), money with currency
   (`£1,250.50`, `1.190,00 EUR`, `(45.00)` as a credit), references. Things
   with a shape of their own are found without a label: emails, phone
   numbers, IBANs and UK VAT numbers (both check-digit verified), UK company
   numbers. Contract parties come from "made between A (…) and B (…)". It
   also guesses the kind of document (invoice, credit note, receipt, quote,
   purchase order, statement, contract) from keywords.
4. **Issuer templates** (`core_pdf::templates`). For suppliers whose
   invoices arrive often, a workspace can describe the layout once:
   keywords that identify the issuer and a regular expression per field,
   including `custom.<key>` fields. This is the invoice2data approach.
   Templates are validated like custom field patterns (size limits, issues
   named by path).

Each suggestion carries its confidence (high, medium, low) and where it was
found (page, line, the line's text, the label or template), so the screen
can show the evidence next to each proposed value.

## Prior art we borrowed from

- **invoice2data** (Python): per-issuer YAML templates with keywords and
  regexes. Our templates are the same idea in JSON.
- **pdfplumber / pdfminer.six**: rebuilding lines and columns from character
  positions rather than trusting the PDF's drawing order.
- **pdftotext -layout** (Poppler): column gaps kept as runs of spaces.
- **pdf-extract, lopdf** (Rust): the parsing itself.
- **Tesseract**: open-source OCR (not AI in the generative sense; a
  classic trained recogniser), optional.

Considered and not used: `pdfium-render` (needs Google's PDFium binary per
platform), `pdf_oxide` (young), Poppler bindings (C++ dependency, awkward on
Windows).

## Limits and safety

- 50 MB, 500 pages and 2 million characters by default (`Limits`).
- Panics inside the PDF libraries on malformed files become errors, and a
  page that fails is kept empty instead of failing the document.
- Password-protected PDFs are refused with a clear error; ones with only an
  owner password (print or copy restrictions) open normally.
- Parsing is CPU-bound: the worker should run it on a blocking thread with a
  timeout. OCR has its own timeout (120 s per page by default).

## Not yet

- Line items (the table of rows on an invoice). Totals, dates and numbers
  come first; rows are a later addition.
- Pages whose text is drawn as vector outlines, which only a renderer plus
  OCR could read.
- Learning templates from confirmed imports.

## Trying it

```sh
cargo run -p core-pdf --example read_pdf -- invoice.pdf [templates.json]
cargo run -p core-pdf --example ocr_pdf -- scanned.pdf   # needs tesseract
```
