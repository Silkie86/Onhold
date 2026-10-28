//! M6 the workspace's books, against stand-ins for Xero and QuickBooks
//! Online (OAuth and their accounting APIs): connecting and choosing where
//! things go; issued invoices, payments, voids and credit notes sent; what
//! was paid in the books read back; past invoices imported; an answer lost,
//! a refusal put right, a grant withdrawn, and disconnecting.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../core-api/tests/common/mod.rs"]
mod common;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use chrono::{Days, NaiveDate, Utc};
use common::{Client, TestApp};
use core_api::Config;
use core_api::config::AccountingConfig;
use core_integrations::http::WebhookClient;
use core_integrations::mail::MemoryMailer;
use core_integrations::oauth::OAuthApp;
use core_worker::{Settings, Worker};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use url::Url;
use uuid::Uuid;

/// An invoice or credit note in the books.
#[derive(Clone, Debug)]
struct Doc {
    credit: bool,
    number: String,
    contact: String,
    date: NaiveDate,
    currency: String,
    total: i64,
    tax: i64,
    due: i64,
    /// AUTHORISED, PAID or VOIDED.
    status: String,
    /// What was sent.
    body: Value,
}

/// What the stand-ins hold and were asked.
#[derive(Default)]
struct World {
    /// The refresh token that works now; each refresh replaces it.
    refresh: String,
    refreshes: usize,
    withdrawn: bool,
    revoked: Vec<String>,
    /// Xero connections ended (one organisation each).
    disconnected: Vec<String>,
    /// Customers made inactive in QuickBooks.
    inactive: BTreeSet<String>,
    /// Contacts (customers) by id.
    contacts: BTreeMap<String, String>,
    docs: BTreeMap<String, Doc>,
    /// Payments by id: the invoice and amount.
    payments: BTreeMap<String, (String, i64, Value)>,
    /// Credit notes set against invoices: (credit note, invoice, amount).
    allocations: BTreeMap<String, (String, String, i64)>,
    /// Answers by idempotency key (Xero) or request id (Intuit), refusals
    /// included.
    keys: HashMap<String, (StatusCode, Value)>,
    /// The next document added is kept but its answer is lost.
    lose_next: bool,
    /// The next payment added is kept but its answer is lost.
    lose_next_payment: bool,
    /// Contacts of this name are refused.
    refuse: Option<String>,
    ids: usize,
    /// Documents added.
    added: usize,
}

impl World {
    fn id(&mut self, prefix: &str) -> String {
        self.ids += 1;
        format!("{prefix}-{}", self.ids)
    }

    fn doc_numbered(&self, number: &str) -> Vec<(String, Doc)> {
        self.docs
            .iter()
            .filter(|(_, d)| d.number == number)
            .map(|(id, d)| (id.clone(), d.clone()))
            .collect()
    }
}

type Shared = Arc<Mutex<World>>;

fn form(body: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

/// An amount the APIs send as a decimal, in pence.
fn pence(v: &Value) -> i64 {
    (v.as_f64().unwrap() * 100.0).round() as i64
}

fn decimal(minor: i64) -> Value {
    json!(minor as f64 / 100.0)
}

fn answer(status: StatusCode, body: Value) -> Response {
    (status, axum::Json(body)).into_response()
}

fn xero_invalid(message: &str) -> Response {
    answer(
        StatusCode::BAD_REQUEST,
        json!({ "ErrorNumber": 10, "Type": "ValidationException",
                "Message": "A validation exception occurred",
                "Elements": [{ "ValidationErrors": [{ "Message": message }] }] }),
    )
}

fn intuit_invalid(message: &str) -> Response {
    answer(
        StatusCode::BAD_REQUEST,
        json!({ "Fault": { "Error": [{ "Message": "A business validation error has occurred",
                                       "Detail": message, "code": "6000" }],
                           "type": "ValidationFault" } }),
    )
}

fn not_found() -> Response {
    answer(
        StatusCode::NOT_FOUND,
        json!({ "Message": "The resource you're looking for cannot be found" }),
    )
}

fn token(w: &mut World, headers: &HeaderMap, body: &str, xero: bool) -> Response {
    assert!(
        headers["authorization"]
            .to_str()
            .unwrap()
            .starts_with("Basic "),
        "the client authenticates with Basic auth"
    );
    let f = form(body);
    let ok = match f["grant_type"].as_str() {
        "authorization_code" => f.get("code").map(String::as_str) == Some("good-code"),
        // Refresh tokens are used once: an old one is refused.
        "refresh_token" => !w.withdrawn && f.get("refresh_token") == Some(&w.refresh),
        _ => false,
    };
    if !ok {
        return answer(
            StatusCode::BAD_REQUEST,
            json!({ "error": "invalid_grant", "error_description": "The refresh token is invalid." }),
        );
    }
    w.refreshes += 1;
    w.refresh = format!("refresh-{}", w.refreshes);
    let mut v = json!({
        "access_token": "access",
        "refresh_token": w.refresh,
        // Expired at once, so every use refreshes first.
        "expires_in": 0,
        "token_type": "Bearer",
    });
    if xero {
        v["scope"] = json!(
            "offline_access accounting.invoices accounting.payments accounting.contacts accounting.settings.read"
        );
    }
    axum::Json(v).into_response()
}

fn xero_doc(id: &str, d: &Doc, contacts: &BTreeMap<String, String>) -> Value {
    json!({
        "InvoiceID": id,
        "InvoiceNumber": d.number,
        "Type": "ACCREC",
        "Contact": { "ContactID": d.contact, "Name": contacts.get(&d.contact) },
        "DateString": format!("{}T00:00:00", d.date),
        "DueDateString": format!("{}T00:00:00", d.date + Days::new(30)),
        "CurrencyCode": d.currency,
        "Total": decimal(d.total),
        "TotalTax": decimal(d.tax),
        "AmountDue": decimal(d.due),
        "Status": d.status,
        "LineItems": d.body["LineItems"],
    })
}

fn xero(
    w: &mut World,
    method: &str,
    path: &str,
    q: &HashMap<String, String>,
    headers: &HeaderMap,
    body: &str,
) -> Response {
    assert_eq!(headers["authorization"], "Bearer access");
    assert_eq!(headers["xero-tenant-id"], "tenant-1");
    let key = headers
        .get("idempotency-key")
        .map(|k| k.to_str().unwrap().to_owned());
    if let Some((status, v)) = key.as_ref().and_then(|k| w.keys.get(k)) {
        return answer(*status, v.clone());
    }
    let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let done: Value = match (method, parts.as_slice()) {
        ("GET", ["Accounts"]) => json!({ "Accounts": [
            { "AccountID": "acc-200", "Code": "200", "Name": "Sales", "Class": "REVENUE", "Type": "REVENUE", "Status": "ACTIVE" },
            { "AccountID": "acc-201", "Code": "201", "Name": "Old sales", "Class": "REVENUE", "Type": "REVENUE", "Status": "ARCHIVED" },
            { "AccountID": "bank-1", "Code": "090", "Name": "Business Bank", "Class": "ASSET", "Type": "BANK", "Status": "ACTIVE" },
            { "AccountID": "acc-400", "Code": "400", "Name": "Advertising", "Class": "EXPENSE", "Type": "EXPENSE", "Status": "ACTIVE" },
        ] }),
        ("GET", ["TaxRates"]) => json!({ "TaxRates": [
            { "Name": "20% (VAT on Income)", "TaxType": "OUTPUT2", "EffectiveRate": "20.0000", "Status": "ACTIVE" },
            { "Name": "No VAT", "TaxType": "NONE", "EffectiveRate": "0.0000", "Status": "ACTIVE" },
        ] }),
        ("GET", ["Contacts"]) => {
            let term = q["searchTerm"].to_lowercase();
            let found: Vec<Value> = w
                .contacts
                .iter()
                .filter(|(_, n)| n.to_lowercase().contains(&term))
                .map(|(id, n)| json!({ "ContactID": id, "Name": n, "ContactStatus": "ACTIVE" }))
                .collect();
            json!({ "Contacts": found })
        }
        ("PUT", ["Contacts"]) => {
            let name = body["Contacts"][0]["Name"].as_str().unwrap().to_owned();
            if w.refuse.as_deref() == Some(name.as_str()) {
                // Remembered under its key: the same request is refused again.
                let message = format!("{name} is on hold in these books");
                w.keys.insert(
                    key.unwrap(),
                    (
                        StatusCode::BAD_REQUEST,
                        json!({ "Elements": [{ "ValidationErrors": [{ "Message": message }] }] }),
                    ),
                );
                return xero_invalid(&message);
            }
            let id = w.id("contact");
            w.contacts.insert(id.clone(), name);
            json!({ "Contacts": [{ "ContactID": id }] })
        }
        ("PUT", [list @ ("Invoices" | "CreditNotes")]) => {
            let credit = *list == "CreditNotes";
            let d = &body[*list][0];
            assert_eq!(d["Type"], if credit { "ACCRECCREDIT" } else { "ACCREC" });
            assert_eq!(d["Status"], "AUTHORISED");
            assert_eq!(d["LineAmountTypes"], "Exclusive");
            let (mut net, mut tax) = (0, 0);
            for l in d["LineItems"].as_array().unwrap() {
                assert_eq!(l["AccountCode"], "200", "{l}");
                let amount = (l["Quantity"].as_f64().unwrap() * pence(&l["UnitAmount"]) as f64)
                    .round() as i64;
                net += amount;
                // No tax type: the account's default, 20%.
                if l["TaxType"] != "NONE" {
                    tax += amount / 5;
                }
            }
            let number = if credit {
                d["CreditNoteNumber"].as_str()
            } else {
                d["InvoiceNumber"].as_str()
            }
            .unwrap()
            .to_owned();
            let id = w.id(if credit { "credit" } else { "invoice" });
            w.docs.insert(
                id.clone(),
                Doc {
                    credit,
                    number,
                    contact: d["Contact"]["ContactID"].as_str().unwrap().to_owned(),
                    date: d["Date"].as_str().unwrap().parse().unwrap(),
                    currency: d["CurrencyCode"].as_str().unwrap().to_owned(),
                    total: net + tax,
                    tax,
                    due: net + tax,
                    status: "AUTHORISED".into(),
                    body: d.clone(),
                },
            );
            w.added += 1;
            let field = if credit { "CreditNoteID" } else { "InvoiceID" };
            let v = json!({ *list: [{ field: id }] });
            if w.lose_next {
                w.lose_next = false;
                w.keys.insert(key.unwrap(), (StatusCode::OK, v));
                return answer(
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({ "Message": "lost" }),
                );
            }
            v
        }
        ("GET", ["Invoices"]) => {
            let rows: Vec<Value> = match q.get("IDs") {
                Some(ids) => ids
                    .split(',')
                    .filter_map(|id| w.docs.get(id).map(|d| xero_doc(id, d, &w.contacts)))
                    .collect(),
                None => {
                    // Past invoices: `Type=="ACCREC"&&Date<DateTime(y,m,d)`.
                    let filter = &q["where"];
                    assert!(
                        filter.starts_with("Type==\"ACCREC\"&&Date<DateTime("),
                        "{filter}"
                    );
                    let ymd: Vec<u32> = filter
                        .trim_start_matches("Type==\"ACCREC\"&&Date<DateTime(")
                        .trim_end_matches(')')
                        .split(',')
                        .map(|n| n.parse().unwrap())
                        .collect();
                    let before = NaiveDate::from_ymd_opt(ymd[0] as i32, ymd[1], ymd[2]).unwrap();
                    let statuses: Vec<&str> = q["Statuses"].split(',').collect();
                    if q["page"] != "1" {
                        Vec::new()
                    } else {
                        w.docs
                            .iter()
                            .filter(|(_, d)| {
                                !d.credit
                                    && d.date < before
                                    && statuses.contains(&d.status.as_str())
                            })
                            .map(|(id, d)| xero_doc(id, d, &w.contacts))
                            .collect()
                    }
                }
            };
            json!({ "Invoices": rows })
        }
        ("GET", [list @ ("Invoices" | "CreditNotes"), id]) => {
            let Some(d) = w.docs.get(*id) else {
                return not_found();
            };
            json!({ *list: [{ "Status": d.status }] })
        }
        ("POST", [list @ ("Invoices" | "CreditNotes"), id]) => {
            let entry = &body[*list][0];
            assert_eq!(entry["Status"], "VOIDED");
            let Some(d) = w.docs.get_mut(*id) else {
                return not_found();
            };
            if d.status == "VOIDED" {
                return xero_invalid("Invoice not of valid status for modification");
            }
            if d.due != d.total {
                return xero_invalid(
                    "This document cannot be edited as it has a payment or credit note allocated to it.",
                );
            }
            d.status = "VOIDED".into();
            d.due = 0;
            json!({ *list: [{ "Status": "VOIDED" }] })
        }
        ("PUT", ["Payments"]) => {
            let p = &body["Payments"][0];
            assert_eq!(p["Account"]["AccountID"], "bank-1");
            let invoice = p["Invoice"]["InvoiceID"].as_str().unwrap().to_owned();
            let amount = pence(&p["Amount"]);
            let Some(d) = w.docs.get_mut(&invoice) else {
                return not_found();
            };
            if amount > d.due {
                return xero_invalid(
                    "Payment amount exceeds the amount outstanding on this document",
                );
            }
            d.due -= amount;
            if d.due == 0 {
                d.status = "PAID".into();
            }
            let id = w.id("payment");
            w.payments.insert(id.clone(), (invoice, amount, p.clone()));
            let v = json!({ "Payments": [{ "PaymentID": id }] });
            if w.lose_next_payment {
                w.lose_next_payment = false;
                w.keys.insert(key.unwrap(), (StatusCode::OK, v));
                return answer(
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({ "Message": "lost" }),
                );
            }
            v
        }
        ("POST", ["Payments", id]) => {
            assert_eq!(body["Payments"][0]["Status"], "DELETED");
            let Some((invoice, amount, _)) = w.payments.remove(*id) else {
                return not_found();
            };
            let d = w.docs.get_mut(&invoice).unwrap();
            d.due += amount;
            d.status = "AUTHORISED".into();
            json!({ "Payments": [{ "Status": "DELETED" }] })
        }
        ("PUT", ["CreditNotes", credit, "Allocations"]) => {
            let a = &body["Allocations"][0];
            let invoice = a["Invoice"]["InvoiceID"].as_str().unwrap().to_owned();
            let amount = pence(&a["Amount"]);
            if amount > w.docs[&invoice].due || amount > w.docs[*credit].due {
                return xero_invalid("The allocation is more than is left to allocate");
            }
            w.docs.get_mut(&invoice).unwrap().due -= amount;
            w.docs.get_mut(*credit).unwrap().due -= amount;
            let id = w.id("allocation");
            w.allocations
                .insert(id.clone(), ((*credit).to_owned(), invoice, amount));
            json!({ "Allocations": [{ "AllocationID": id }] })
        }
        ("DELETE", ["CreditNotes", _credit, "Allocations", id]) => {
            let Some((credit, invoice, amount)) = w.allocations.remove(*id) else {
                return not_found();
            };
            w.docs.get_mut(&invoice).unwrap().due += amount;
            w.docs.get_mut(&credit).unwrap().due += amount;
            json!({})
        }
        _ => panic!("unexpected Xero call {method} {path}"),
    };
    if let Some(k) = key {
        w.keys.insert(k, (StatusCode::OK, done.clone()));
    }
    axum::Json(done).into_response()
}

/// The text between the first pair of single quotes after `after`.
fn quoted_after(q: &str, after: &str) -> String {
    let rest = &q[q.find(after).unwrap() + after.len()..];
    let rest = rest.trim_start().trim_start_matches('\'');
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            '\'' => break,
            c => out.push(c),
        }
    }
    out
}

fn intuit_invoice(id: &str, d: &Doc, contacts: &BTreeMap<String, String>) -> Value {
    json!({
        "Id": id,
        "DocNumber": d.number,
        "TxnDate": d.date.to_string(),
        "DueDate": (d.date + Days::new(30)).to_string(),
        "CurrencyRef": { "value": d.currency },
        "CustomerRef": { "value": d.contact, "name": contacts.get(&d.contact) },
        "TotalAmt": decimal(d.total),
        "Balance": decimal(d.due),
        "TxnTaxDetail": { "TotalTax": decimal(d.tax) },
        "Line": [
            { "DetailType": "SalesItemLineDetail", "Amount": decimal(d.total - d.tax),
              "Description": "Performance",
              "SalesItemLineDetail": { "Qty": 1, "UnitPrice": decimal(d.total - d.tax) } },
            { "DetailType": "SubTotalLineDetail", "Amount": decimal(d.total - d.tax) },
        ],
    })
}

fn intuit_query(w: &World, query: &str) -> Value {
    if query.contains("from Item") {
        json!({ "QueryResponse": { "Item": [{ "Id": "1", "Name": "Services", "FullyQualifiedName": "Services", "Type": "Service" }] } })
    } else if query.contains("from Account") {
        json!({ "QueryResponse": { "Account": [{ "Id": "35", "Name": "Checking", "AccountType": "Bank" }] } })
    } else if query.contains("from Customer") {
        let name = quoted_after(query, "DisplayName in (");
        // Intuit leaves inactive ones out unless asked for them, and adds
        // " (deleted)" to their names.
        let all = query.contains("Active in (true, false)");
        assert!(query.contains(&format!("'{name} (deleted)'")), "{query}");
        let found: Vec<Value> = w
            .contacts
            .iter()
            .filter(|(id, n)| **n == name && (all || !w.inactive.contains(*id)))
            .map(|(id, _)| intuit_customer(w, id))
            .collect();
        json!({ "QueryResponse": { "Customer": found } })
    } else if query.contains("from Invoice where Id in (") {
        let inner = &query[query.find('(').unwrap() + 1..query.find(')').unwrap()];
        let rows: Vec<Value> = inner
            .split(',')
            .map(|i| i.trim().trim_matches('\''))
            .filter_map(|id| w.docs.get(id).map(|d| intuit_invoice(id, d, &w.contacts)))
            .collect();
        json!({ "QueryResponse": { "Invoice": rows } })
    } else if query.contains("from Invoice where TxnDate <") {
        let before: NaiveDate = quoted_after(query, "TxnDate <").parse().unwrap();
        assert!(query.contains("startposition 1 "), "{query}");
        let rows: Vec<Value> = w
            .docs
            .iter()
            .filter(|(_, d)| !d.credit && d.date < before)
            .map(|(id, d)| intuit_invoice(id, d, &w.contacts))
            .collect();
        json!({ "QueryResponse": { "Invoice": rows } })
    } else {
        panic!("unexpected Intuit query {query}")
    }
}

fn intuit_customer(w: &World, id: &str) -> Value {
    let name = &w.contacts[id];
    let inactive = w.inactive.contains(id);
    let shown = if inactive {
        format!("{name} (deleted)")
    } else {
        name.clone()
    };
    json!({ "Id": id, "DisplayName": shown, "SyncToken": "0", "Active": !inactive })
}

/// Intuit's answer to a change that uses a customer made inactive: the
/// same as for an object not there.
fn intuit_inactive() -> Response {
    answer(
        StatusCode::BAD_REQUEST,
        json!({ "Fault": { "Error": [{ "Message": "Object Not Found",
                                       "Detail": "Object Not Found : Something you're trying to use has been made inactive.",
                                       "code": "610" }],
                           "type": "ValidationFault" } }),
    )
}

fn quickbooks(
    w: &mut World,
    method: &str,
    path: &str,
    q: &HashMap<String, String>,
    headers: &HeaderMap,
    body: &str,
) -> Response {
    assert_eq!(headers["authorization"], "Bearer access");
    assert_eq!(q["minorversion"], "75");
    let key = q.get("requestid").cloned();
    if let Some((status, v)) = key.as_ref().and_then(|k| w.keys.get(k)) {
        return answer(*status, v.clone());
    }
    let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let operation = q.get("operation").map(String::as_str);
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let done: Value = match (method, parts.as_slice(), operation) {
        ("GET", ["companyinfo", "realm-1"], _) => {
            json!({ "CompanyInfo": { "CompanyName": "Owls Inc", "Country": "US" } })
        }
        ("GET", ["query"], _) => intuit_query(w, &q["query"]),
        ("GET", ["preferences"], _) => {
            json!({ "Preferences": { "SalesFormsPrefs": { "AutoApplyCredit": true } } })
        }
        ("POST", ["customer"], None) if body["sparse"] == true => {
            assert_eq!(body["Active"], true);
            let id = body["Id"].as_str().unwrap().to_owned();
            assert_eq!(body["DisplayName"], w.contacts[&id], "named as before");
            assert!(w.inactive.remove(&id), "only inactive ones are made active");
            json!({ "Customer": { "Id": id } })
        }
        ("GET", ["customer", id], _) => json!({ "Customer": intuit_customer(w, id) }),
        ("POST", ["customer"], None) => {
            let name = body["DisplayName"].as_str().unwrap().to_owned();
            if w.refuse.as_deref() == Some(name.as_str()) {
                return intuit_invalid("Duplicate Name Exists Error");
            }
            assert!(
                !w.contacts.values().any(|n| *n == name),
                "Intuit refuses a name taken, inactive or not"
            );
            let id = w.id("cust");
            w.contacts.insert(id.clone(), name);
            json!({ "Customer": { "Id": id } })
        }
        ("POST", ["invoice"], None)
            if w.inactive
                .contains(body["CustomerRef"]["value"].as_str().unwrap()) =>
        {
            return intuit_inactive();
        }
        ("POST", ["invoice"], None) => {
            // US companies' tax is worked out by QuickBooks.
            assert!(body.get("GlobalTaxCalculation").is_none(), "{body}");
            let (mut net, mut tax) = (0, 0);
            for l in body["Line"].as_array().unwrap() {
                assert_eq!(l["DetailType"], "SalesItemLineDetail");
                let detail = &l["SalesItemLineDetail"];
                assert_eq!(detail["ItemRef"]["value"], "1");
                let amount = pence(&l["Amount"]);
                net += amount;
                match detail["TaxCodeRef"]["value"].as_str() {
                    Some("TAX") => tax += amount / 5,
                    Some("NON") => {}
                    other => panic!("tax code {other:?}"),
                }
            }
            let id = w.id("inv");
            w.docs.insert(
                id.clone(),
                Doc {
                    credit: false,
                    number: body["DocNumber"].as_str().unwrap().to_owned(),
                    contact: body["CustomerRef"]["value"].as_str().unwrap().to_owned(),
                    date: body["TxnDate"].as_str().unwrap().parse().unwrap(),
                    currency: body["CurrencyRef"]["value"].as_str().unwrap().to_owned(),
                    total: net + tax,
                    tax,
                    due: net + tax,
                    status: "AUTHORISED".into(),
                    body: body.clone(),
                },
            );
            w.added += 1;
            json!({ "Invoice": { "Id": id } })
        }
        ("GET", [entity @ ("invoice" | "payment"), id], _) => {
            let known = if *entity == "invoice" {
                w.docs.contains_key(*id)
            } else {
                w.payments.contains_key(*id)
            };
            if !known {
                return intuit_inactive();
            }
            let name = if *entity == "invoice" {
                "Invoice"
            } else {
                "Payment"
            };
            json!({ name: { "Id": id, "SyncToken": "0" } })
        }
        ("POST", ["invoice"], Some("void")) => {
            assert_eq!(body["SyncToken"], "0");
            let id = body["Id"].as_str().unwrap();
            let d = w.docs.get_mut(id).unwrap();
            d.status = "VOIDED".into();
            // Intuit keeps a voided invoice with nothing on it.
            d.total = 0;
            d.tax = 0;
            d.due = 0;
            json!({ "Invoice": { "Id": id } })
        }
        ("POST", ["payment"], None) => {
            let line = &body["Line"][0];
            let invoice = line["LinkedTxn"][0]["TxnId"].as_str().unwrap().to_owned();
            assert_eq!(line["LinkedTxn"][0]["TxnType"], "Invoice");
            let amount = pence(&body["TotalAmt"]);
            let d = w.docs.get_mut(&invoice).unwrap();
            if amount > d.due {
                return intuit_invalid("The payment is more than the invoice owes");
            }
            d.due -= amount;
            let id = w.id("pay");
            w.payments
                .insert(id.clone(), (invoice, amount, body.clone()));
            json!({ "Payment": { "Id": id } })
        }
        ("POST", ["payment"], Some("delete"))
            if w.payments
                .get(body["Id"].as_str().unwrap())
                .is_some_and(|(invoice, _, _)| w.inactive.contains(&w.docs[invoice].contact)) =>
        {
            return intuit_inactive();
        }
        ("POST", ["payment"], Some("delete")) => {
            let id = body["Id"].as_str().unwrap();
            let (invoice, amount, _) = w.payments.remove(id).unwrap();
            w.docs.get_mut(&invoice).unwrap().due += amount;
            json!({ "Payment": { "Id": id, "status": "Deleted" } })
        }
        _ => panic!("unexpected Intuit call {method} {path} {operation:?}"),
    };
    if let Some(k) = key {
        w.keys.insert(k, (StatusCode::OK, done.clone()));
    }
    axum::Json(done).into_response()
}

async fn handle(
    State(w): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> Response {
    let path = uri.path().to_owned();
    let q: HashMap<String, String> = uri.query().map(form).unwrap_or_default();
    let mut w = w.lock().unwrap();
    match (method.as_str(), path.as_str()) {
        ("POST", "/connect/token") => token(&mut w, &headers, &body, true),
        ("POST", "/oauth2/v1/tokens/bearer") => token(&mut w, &headers, &body, false),
        ("POST", "/connect/revocation") => {
            w.revoked.push(form(&body)["token"].clone());
            StatusCode::OK.into_response()
        }
        ("POST", "/v2/oauth2/tokens/revoke") => {
            let v: Value = serde_json::from_str(&body).expect("Intuit revokes with JSON");
            w.revoked.push(v["token"].as_str().unwrap().to_owned());
            StatusCode::OK.into_response()
        }
        ("GET", "/connections") => {
            assert_eq!(headers["authorization"], "Bearer access");
            axum::Json(json!([
                { "id": "conn-0", "tenantId": "tenant-0", "tenantType": "ORGANISATION",
                  "tenantName": "Chosen Before", "updatedDateUtc": "2025-01-01T10:00:00" },
                { "id": "conn-p", "tenantId": "practice", "tenantType": "PRACTICEMANAGER",
                  "tenantName": "A Practice", "updatedDateUtc": "2026-12-01T10:00:00" },
                { "id": "conn-1", "tenantId": "tenant-1", "tenantType": "ORGANISATION",
                  "tenantName": "Owls Agency Ltd", "updatedDateUtc": "2026-10-01T10:00:00" },
            ]))
            .into_response()
        }
        ("DELETE", p) if p.starts_with("/connections/") => {
            assert_eq!(headers["authorization"], "Bearer access");
            w.disconnected.push(p["/connections/".len()..].to_owned());
            StatusCode::NO_CONTENT.into_response()
        }
        (m, p) if p.starts_with("/api.xro/2.0/") => {
            xero(&mut w, m, &p["/api.xro/2.0".len()..], &q, &headers, &body)
        }
        (m, p) if p.starts_with("/v3/company/realm-1/") => quickbooks(
            &mut w,
            m,
            &p["/v3/company/realm-1".len()..],
            &q,
            &headers,
            &body,
        ),
        (m, p) => panic!("unexpected call {m} {p}"),
    }
}

async fn stand_ins() -> (Url, Shared) {
    let world: Shared = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new().fallback(handle).with_state(world.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (Url::parse(&format!("http://{addr}")).unwrap(), world)
}

struct Harness {
    app: TestApp,
    worker: Worker,
    world: Shared,
}

async fn harness(pool: PgPool) -> Harness {
    let (base, world) = stand_ins().await;
    let config = Config {
        accounting: AccountingConfig {
            xero: Some(OAuthApp::xero("xero-client".into(), "xero-secret".into()).at(&base)),
            quickbooks: Some(
                OAuthApp::quickbooks("qb-client".into(), "qb-secret".into()).at(&base),
            ),
            xero_api: base.clone(),
            quickbooks_api: base.clone(),
            ..AccountingConfig::default()
        },
        ..Config::for_development()
    };
    let app = TestApp::with_config(pool, config).await;
    let options = app.owner.connect_options().as_ref().clone();
    let queue = core_db::worker_pool_options(PgPoolOptions::new().max_connections(3))
        .connect_with(options)
        .await
        .unwrap();
    let worker = Worker {
        app: app.state.clone(),
        queue,
        mailer: Arc::new(MemoryMailer::default()),
        webhooks: WebhookClient::new(true).unwrap(),
        settings: Settings {
            concurrency: 2,
            backoff: |_| Duration::ZERO,
            fx_rates: None,
            ..Settings::default()
        },
    };
    Harness { app, worker, world }
}

impl Harness {
    async fn drain(&self) {
        for _ in 0..50 {
            let swept = self.worker.schedule_once().await.unwrap();
            let published = self.worker.publish_once().await.unwrap();
            let ran = self.worker.run_jobs_once().await.unwrap();
            if swept.unwrap_or(0) == 0 && published == 0 && ran == 0 {
                return;
            }
        }
        panic!("the queue did not settle");
    }

    async fn get(&self, who: &Client, path: &str) -> Value {
        self.app.get(path).signed_in(who).send().await.ok().body
    }

    async fn post(&self, who: &Client, path: &str, body: Value) -> Value {
        self.app
            .post(path)
            .signed_in(who)
            .json(body)
            .send()
            .await
            .ok()
            .body
    }

    async fn accounting(&self, who: &Client) -> Value {
        self.get(who, "/api/v1/accounting").await["connection"].clone()
    }

    /// Connects as a person would: off to the provider and back.
    async fn try_connect(&self, who: &Client, provider: &str) -> common::Resp {
        let start = self
            .post(
                who,
                "/api/v1/accounting/start",
                json!({ "provider": provider }),
            )
            .await;
        let url = Url::parse(start["url"].as_str().unwrap()).unwrap();
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert!(q["redirect_uri"].ends_with("/settings/accounting"), "{url}");
        if provider == "xero" {
            assert!(q["scope"].contains("accounting.invoices"), "{url}");
            assert!(q["scope"].contains("offline_access"), "{url}");
        } else {
            assert_eq!(q["scope"], "com.intuit.quickbooks.accounting", "{url}");
        }
        self.app
            .post("/api/v1/accounting/finish")
            .signed_in(who)
            .json(json!({ "code": "good-code", "state": q["state"],
                          "realm_id": (provider == "quickbooks").then_some("realm-1") }))
            .send()
            .await
    }

    async fn connect(&self, who: &Client, provider: &str) -> Value {
        self.try_connect(who, provider).await.ok().body
    }

    async fn sync(&self, who: &Client) {
        self.post(who, "/api/v1/accounting/sync", json!({})).await;
        self.drain().await;
    }

    async fn organisation(&self, who: &Client, name: &str) -> Uuid {
        self.app
            .post("/api/v1/organisations")
            .signed_in(who)
            .json(json!({ "name": name }))
            .send()
            .await
            .ok()
            .id("/id")
    }

    /// Issues an invoice of `net` (plus the default 20% VAT) to `payer`.
    async fn issue(&self, who: &Client, payer: Uuid, net: i64) -> Value {
        let draft = self
            .post(
                who,
                "/api/v1/invoices",
                json!({ "payer": { "organisation_id": payer },
                        "line_items": [{ "description": "Evening performance", "unit_minor": net }] }),
            )
            .await;
        let id = draft["id"].as_str().unwrap();
        self.post(who, &format!("/api/v1/invoices/{id}/issue"), json!({}))
            .await
    }

    /// The one document in the books numbered as `inv` is.
    fn in_books(&self, inv: &Value) -> (String, Doc) {
        let w = self.world.lock().unwrap();
        let found = w.doc_numbered(inv["reference"].as_str().unwrap());
        assert_eq!(found.len(), 1, "{found:?}");
        found.into_iter().next().unwrap()
    }

    fn doc(&self, id: &str) -> Doc {
        self.world.lock().unwrap().docs[id].clone()
    }
}

/// Owner's settings: sales to `sales`, payments into `bank`, and each tax
/// code as `tax(rate)` says.
async fn choose(
    h: &Harness,
    owner: &Client,
    sales: &str,
    bank: Option<&str>,
    tax: fn(i64) -> Option<&'static str>,
) -> Value {
    let o = h.get(owner, "/api/v1/accounting/options").await;
    let codes: Vec<Value> = o["tax_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| json!({ "tax_code_id": t["tax_code_id"], "remote": tax(t["rate_bps"].as_i64().unwrap()) }))
        .collect();
    h.app
        .put("/api/v1/accounting/settings")
        .signed_in(owner)
        .json(json!({ "sales_account": sales, "bank_account": bank, "tax_codes": codes }))
        .send()
        .await
        .ok()
        .body
}

fn seed_past(
    w: &mut World,
    contact: &str,
    number: &str,
    days_ago: u64,
    total: i64,
    due: i64,
    status: &str,
) {
    let id = w.id("past");
    w.docs.insert(
        id,
        Doc {
            credit: false,
            number: number.into(),
            contact: contact.into(),
            date: Utc::now().date_naive() - Days::new(days_ago),
            currency: "GBP".into(),
            total,
            tax: total / 6,
            due,
            status: status.into(),
            body: json!({ "LineItems": [
                { "Description": "Spring tour", "Quantity": 1.0, "UnitAmount": decimal(total), "LineAmount": decimal(total) }
            ] }),
        },
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn xero_keeps_the_books(pool: PgPool) {
    let h = harness(pool).await;
    let (_ws, owner) = h.app.workspace_with_owner("owls").await;
    let (_other, stranger) = h.app.workspace_with_owner("larks").await;
    {
        let mut w = h.world.lock().unwrap();
        w.contacts
            .insert("contact-hall".into(), "The Grand Hall".into());
        w.contacts
            .insert("contact-new".into(), "Someone New".into());
        seed_past(
            &mut w,
            "contact-hall",
            "OLD-1",
            200,
            60_000,
            10_000,
            "AUTHORISED",
        );
        seed_past(&mut w, "contact-new", "OLD-2", 150, 25_000, 0, "PAID");
        seed_past(&mut w, "contact-new", "OLD-3", 100, 5_000, 0, "VOIDED");
    }
    let hall = h.organisation(&owner, "The Grand Hall").await;

    let none = h.get(&owner, "/api/v1/accounting").await;
    assert_eq!(none["connection"], Value::Null);
    assert_eq!(none["xero"], true);
    assert_eq!(none["quickbooks"], true);

    // Connecting: the organisation chosen last is the one connected.
    let c = h.connect(&owner, "xero").await;
    assert_eq!(c["provider"], "xero");
    assert_eq!(c["tenant_name"], "Owls Agency Ltd");
    assert_eq!(c["ready"], false);
    assert_eq!(c["start_date"], Utc::now().date_naive().to_string());
    let id = c["id"].as_str().unwrap().to_owned();
    // Another workspace sees nothing of it.
    assert_eq!(h.accounting(&stranger).await, Value::Null);
    // Nothing is sent before it is said where sales go.
    let first = h.issue(&owner, hall, 100_000).await;
    h.drain().await;
    assert_eq!(h.world.lock().unwrap().added, 0);

    // Choosing: revenue accounts by code, banks, active tax rates.
    let o = h.get(&owner, "/api/v1/accounting/options").await;
    assert_eq!(o["sales"], json!([{ "id": "200", "name": "200 · Sales" }]));
    assert_eq!(
        o["banks"],
        json!([{ "id": "bank-1", "name": "090 · Business Bank" }])
    );
    assert_eq!(
        o["taxes"][0],
        json!({ "id": "OUTPUT2", "name": "20% (VAT on Income) (20.0000%)" })
    );
    assert_eq!(o["warnings"], json!([]));
    let saved = choose(&h, &owner, "200", Some("bank-1"), |rate| {
        Some(if rate > 0 { "OUTPUT2" } else { "NONE" })
    })
    .await;
    assert_eq!(saved["ready"], true);
    h.drain().await;

    // The invoice is sent, to the contact the books already had.
    let (first_id, doc) = h.in_books(&first);
    assert_eq!(doc.contact, "contact-hall");
    assert_eq!(doc.total, first["total"]["minor"].as_i64().unwrap());
    assert_eq!(doc.total, 120_000);
    assert_eq!(doc.body["LineItems"][0]["TaxType"], "OUTPUT2");
    assert_eq!(
        doc.body["LineItems"][0]["Description"],
        "Evening performance"
    );
    assert_eq!(doc.body["DueDate"], first["due_date"]);
    let c = h.accounting(&owner).await;
    assert_eq!(c["status"], "active");
    assert!(c["last_synced_at"].is_string());
    assert_eq!(c["refused"], 0);

    // A payment is sent, and taken back when voided.
    let paid = h
        .post(
            &owner,
            "/api/v1/payments",
            json!({ "amount": { "minor": 50_000, "currency": "GBP" }, "reference": "BACS 1",
                    "allocations": [{ "invoice_id": first["id"] }] }),
        )
        .await;
    h.drain().await;
    {
        let w = h.world.lock().unwrap();
        assert_eq!(w.payments.len(), 1);
        let (invoice, amount, body) = w.payments.values().next().unwrap();
        assert_eq!(invoice, &first_id);
        assert_eq!(*amount, 50_000);
        assert_eq!(body["Reference"], "BACS 1");
    }
    assert_eq!(h.doc(&first_id).due, 70_000);
    let payment = paid["id"].as_str().unwrap();
    h.post(
        &owner,
        &format!("/api/v1/payments/{payment}/void"),
        json!({ "reason": "Bounced" }),
    )
    .await;
    h.drain().await;
    assert!(h.world.lock().unwrap().payments.is_empty());
    assert_eq!(h.doc(&first_id).due, 120_000);

    // Paid in full, and Xero's answer lost: asked for again with the same
    // key, so it is there once and known, and taken back when voided.
    h.world.lock().unwrap().lose_next_payment = true;
    let whole = h
        .post(
            &owner,
            "/api/v1/payments",
            json!({ "amount": { "minor": 120_000, "currency": "GBP" },
                    "allocations": [{ "invoice_id": first["id"] }] }),
        )
        .await;
    h.drain().await;
    assert!(h.accounting(&owner).await["last_error"].is_string());
    h.sync(&owner).await;
    assert_eq!(h.world.lock().unwrap().payments.len(), 1);
    assert_eq!(h.doc(&first_id).due, 0);
    h.post(
        &owner,
        &format!("/api/v1/payments/{}/void", whole["id"].as_str().unwrap()),
        json!({ "reason": "Entered twice" }),
    )
    .await;
    h.drain().await;
    assert!(h.world.lock().unwrap().payments.is_empty());
    assert_eq!(h.doc(&first_id).due, 120_000);

    // Lost again, and the bank account cleared before it could be asked
    // for again: kept as on its way (it is in Xero), not dropped when
    // voided, and taken back once it can be asked for just so.
    let tax: fn(i64) -> Option<&'static str> =
        |rate| Some(if rate > 0 { "OUTPUT2" } else { "NONE" });
    h.world.lock().unwrap().lose_next_payment = true;
    let again = h
        .post(
            &owner,
            "/api/v1/payments",
            json!({ "amount": { "minor": 120_000, "currency": "GBP" },
                    "allocations": [{ "invoice_id": first["id"] }] }),
        )
        .await;
    h.drain().await;
    choose(&h, &owner, "200", None, tax).await;
    h.post(
        &owner,
        &format!("/api/v1/payments/{}/void", again["id"].as_str().unwrap()),
        json!({ "reason": "Entered twice" }),
    )
    .await;
    h.drain().await;
    h.sync(&owner).await;
    assert_eq!(h.world.lock().unwrap().payments.len(), 1, "still in Xero");
    choose(&h, &owner, "200", Some("bank-1"), tax).await;
    h.drain().await;
    assert!(h.world.lock().unwrap().payments.is_empty());
    assert_eq!(h.doc(&first_id).due, 120_000);

    // Paid in Xero: read back, recorded as paid here, never sent back.
    {
        let mut w = h.world.lock().unwrap();
        let d = w.docs.get_mut(&first_id).unwrap();
        d.due = 0;
        d.status = "PAID".into();
    }
    h.sync(&owner).await;
    let first_now = h
        .get(
            &owner,
            &format!("/api/v1/invoices/{}", first["id"].as_str().unwrap()),
        )
        .await;
    assert_eq!(first_now["status"], "paid", "{first_now}");
    let reference: String = sqlx::query_scalar(
        "SELECT reference FROM payments WHERE reference LIKE 'Paid in%' AND voided_at IS NULL",
    )
    .fetch_one(&h.app.owner)
    .await
    .unwrap();
    assert_eq!(reference, "Paid in Xero");
    h.sync(&owner).await;
    assert!(h.world.lock().unwrap().payments.is_empty());

    // A credit note is sent and set against its invoice; voided, it is
    // taken off again.
    let second = h.issue(&owner, hall, 50_000).await;
    h.drain().await;
    let (second_id, _) = h.in_books(&second);
    let note = h
        .post(
            &owner,
            &format!(
                "/api/v1/invoices/{}/credit-notes",
                second["id"].as_str().unwrap()
            ),
            json!({ "line_items": [{ "description": "Discount", "unit_minor": 10_000 }] }),
        )
        .await;
    h.drain().await;
    let (note_id, note_doc) = h.in_books(&note);
    assert!(note_doc.credit);
    assert_eq!(note_doc.total, 12_000);
    assert_eq!(h.doc(&second_id).due, 48_000);
    assert_eq!(h.world.lock().unwrap().allocations.len(), 1);
    h.post(
        &owner,
        &format!("/api/v1/invoices/{}/void", note["id"].as_str().unwrap()),
        json!({ "reason": "Agreed in error" }),
    )
    .await;
    h.drain().await;
    assert!(h.world.lock().unwrap().allocations.is_empty());
    assert_eq!(h.doc(&note_id).status, "VOIDED");
    assert_eq!(h.doc(&second_id).due, 60_000);

    // Voided after it was sent: voided there (already voided there is
    // fine too). Voided before: never sent.
    h.world
        .lock()
        .unwrap()
        .docs
        .get_mut(&second_id)
        .unwrap()
        .status = "VOIDED".into();
    h.post(
        &owner,
        &format!("/api/v1/invoices/{}/void", second["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    let never = h.issue(&owner, hall, 1_000).await;
    h.post(
        &owner,
        &format!("/api/v1/invoices/{}/void", never["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    h.drain().await;
    assert_eq!(h.doc(&second_id).status, "VOIDED");
    assert_eq!(h.accounting(&owner).await["refused"], 0);
    assert!(
        h.world
            .lock()
            .unwrap()
            .doc_numbered(never["reference"].as_str().unwrap())
            .is_empty()
    );

    // Refused: kept with the reason, sent once put right.
    let held = h.organisation(&owner, "Hold Fast Ltd").await;
    h.world.lock().unwrap().refuse = Some("Hold Fast Ltd".into());
    let refused = h.issue(&owner, held, 2_000).await;
    h.drain().await;
    let c = h.accounting(&owner).await;
    assert_eq!(c["refused"], 1);
    assert_eq!(
        c["status"], "active",
        "a refusal is not the connection failing"
    );
    let failures = h.get(&owner, "/api/v1/accounting/failures").await;
    assert_eq!(failures[0]["kind"], "invoice");
    assert_eq!(failures[0]["reference"], refused["reference"]);
    assert!(
        failures[0]["error"]
            .as_str()
            .unwrap()
            .contains("on hold in these books"),
        "{failures}"
    );
    assert_eq!(failures[0]["attempts"], 1);
    // Other work goes on; the refusal is tried again on "send now", not on
    // every change.
    let meanwhile = h.issue(&owner, hall, 2_500).await;
    h.drain().await;
    h.in_books(&meanwhile);
    let failures = h.get(&owner, "/api/v1/accounting/failures").await;
    assert_eq!(failures[0]["attempts"], 1, "{failures}");
    h.world.lock().unwrap().refuse = None;
    h.sync(&owner).await;
    assert_eq!(h.accounting(&owner).await["refused"], 0);
    let (_, doc) = h.in_books(&refused);
    assert_eq!(
        h.world.lock().unwrap().contacts[&doc.contact],
        "Hold Fast Ltd"
    );

    // An answer lost after Xero added the invoice: tried again with the same
    // key, so it is there once.
    h.world.lock().unwrap().lose_next = true;
    let lost = h.issue(&owner, hall, 3_000).await;
    h.drain().await;
    let c = h.accounting(&owner).await;
    assert!(c["last_error"].is_string(), "{c}");
    h.sync(&owner).await;
    h.in_books(&lost);
    assert_eq!(h.accounting(&owner).await["last_error"], Value::Null);

    // A sync cut off part way (its worker gone): a change meanwhile waits
    // for it, and once its lease runs out another sync follows.
    sqlx::query(
        "UPDATE accounting_connections SET sync_token = gen_random_uuid(),
                sync_lease_until = now() + interval '10 minutes',
                next_sync_at = now() + interval '10 minutes'",
    )
    .execute(&h.app.owner)
    .await
    .unwrap();
    let late = h.issue(&owner, hall, 4_000).await;
    h.sync(&owner).await;
    assert!(
        h.world
            .lock()
            .unwrap()
            .doc_numbered(late["reference"].as_str().unwrap())
            .is_empty()
    );
    sqlx::query(
        "UPDATE accounting_connections SET sync_lease_until = now() - interval '1 second',
                next_sync_at = now() - interval '1 second'",
    )
    .execute(&h.app.owner)
    .await
    .unwrap();
    h.drain().await;
    h.in_books(&late);

    // Past invoices: those before the start date, with what they still owe,
    // matched to the organisation; never sent back; imported once.
    let added = h.world.lock().unwrap().added;
    let c = h.post(&owner, "/api/v1/accounting/import", json!({})).await;
    assert_eq!(c["import"]["status"], "running");
    h.drain().await;
    let c = h.accounting(&owner).await;
    assert_eq!(
        c["import"],
        json!({ "status": "done", "imported": 2, "error": null })
    );
    let list = h.get(&owner, "/api/v1/invoices?limit=200").await;
    let named = |reference: &str| {
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["reference"] == reference)
            .cloned()
    };
    let old1 = named("OLD-1").expect("OLD-1 imported");
    assert_eq!(old1["payer_organisation_id"], hall.to_string());
    assert_eq!(old1["total"]["minor"], 60_000);
    assert_eq!(old1["outstanding"]["minor"], 10_000);
    assert!(old1["notes"].as_str().unwrap().contains("Spring tour"));
    let old2 = named("OLD-2").expect("OLD-2 imported");
    assert_eq!(old2["payer_name"], "Someone New");
    assert_eq!(old2["status"], "paid");
    assert!(named("OLD-3").is_none(), "voided ones are left out");
    h.sync(&owner).await;
    assert_eq!(h.world.lock().unwrap().added, added);
    h.post(&owner, "/api/v1/accounting/import", json!({})).await;
    h.drain().await;
    assert_eq!(h.accounting(&owner).await["import"]["imported"], 0);

    // The grant withdrawn in Xero: waits to be connected again, then picks
    // up where it was, sending nothing twice.
    h.world.lock().unwrap().withdrawn = true;
    h.sync(&owner).await;
    let c = h.accounting(&owner).await;
    assert_eq!(c["status"], "reconnect");
    h.app
        .post("/api/v1/accounting/sync")
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::CONFLICT, "reconnect_required");
    h.world.lock().unwrap().withdrawn = false;
    let again = h.connect(&owner, "xero").await;
    assert_eq!(again["id"], id.as_str());
    assert_eq!(again["status"], "active");
    h.sync(&owner).await;
    assert_eq!(h.world.lock().unwrap().added, added);
    assert!(h.world.lock().unwrap().revoked.is_empty());

    // Connecting other books while these are connected is refused before
    // anything is granted.
    h.app
        .post("/api/v1/accounting/start")
        .signed_in(&owner)
        .json(json!({ "provider": "quickbooks" }))
        .send()
        .await
        .expect(StatusCode::CONFLICT, "already_connected");

    // Another workspace connects the same organisation.
    let theirs = h.connect(&stranger, "xero").await;
    assert_eq!(theirs["tenant_name"], "Owls Agency Ltd");

    // Disconnecting: the grant is forgotten. The organisation stays
    // connected while the other workspace uses it; once that one
    // disconnects too, only the connection to that organisation is ended
    // (revoking the grant would end the person's others too).
    for who in [&owner, &stranger] {
        h.app
            .delete("/api/v1/accounting")
            .signed_in(who)
            .send()
            .await
            .ok();
        h.drain().await;
        let w = h.world.lock().unwrap();
        assert!(w.revoked.is_empty());
        let expected: &[&str] = if std::ptr::eq(who, &owner) {
            &[]
        } else {
            &["conn-1"]
        };
        assert_eq!(w.disconnected, expected);
    }
    assert_eq!(h.accounting(&owner).await, Value::Null);
    let kept: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM accounting_connections WHERE length(secret) > 0 OR revoke_at IS NOT NULL",
    )
    .fetch_one(&h.app.owner)
    .await
    .unwrap();
    assert_eq!(kept, 0);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn quickbooks_keeps_the_books(pool: PgPool) {
    let h = harness(pool).await;
    let (_ws, owner) = h.app.workspace_with_owner("owls").await;
    {
        let mut w = h.world.lock().unwrap();
        w.contacts.insert("cust-old".into(), "Old Client".into());
        // Made inactive in QuickBooks: found and made active again.
        w.contacts
            .insert("cust-hall".into(), "The Grand Hall".into());
        w.inactive.insert("cust-hall".into());
        seed_past(&mut w, "cust-old", "1001", 120, 30_000, 0, "PAID");
    }
    let hall = h.organisation(&owner, "The Grand Hall").await;

    // The company comes from QuickBooks' redirect; US companies' tax is
    // worked out by QuickBooks.
    h.app
        .post("/api/v1/accounting/finish")
        .signed_in(&owner)
        .json(json!({ "code": "good-code", "state": "made-up" }))
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "connect_state_invalid");
    let c = h.connect(&owner, "quickbooks").await;
    assert_eq!(c["tenant_name"], "Owls Inc");
    assert_eq!(c["automatic_tax"], true);
    let o = h.get(&owner, "/api/v1/accounting/options").await;
    assert_eq!(o["sales"], json!([{ "id": "1", "name": "Services" }]));
    assert_eq!(o["banks"], json!([{ "id": "35", "name": "Checking" }]));
    let taxes: Vec<&str> = o["taxes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(taxes, ["TAX", "NON"]);
    assert_eq!(o["warnings"].as_array().unwrap().len(), 1, "{o}");
    // Tax codes left to OnHold's rates: taxable or not.
    choose(&h, &owner, "1", Some("35"), |_| None).await;

    let first = h.issue(&owner, hall, 100_000).await;
    h.drain().await;
    let (first_id, doc) = h.in_books(&first);
    assert_eq!(doc.total, 120_000);
    assert_eq!(doc.contact, "cust-hall");
    assert!(h.world.lock().unwrap().inactive.is_empty());
    let line = &doc.body["Line"][0];
    assert_eq!(line["SalesItemLineDetail"]["TaxCodeRef"]["value"], "TAX");
    assert_eq!(line["Description"], "Evening performance");

    h.post(
        &owner,
        "/api/v1/payments",
        json!({ "amount": { "minor": 50_000, "currency": "GBP" }, "reference": "Cheque 7",
                "allocations": [{ "invoice_id": first["id"] }] }),
    )
    .await;
    h.drain().await;
    {
        let w = h.world.lock().unwrap();
        let (invoice, amount, body) = w.payments.values().next().unwrap();
        assert_eq!(invoice, &first_id);
        assert_eq!(*amount, 50_000);
        assert_eq!(body["DepositToAccountRef"]["value"], "35");
        assert_eq!(body["PaymentRefNum"], "Cheque 7");
    }
    assert_eq!(h.doc(&first_id).due, 70_000);

    // Voided after it was sent.
    let second = h.issue(&owner, hall, 5_000).await;
    h.drain().await;
    let (second_id, _) = h.in_books(&second);
    h.post(
        &owner,
        &format!("/api/v1/invoices/{}/void", second["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    h.drain().await;
    assert_eq!(h.doc(&second_id).status, "VOIDED");
    // Deleted in QuickBooks before it was voided here: nothing to do.
    let third = h.issue(&owner, hall, 6_000).await;
    h.drain().await;
    let (third_id, _) = h.in_books(&third);
    h.world.lock().unwrap().docs.remove(&third_id);
    h.post(
        &owner,
        &format!("/api/v1/invoices/{}/void", third["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    h.drain().await;
    assert_eq!(h.accounting(&owner).await["refused"], 0);

    // The rest paid in QuickBooks.
    h.world.lock().unwrap().docs.get_mut(&first_id).unwrap().due = 0;
    h.sync(&owner).await;
    let first_now = h
        .get(
            &owner,
            &format!("/api/v1/invoices/{}", first["id"].as_str().unwrap()),
        )
        .await;
    assert_eq!(first_now["status"], "paid", "{first_now}");
    assert_eq!(h.world.lock().unwrap().payments.len(), 1);

    // The customer made inactive in QuickBooks once linked: taking a
    // payment back is refused (Intuit answers as for one not there, but it
    // is), and a new invoice too, until a retry makes the customer active.
    let fourth = h.issue(&owner, hall, 7_000).await;
    h.drain().await;
    let (fourth_id, _) = h.in_books(&fourth);
    let paid = h
        .post(
            &owner,
            "/api/v1/payments",
            json!({ "amount": { "minor": 1_000, "currency": "GBP" },
                    "allocations": [{ "invoice_id": fourth["id"] }] }),
        )
        .await;
    h.drain().await;
    assert_eq!(h.doc(&fourth_id).due, 7_400);
    h.world.lock().unwrap().inactive.insert("cust-hall".into());
    h.post(
        &owner,
        &format!("/api/v1/payments/{}/void", paid["id"].as_str().unwrap()),
        json!({}),
    )
    .await;
    let fifth = h.issue(&owner, hall, 8_000).await;
    h.drain().await;
    assert_eq!(
        h.world.lock().unwrap().payments.len(),
        2,
        "still in the books"
    );
    assert_eq!(h.accounting(&owner).await["refused"], 2);
    h.sync(&owner).await;
    assert!(h.world.lock().unwrap().inactive.is_empty());
    h.in_books(&fifth);
    assert_eq!(h.doc(&fourth_id).due, 8_400);
    assert_eq!(h.accounting(&owner).await["refused"], 0);

    // Past invoices.
    h.post(&owner, "/api/v1/accounting/import", json!({})).await;
    h.drain().await;
    let c = h.accounting(&owner).await;
    assert_eq!(c["import"]["status"], "done");
    assert_eq!(c["import"]["imported"], 1);

    // Disconnecting revokes the grant (Intuit takes JSON).
    h.app
        .delete("/api/v1/accounting")
        .signed_in(&owner)
        .send()
        .await
        .ok();
    h.drain().await;
    let w = h.world.lock().unwrap();
    assert_eq!(w.revoked, std::slice::from_ref(&w.refresh));
}
