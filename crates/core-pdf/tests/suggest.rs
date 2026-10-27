mod common;

use core_engines::import::DateOrder;
use core_pdf::suggest::{Confidence, DocumentKind, Options, Suggestions, Value, suggest};
use core_pdf::templates::{Template, apply, compile, compile_all};
use core_pdf::{Limits, extract};

fn run(text: &str) -> Suggestions {
    suggest(&[text], &Options::default())
}

fn text_of(s: &Suggestions, field: &str) -> Option<String> {
    match &s.best(field)?.value {
        Value::Text(t) => Some(t.clone()),
        Value::Date(d) => Some(d.to_string()),
        Value::Money(m) => Some(format!("{} {}", m.minor, m.currency)),
    }
}

const UK_INVOICE: &str = "\
Bright Lights Studios Ltd
12 Market Street, London EC1A 1BB
Tel: +44 (0)20 7946 0018      accounts@brightlights.co.uk

TAX INVOICE
Invoice No:      BLS-2024-0117              Bill to:
Invoice Date:    14 March 2024              OnHold Talent Agency Ltd
Due Date:        13/04/2024                 5 River Lane, Bristol
PO Number:       PO-88231

Description                         Qty     Unit price      Amount
Photoshoot day rate - Jane Doe      2       £750.00         £1,500.00
Usage licence (12 months)           1       £400.00         £400.00
                                            Sub-total       £1,900.00
                                            VAT @ 20%       £380.00
                                            Total due       £2,280.00

Please pay to: Bright Lights Studios Ltd, IBAN GB82 WEST 1234 5698 7654 32
VAT Reg No: GB 562 2359 45    Registered in England and Wales, company number 08123456
";

#[test]
fn uk_invoice() {
    let s = run(UK_INVOICE);
    assert_eq!(s.kinds[0].kind, DocumentKind::Invoice);
    assert_eq!(
        text_of(&s, "invoice_number").as_deref(),
        Some("BLS-2024-0117")
    );
    assert_eq!(text_of(&s, "order_number").as_deref(), Some("PO-88231"));
    assert_eq!(text_of(&s, "issue_date").as_deref(), Some("2024-03-14"));
    assert_eq!(text_of(&s, "due_date").as_deref(), Some("2024-04-13"));
    assert_eq!(text_of(&s, "subtotal").as_deref(), Some("190000 GBP"));
    assert_eq!(text_of(&s, "tax").as_deref(), Some("38000 GBP"));
    assert_eq!(text_of(&s, "total").as_deref(), Some("228000 GBP"));
    assert_eq!(
        text_of(&s, "email").as_deref(),
        Some("accounts@brightlights.co.uk")
    );
    assert_eq!(text_of(&s, "phone").as_deref(), Some("+44 (0)20 7946 0018"));
    assert_eq!(
        text_of(&s, "iban").as_deref(),
        Some("GB82WEST12345698765432")
    );
    assert_eq!(text_of(&s, "vat_number").as_deref(), Some("GB562235945"));
    assert_eq!(text_of(&s, "company_number").as_deref(), Some("08123456"));
    assert_eq!(
        text_of(&s, "party").as_deref(),
        Some("OnHold Talent Agency Ltd")
    );
    assert_eq!(
        text_of(&s, "issuer").as_deref(),
        Some("Bright Lights Studios Ltd")
    );

    // Every suggestion says where it came from.
    let total = s.best("total").expect("total");
    assert_eq!(total.confidence, Confidence::High);
    assert_eq!(total.source.label.as_deref(), Some("Total due"));
    assert_eq!((total.source.page, total.source.line), (1, 16));
    // Line amounts and the sub-total are not offered as the total.
    assert!(s.all("total").all(|t| t.value == total.value));
}

#[test]
fn us_invoice_with_month_first_dates() {
    let text = "\
INVOICE #10442
Date: 03/07/2024
Payment due: April 6, 2024
Subtotal $1,000.00
Sales tax $82.50
Balance due $1,082.50
";
    let s = suggest(
        &[text],
        &Options {
            date_order: DateOrder::Mdy,
            default_currency: "GBP".into(),
        },
    );
    assert_eq!(text_of(&s, "invoice_number").as_deref(), Some("10442"));
    assert_eq!(text_of(&s, "issue_date").as_deref(), Some("2024-03-07"));
    assert_eq!(text_of(&s, "due_date").as_deref(), Some("2024-04-06"));
    assert_eq!(text_of(&s, "tax").as_deref(), Some("8250 USD"));
    assert_eq!(text_of(&s, "total").as_deref(), Some("108250 USD"));
}

#[test]
fn european_amounts_and_document_currency() {
    let text = "\
Rechnung
Rechnungsnummer: 2024-118
Rechnungsdatum: 05.02.2024
Nettobetrag              1.000,00
MwSt 19%                   190,00
Gesamtbetrag             1.190,00 EUR
";
    let s = run(text);
    assert_eq!(text_of(&s, "invoice_number").as_deref(), Some("2024-118"));
    assert_eq!(text_of(&s, "issue_date").as_deref(), Some("2024-02-05"));
    assert_eq!(text_of(&s, "tax").as_deref(), Some("19000 EUR"));
    assert_eq!(text_of(&s, "total").as_deref(), Some("119000 EUR"));
}

#[test]
fn values_on_the_line_below_their_label() {
    let text = "\
Invoice number          Invoice date          Amount due
INV-0099                01/06/2024            £640.00

Bill to
Northern Lights Agency
";
    let s = run(text);
    assert_eq!(text_of(&s, "invoice_number").as_deref(), Some("INV-0099"));
    assert_eq!(text_of(&s, "issue_date").as_deref(), Some("2024-06-01"));
    assert_eq!(text_of(&s, "total").as_deref(), Some("64000 GBP"));
    assert_eq!(
        text_of(&s, "party").as_deref(),
        Some("Northern Lights Agency")
    );
}

#[test]
fn credit_note_is_not_an_invoice() {
    let s =
        run("CREDIT NOTE\nCredit note number: CN-12\nAgainst invoice INV-0099\nTotal (£45.00)\n");
    assert_eq!(s.kinds[0].kind, DocumentKind::CreditNote);
    assert_eq!(text_of(&s, "invoice_number").as_deref(), Some("CN-12"));
    assert_eq!(text_of(&s, "total").as_deref(), Some("-4500 GBP"));
}

#[test]
fn contract_parties_and_dates() {
    let text = "\
MODEL REPRESENTATION AGREEMENT
This Agreement is dated 1st February 2023 and is made between OnHold Talent
Agency Ltd, a company registered in England and Wales with company number
12345678 (\"the Agency\") and Jane Doe of 4 High Street, Leeds (\"the Model\").
1. Term. This agreement commences on 1 March 2023 and continues until 28
February 2025 unless terminated earlier.
2. The Agency shall be entitled to a commission of 20% of all fees.
Signed by the parties on 3 February 2023.
";
    let s = run(text);
    assert_eq!(s.kinds[0].kind, DocumentKind::Contract);
    let parties: Vec<String> = s
        .all("party")
        .filter_map(|p| match &p.value {
            Value::Text(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(parties, ["OnHold Talent Agency Ltd", "Jane Doe"]);
    assert_eq!(s.best("party").map(|p| p.source.line), Some(2));
    assert_eq!(text_of(&s, "start_date").as_deref(), Some("2023-03-01"));
    let signed: Vec<String> = s
        .all("signed_date")
        .filter_map(|p| match &p.value {
            Value::Date(d) => Some(d.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(signed, ["2023-02-01", "2023-02-03"]);
    // No invoice fields out of a contract's prose.
    assert!(s.best("total").is_none());
    assert!(s.best("invoice_number").is_none());
}

#[test]
fn numbered_parties() {
    let text = "THIS AGREEMENT is made on 4 May 2024\nBETWEEN:\n(1) ACME MODELS LIMITED (company number 01234567); and\n(2) JOHN SMITH of 1 Long Road, York.\n";
    let s = run(text);
    let parties: Vec<&Value> = s.all("party").map(|p| &p.value).collect();
    assert_eq!(
        parties,
        [
            &Value::Text("ACME MODELS LIMITED".into()),
            &Value::Text("JOHN SMITH".into())
        ]
    );
}

#[test]
fn prose_does_not_produce_fields() {
    let s = run(
        "Thank you for your business. Payment is due within 30 days.\nPlease send it to the address above.\nThe total is shown on page one.\n",
    );
    assert!(s.fields.is_empty(), "{:?}", s.fields);
    assert!(run("").fields.is_empty());
    assert!(run("").kinds.is_empty());
}

#[test]
fn bad_check_digits_are_ignored() {
    let s = run("IBAN GB82 WEST 1234 5698 7654 33\nVAT GB 562 2359 46\n");
    assert!(s.best("iban").is_none());
    assert!(s.all("vat_number").all(|v| v.confidence < Confidence::High));
}

fn template(json: serde_json::Value) -> Template {
    serde_json::from_value(json).expect("template")
}

#[test]
fn templates_read_their_issuers_layout() {
    let t = template(serde_json::json!({
        "name": "Bright Lights",
        "keywords": ["Bright Lights Studios", "GB 562 2359 45"],
        "kind": "invoice",
        "fields": {
            "invoice_number": "BLS-\\d{4}-\\d+",
            "total": "Total due\\s+(\\S+)",
            "issue_date": { "pattern": "Invoice Date:\\s+(.+?)\\s{3}" },
            "custom.shoot_days": { "pattern": "Jane Doe\\s+(\\d+)", "type": "text" }
        }
    }));
    let compiled = compile_all(&[t]).expect("valid");
    let pages = [UK_INVOICE];
    let mut s = suggest(&pages, &Options::default());
    let applied = apply(&compiled, &pages, &Options::default(), &mut s);
    assert_eq!(applied, ["Bright Lights"]);
    let best = s.best("total").expect("total");
    assert_eq!(best.source.template.as_deref(), Some("Bright Lights"));
    assert_eq!(text_of(&s, "total").as_deref(), Some("228000 GBP"));
    // The template's finding replaces the general one with the same value.
    assert_eq!(s.all("total").count(), 1);
    assert_eq!(text_of(&s, "issue_date").as_deref(), Some("2024-03-14"));
    assert_eq!(text_of(&s, "custom.shoot_days").as_deref(), Some("2"));
    assert_eq!(s.best("custom.shoot_days").map(|c| c.source.line), Some(12));
    assert_eq!(s.kinds[0].kind, DocumentKind::Invoice);
}

#[test]
fn templates_apply_only_when_keywords_match() {
    let t = compile(&template(serde_json::json!({
        "name": "Other",
        "keywords": ["Bright Lights"],
        "exclude_keywords": ["credit note"],
        "fields": { "total": "Total due\\s+(\\S+)" }
    })))
    .expect("valid");
    assert!(t.matches(UK_INVOICE));
    assert!(!t.matches("Bright Lights credit note"));
    assert!(!t.matches("Someone else entirely"));
}

#[test]
fn template_validation_names_the_problem() {
    let bad = template(serde_json::json!({
        "name": "",
        "keywords": [],
        "currency": "XXY",
        "fields": {
            "nonsense": "x",
            "total": "(unclosed",
            "custom.Bad Key": "x"
        }
    }));
    let issues = compile_all(&[bad]).expect_err("invalid");
    let fields: Vec<&str> = issues.iter().map(|i| i.field.as_str()).collect();
    assert_eq!(
        fields,
        [
            "[0].name",
            "[0].keywords",
            "[0].currency",
            "[0].fields.custom.Bad Key",
            "[0].fields.nonsense",
            "[0].fields.total"
        ]
    );
    // Unknown keys in the template itself are refused when reading it.
    assert!(
        serde_json::from_value::<Template>(
            serde_json::json!({"name": "x", "keywords": ["x"], "feilds": {}})
        )
        .is_err()
    );
}

#[test]
fn end_to_end_from_a_pdf() {
    use common::{t, text_pdf};
    let bytes = text_pdf(vec![
        t(50.0, 800.0, "Acme Talent Ltd"),
        t(50.0, 760.0, "INVOICE"),
        t(50.0, 740.0, "Invoice number:"),
        t(160.0, 740.0, "INV-1042"),
        t(50.0, 725.0, "Invoice date:"),
        t(160.0, 725.0, "07/03/2024"),
        t(50.0, 600.0, "Total"),
        t(400.0, 600.0, "£1,200.00"),
        t(50.0, 580.0, "billing@acme-talent.co.uk"),
    ]);
    let doc = extract(&bytes, &Limits::default()).expect("extract");
    let pages: Vec<&str> = doc.pages.iter().map(|p| p.text.as_str()).collect();
    let s = suggest(&pages, &Options::default());
    assert_eq!(text_of(&s, "invoice_number").as_deref(), Some("INV-1042"));
    assert_eq!(text_of(&s, "issue_date").as_deref(), Some("2024-03-07"));
    assert_eq!(text_of(&s, "total").as_deref(), Some("120000 GBP"));
    assert_eq!(
        text_of(&s, "email").as_deref(),
        Some("billing@acme-talent.co.uk")
    );
    assert_eq!(text_of(&s, "issuer").as_deref(), Some("Acme Talent Ltd"));
}

#[test]
fn suggestions_serialise_for_the_api() {
    let s = run("Invoice No: A-1\nDue date: 01/02/2024\nTotal £10.00\n");
    let json = serde_json::to_value(&s).expect("json");
    let due = json["fields"]
        .as_array()
        .and_then(|f| f.iter().find(|x| x["field"] == "due_date"))
        .expect("due date");
    assert_eq!(
        due["value"],
        serde_json::json!({"type": "date", "value": "2024-02-01"})
    );
    assert_eq!(due["confidence"], "high");
    let total = json["fields"]
        .as_array()
        .and_then(|f| f.iter().find(|x| x["field"] == "total"))
        .expect("total");
    assert_eq!(
        total["value"],
        serde_json::json!({"type": "money", "value": {"minor": 1000, "currency": "GBP"}})
    );
}
