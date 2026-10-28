//! A workspace's books in Xero or QuickBooks Online. OnHold sends what it
//! issues there (customers, invoices, credit notes and the payments it
//! records), reads back what was paid there, and reads past invoices to
//! import them. Creates carry a key the provider remembers, so trying
//! again after a lost answer never makes a second copy.

use chrono::NaiveDate;
use serde_json::{Value, json};
use url::Url;

use crate::provider::{self, ProviderError, endpoint, segment};

pub const XERO_API: &str = "https://api.xero.com";
pub const QUICKBOOKS_API: &str = "https://quickbooks.api.intuit.com";
pub const QUICKBOOKS_SANDBOX_API: &str = "https://sandbox-quickbooks.api.intuit.com";

/// Xero's granular scopes (apps made since March 2026 have no others):
/// invoices and credit notes, payments, contacts, and reading accounts and
/// tax rates.
pub const XERO_SCOPES: &[&str] = &[
    "offline_access",
    "accounting.invoices",
    "accounting.payments",
    "accounting.contacts",
    "accounting.settings.read",
];
pub const QUICKBOOKS_SCOPES: &[&str] = &["com.intuit.quickbooks.accounting"];

/// Intuit's API version OnHold is written against.
const QUICKBOOKS_MINOR_VERSION: &str = "75";
/// Invoices asked about in one call.
pub const STATE_BATCH: usize = 50;
/// Past invoices read in one call.
pub const PAGE_SIZE: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Xero,
    QuickBooks,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "xero" => Some(Self::Xero),
            "quickbooks" => Some(Self::QuickBooks),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Xero => "xero",
            Self::QuickBooks => "quickbooks",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Xero => "Xero",
            Self::QuickBooks => "QuickBooks",
        }
    }
}

/// An amount in a currency's minor units, as the providers write it.
fn decimal(minor: i64, exponent: u32) -> Value {
    json!(minor as f64 / 10f64.powi(exponent as i32))
}

fn minor(v: Option<&Value>, exponent: u32) -> i64 {
    let n = v
        .and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0.0);
    (n * 10f64.powi(exponent as i32)).round() as i64
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| {
            x.as_str()
                .map(str::to_owned)
                .or_else(|| x.as_i64().map(|n| n.to_string()))
        })
        .filter(|s| !s.is_empty())
}

fn date(v: &Value, key: &str) -> Option<NaiveDate> {
    v.get(key)
        .and_then(Value::as_str)
        .and_then(|s| s.get(..10))
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
}

fn invalid(what: &str) -> ProviderError {
    ProviderError::Invalid(format!("no {what} in the answer"))
}

/// A Xero organisation the grant reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tenant {
    pub id: String,
    pub name: String,
    /// When it was connected, so the one just chosen can be told apart.
    pub updated: String,
}

/// Something to choose: an account, an item, a tax rate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub id: String,
    pub name: String,
}

/// What a workspace chooses from when setting up.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// Xero: revenue accounts, by code. QuickBooks: service items.
    pub sales: Vec<Choice>,
    /// Accounts payments are paid into.
    pub banks: Vec<Choice>,
    /// Xero: tax types. QuickBooks: tax codes.
    pub taxes: Vec<Choice>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub description: String,
    pub quantity: f64,
    pub unit_minor: i64,
    /// Net of tax.
    pub amount_minor: i64,
    /// The provider's tax type or code, when one is chosen for it.
    pub tax: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    Invoice,
    CreditNote,
}

/// An invoice or credit note as issued.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub kind: DocKind,
    /// The provider's contact (customer) id.
    pub contact: String,
    pub number: String,
    pub date: NaiveDate,
    pub due: Option<NaiveDate>,
    pub currency: String,
    pub exponent: u32,
    pub lines: Vec<Line>,
    /// Xero: the revenue account's code. QuickBooks: the service item.
    pub sales: String,
    /// QuickBooks works out the sales tax itself (US companies).
    pub automatic_tax: bool,
}

/// A payment against one invoice.
#[derive(Debug, Clone, PartialEq)]
pub struct Payment {
    pub invoice: String,
    pub contact: String,
    pub amount_minor: i64,
    pub exponent: u32,
    pub date: NaiveDate,
    /// The account it was paid into (Xero needs one).
    pub account: Option<String>,
    pub reference: Option<String>,
}

/// Where an invoice stands in the books.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceState {
    pub id: String,
    pub total_minor: i64,
    /// What it still owes there.
    pub due_minor: i64,
    pub voided: bool,
}

/// An invoice from the books, to import.
#[derive(Debug, Clone, PartialEq)]
pub struct PastInvoice {
    pub id: String,
    pub number: String,
    pub contact_id: Option<String>,
    pub contact_name: String,
    pub date: NaiveDate,
    pub due: Option<NaiveDate>,
    pub currency: String,
    pub total_minor: i64,
    pub tax_minor: i64,
    pub due_minor: i64,
    pub lines: Vec<PastLine>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PastLine {
    pub description: String,
    pub quantity: f64,
    pub unit_minor: i64,
    pub amount_minor: i64,
}

/// One page of past invoices, and whether more follow.
#[derive(Debug, Clone, PartialEq)]
pub struct PastPage {
    pub invoices: Vec<PastInvoice>,
    pub more: bool,
}

/// A client for one connected organisation (Xero) or company (QuickBooks).
#[derive(Debug, Clone)]
pub struct Books {
    kind: Kind,
    http: reqwest::Client,
    base: Url,
    token: String,
    /// Xero's tenant id; QuickBooks' realm (company) id.
    tenant: String,
    /// Currencies' digits after the point, from the caller.
    exponent: fn(&str) -> u32,
}

/// The Xero organisations a new grant reaches, the one chosen last first.
pub async fn xero_tenants(
    http: &reqwest::Client,
    base: &Url,
    token: &str,
) -> Result<Vec<Tenant>, ProviderError> {
    let v: Value = provider::send_json(
        http.get(endpoint(base, "/connections"))
            .bearer_auth(token)
            .header("accept", "application/json"),
    )
    .await?;
    let mut tenants: Vec<Tenant> = v
        .as_array()
        .ok_or_else(|| invalid("connections"))?
        .iter()
        .filter(|t| t.get("tenantType").and_then(Value::as_str) == Some("ORGANISATION"))
        .filter_map(|t| {
            Some(Tenant {
                id: text(t, "tenantId")?,
                name: text(t, "tenantName").unwrap_or_else(|| "Xero organisation".into()),
                updated: text(t, "updatedDateUtc")
                    .or_else(|| text(t, "createdDateUtc"))
                    .unwrap_or_default(),
            })
        })
        .collect();
    tenants.sort_by(|a, b| b.updated.cmp(&a.updated));
    Ok(tenants)
}

/// Ends the grant's connection to one Xero organisation only: revoking the
/// grant itself would end its connections to every organisation.
pub async fn xero_disconnect(
    http: &reqwest::Client,
    base: &Url,
    token: &str,
    tenant_id: &str,
) -> Result<(), ProviderError> {
    let v: Value = provider::send_json(
        http.get(endpoint(base, "/connections"))
            .bearer_auth(token)
            .header("accept", "application/json"),
    )
    .await?;
    let Some(id) = v
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| text(c, "tenantId").as_deref() == Some(tenant_id))
        .and_then(|c| text(c, "id"))
    else {
        return Ok(());
    };
    match provider::send_raw(
        http.delete(endpoint(base, &format!("/connections/{}", segment(&id))))
            .bearer_auth(token),
    )
    .await
    {
        Ok(_) | Err(ProviderError::Refused { status: 404, .. }) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Intuit's names are at most 100 characters.
fn quickbooks_name(name: &str) -> String {
    name.trim().chars().take(100).collect()
}

/// A quote for Intuit's query language.
fn quoted(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

impl Books {
    pub fn new(
        kind: Kind,
        http: reqwest::Client,
        base: Url,
        token: String,
        tenant: String,
        exponent: fn(&str) -> u32,
    ) -> Self {
        Self {
            kind,
            http,
            base,
            token,
            tenant,
            exponent,
        }
    }

    fn url(&self, path: &str) -> Url {
        match self.kind {
            Kind::Xero => endpoint(&self.base, &format!("/api.xro/2.0{path}")),
            Kind::QuickBooks => {
                let mut url = endpoint(
                    &self.base,
                    &format!("/v3/company/{}{path}", segment(&self.tenant)),
                );
                url.query_pairs_mut()
                    .append_pair("minorversion", QUICKBOOKS_MINOR_VERSION);
                url
            }
        }
    }

    fn request(&self, method: reqwest::Method, url: Url) -> reqwest::RequestBuilder {
        let req = self
            .http
            .request(method, url)
            .bearer_auth(&self.token)
            .header("accept", "application/json");
        match self.kind {
            Kind::Xero => req.header("xero-tenant-id", &self.tenant),
            Kind::QuickBooks => req,
        }
    }

    async fn get(&self, path: &str) -> Result<Value, ProviderError> {
        provider::send_json(self.request(reqwest::Method::GET, self.url(path))).await
    }

    /// Sends a write. `key` makes a create safe to try again: Xero's
    /// `Idempotency-Key`, Intuit's `requestid`.
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &Value,
        key: Option<&str>,
    ) -> Result<Value, ProviderError> {
        let mut url = self.url(path);
        let mut req = match (self.kind, key) {
            (Kind::QuickBooks, Some(k)) => {
                url.query_pairs_mut().append_pair("requestid", k);
                self.request(method, url)
            }
            (Kind::Xero, Some(k)) => self.request(method, url).header("idempotency-key", k),
            (_, None) => self.request(method, url),
        };
        req = req
            .header("content-type", "application/json")
            .body(body.to_string());
        provider::send_json(req).await
    }

    /// Runs an Intuit query and returns the rows of `entity`.
    async fn query(&self, entity: &str, q: &str) -> Result<Vec<Value>, ProviderError> {
        let mut url = self.url("/query");
        url.query_pairs_mut().append_pair("query", q);
        let v: Value = provider::send_json(self.request(reqwest::Method::GET, url)).await?;
        Ok(v.pointer(&format!("/QueryResponse/{entity}"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// The organisation's or company's name.
    pub async fn name(&self) -> Result<String, ProviderError> {
        match self.kind {
            Kind::Xero => {
                let v = self.get("/Organisation").await?;
                v.pointer("/Organisations/0/Name")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| invalid("organisation"))
            }
            Kind::QuickBooks => {
                let v = self
                    .get(&format!("/companyinfo/{}", segment(&self.tenant)))
                    .await?;
                v.pointer("/CompanyInfo/CompanyName")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| invalid("company"))
            }
        }
    }

    /// Whether QuickBooks works out sales tax itself (US companies), which
    /// then takes only "taxable" or "not taxable" per line.
    pub async fn automatic_tax(&self) -> Result<bool, ProviderError> {
        match self.kind {
            Kind::Xero => Ok(false),
            Kind::QuickBooks => {
                let v = self
                    .get(&format!("/companyinfo/{}", segment(&self.tenant)))
                    .await?;
                Ok(v.pointer("/CompanyInfo/Country").and_then(Value::as_str) == Some("US"))
            }
        }
    }

    /// The accounts, items and tax rates a workspace picks from.
    pub async fn options(&self) -> Result<Options, ProviderError> {
        match self.kind {
            Kind::Xero => {
                let accounts = self.get("/Accounts").await?;
                let accounts = accounts
                    .get("Accounts")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let active = |a: &&Value| a.get("Status").and_then(Value::as_str) == Some("ACTIVE");
                let label = |a: &Value| {
                    let name = text(a, "Name").unwrap_or_default();
                    match text(a, "Code") {
                        Some(c) => format!("{c} · {name}"),
                        None => name,
                    }
                };
                let sales = accounts
                    .iter()
                    .filter(active)
                    .filter(|a| a.get("Class").and_then(Value::as_str) == Some("REVENUE"))
                    .filter_map(|a| {
                        Some(Choice {
                            id: text(a, "Code")?,
                            name: label(a),
                        })
                    })
                    .collect();
                let banks = accounts
                    .iter()
                    .filter(active)
                    .filter(|a| {
                        a.get("Type").and_then(Value::as_str) == Some("BANK")
                            || a.get("EnablePaymentsToAccount").and_then(Value::as_bool)
                                == Some(true)
                    })
                    .filter_map(|a| {
                        Some(Choice {
                            id: text(a, "AccountID")?,
                            name: label(a),
                        })
                    })
                    .collect();
                let rates = self.get("/TaxRates").await?;
                let taxes = rates
                    .get("TaxRates")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|t| t.get("Status").and_then(Value::as_str) == Some("ACTIVE"))
                    .filter_map(|t| {
                        let rate = t
                            .get("EffectiveRate")
                            .or_else(|| t.get("DisplayTaxRate"))
                            .map(|r| match r {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            })
                            .unwrap_or_default();
                        Some(Choice {
                            id: text(t, "TaxType")?,
                            name: format!("{} ({rate}%)", text(t, "Name").unwrap_or_default()),
                        })
                    })
                    .collect();
                Ok(Options {
                    sales,
                    banks,
                    taxes,
                })
            }
            Kind::QuickBooks => {
                let choice = |r: &Value| {
                    Some(Choice {
                        id: text(r, "Id")?,
                        name: text(r, "FullyQualifiedName")
                            .or_else(|| text(r, "Name"))
                            .unwrap_or_default(),
                    })
                };
                let sales = self
                    .query(
                        "Item",
                        "select * from Item where Type = 'Service' and Active = true maxresults 1000",
                    )
                    .await?
                    .iter()
                    .filter_map(choice)
                    .collect();
                let banks = self
                    .query(
                        "Account",
                        "select * from Account where AccountType = 'Bank' and Active = true maxresults 1000",
                    )
                    .await?
                    .iter()
                    .filter_map(choice)
                    .collect();
                let taxes = if self.automatic_tax().await? {
                    vec![
                        Choice {
                            id: "TAX".into(),
                            name: "Taxable (QuickBooks works out the sales tax)".into(),
                        },
                        Choice {
                            id: "NON".into(),
                            name: "Not taxable".into(),
                        },
                    ]
                } else {
                    self.query(
                        "TaxCode",
                        "select * from TaxCode where Active = true maxresults 1000",
                    )
                    .await?
                    .iter()
                    .filter_map(choice)
                    .collect()
                };
                Ok(Options {
                    sales,
                    banks,
                    taxes,
                })
            }
        }
    }

    /// The contact (customer) of this name, if the books have one.
    pub async fn find_contact(&self, name: &str) -> Result<Option<String>, ProviderError> {
        let wanted = name.trim().to_lowercase();
        match self.kind {
            Kind::Xero => {
                let mut url = self.url("/Contacts");
                url.query_pairs_mut()
                    .append_pair("searchTerm", name.trim())
                    .append_pair("summaryOnly", "true");
                let v: Value = provider::send_json(self.request(reqwest::Method::GET, url)).await?;
                Ok(v.get("Contacts")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|c| c.get("ContactStatus").and_then(Value::as_str) != Some("ARCHIVED"))
                    .find(|c| text(c, "Name").is_some_and(|n| n.trim().to_lowercase() == wanted))
                    .and_then(|c| text(c, "ContactID")))
            }
            Kind::QuickBooks => {
                // Made inactive in QuickBooks, the name is still taken:
                // such a customer is made active again.
                let rows = self
                    .query(
                        "Customer",
                        &format!(
                            "select * from Customer where DisplayName = {} and Active in (true, false)",
                            quoted(&quickbooks_name(name))
                        ),
                    )
                    .await?;
                let Some(c) = rows.first() else {
                    return Ok(None);
                };
                let id = text(c, "Id").ok_or_else(|| invalid("customer"))?;
                if c.get("Active").and_then(Value::as_bool) == Some(false) {
                    let token = text(c, "SyncToken").ok_or_else(|| invalid("sync token"))?;
                    self.send(
                        reqwest::Method::POST,
                        "/customer",
                        &json!({ "Id": id, "SyncToken": token, "sparse": true, "Active": true }),
                        None,
                    )
                    .await?;
                }
                Ok(Some(id))
            }
        }
    }

    /// Adds a contact (customer). `key` makes trying again safe.
    pub async fn create_contact(
        &self,
        name: &str,
        email: Option<&str>,
        key: &str,
    ) -> Result<String, ProviderError> {
        match self.kind {
            Kind::Xero => {
                let mut c = json!({ "Name": name.trim() });
                if let Some(e) = email {
                    c["EmailAddress"] = json!(e);
                }
                let v = self
                    .send(
                        reqwest::Method::PUT,
                        "/Contacts",
                        &json!({ "Contacts": [c] }),
                        Some(key),
                    )
                    .await?;
                v.pointer("/Contacts/0/ContactID")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| invalid("contact"))
            }
            Kind::QuickBooks => {
                let mut c = json!({ "DisplayName": quickbooks_name(name) });
                if let Some(e) = email {
                    c["PrimaryEmailAddr"] = json!({ "Address": e });
                }
                let v = self
                    .send(reqwest::Method::POST, "/customer", &c, Some(key))
                    .await?;
                v.pointer("/Customer/Id")
                    .and_then(|v| text(&json!({ "Id": v }), "Id"))
                    .ok_or_else(|| invalid("customer"))
            }
        }
    }

    fn xero_lines(&self, d: &Document) -> Value {
        let exp = d.exponent;
        let lines: Vec<Value> = d
            .lines
            .iter()
            .map(|l| {
                // Xero multiplies quantity by unit price itself; when that
                // would not give OnHold's amount, one of the whole amount.
                let exact = (l.quantity * l.unit_minor as f64).round() as i64 == l.amount_minor;
                let (qty, unit) = if exact {
                    (l.quantity, l.unit_minor)
                } else {
                    (1.0, l.amount_minor)
                };
                let mut line = json!({
                    "Description": l.description,
                    "Quantity": qty,
                    "UnitAmount": decimal(unit, exp),
                    "AccountCode": d.sales,
                });
                if let Some(t) = &l.tax {
                    line["TaxType"] = json!(t);
                }
                line
            })
            .collect();
        json!(lines)
    }

    fn quickbooks_lines(&self, d: &Document) -> Value {
        let exp = d.exponent;
        let lines: Vec<Value> = d
            .lines
            .iter()
            .map(|l| {
                let exact = (l.quantity * l.unit_minor as f64).round() as i64 == l.amount_minor;
                let (qty, unit) = if exact {
                    (l.quantity, l.unit_minor)
                } else {
                    (1.0, l.amount_minor)
                };
                let mut detail = json!({
                    "ItemRef": { "value": d.sales },
                    "Qty": qty,
                    "UnitPrice": decimal(unit, exp),
                });
                if let Some(t) = &l.tax {
                    detail["TaxCodeRef"] = json!({ "value": t });
                }
                json!({
                    "DetailType": "SalesItemLineDetail",
                    "Amount": decimal(l.amount_minor, exp),
                    "Description": l.description,
                    "SalesItemLineDetail": detail,
                })
            })
            .collect();
        json!(lines)
    }

    /// Adds an issued invoice or credit note; returns its id.
    pub async fn create_document(&self, d: &Document, key: &str) -> Result<String, ProviderError> {
        match self.kind {
            Kind::Xero => {
                let (path, list, id, number) = match d.kind {
                    DocKind::Invoice => ("/Invoices", "Invoices", "InvoiceID", "InvoiceNumber"),
                    DocKind::CreditNote => (
                        "/CreditNotes",
                        "CreditNotes",
                        "CreditNoteID",
                        "CreditNoteNumber",
                    ),
                };
                let mut doc = json!({
                    "Type": if d.kind == DocKind::Invoice { "ACCREC" } else { "ACCRECCREDIT" },
                    "Contact": { "ContactID": d.contact },
                    "Date": d.date.to_string(),
                    "CurrencyCode": d.currency,
                    "Status": "AUTHORISED",
                    "LineAmountTypes": "Exclusive",
                    "LineItems": self.xero_lines(d),
                });
                doc[number] = json!(d.number);
                if let Some(due) = d.due {
                    doc["DueDate"] = json!(due.to_string());
                }
                let v = self
                    .send(
                        reqwest::Method::PUT,
                        path,
                        &json!({ list: [doc] }),
                        Some(key),
                    )
                    .await?;
                v.pointer(&format!("/{list}/0/{id}"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| invalid("document id"))
            }
            Kind::QuickBooks => {
                let (path, entity) = match d.kind {
                    DocKind::Invoice => ("/invoice", "Invoice"),
                    DocKind::CreditNote => ("/creditmemo", "CreditMemo"),
                };
                let mut doc = json!({
                    "CustomerRef": { "value": d.contact },
                    // Intuit's document numbers are at most 21 characters.
                    "DocNumber": d.number.chars().take(21).collect::<String>(),
                    "TxnDate": d.date.to_string(),
                    "CurrencyRef": { "value": d.currency },
                    "Line": self.quickbooks_lines(d),
                });
                // US companies' tax is worked out by QuickBooks and takes
                // no calculation method.
                if !d.automatic_tax {
                    doc["GlobalTaxCalculation"] = json!("TaxExcluded");
                }
                if let Some(due) = d.due {
                    doc["DueDate"] = json!(due.to_string());
                }
                let v = self
                    .send(reqwest::Method::POST, path, &doc, Some(key))
                    .await?;
                v.pointer(&format!("/{entity}/Id"))
                    .and_then(|v| text(&json!({ "Id": v }), "Id"))
                    .ok_or_else(|| invalid("document id"))
            }
        }
    }

    /// The SyncToken Intuit wants with any change to an entity.
    async fn sync_token(&self, entity: &str, id: &str) -> Result<String, ProviderError> {
        let v = self
            .get(&format!("/{}/{}", entity.to_ascii_lowercase(), segment(id)))
            .await?;
        v.get(entity)
            .and_then(|e| text(e, "SyncToken"))
            .ok_or_else(|| invalid("sync token"))
    }

    /// Whether Xero has this voided or deleted already: it refuses to do
    /// either twice.
    async fn xero_gone(&self, path: &str, list: &str, id: &str) -> Result<bool, ProviderError> {
        let v = match self.get(&format!("/{path}/{}", segment(id))).await {
            Err(ProviderError::Refused { status: 404, .. }) => return Ok(true),
            other => other?,
        };
        Ok(matches!(
            v.pointer(&format!("/{list}/0/Status"))
                .and_then(Value::as_str),
            Some("VOIDED" | "DELETED")
        ))
    }

    /// Voids an invoice or credit note. One already gone counts as done.
    pub async fn void_document(&self, kind: DocKind, id: &str) -> Result<(), ProviderError> {
        let done = match self.kind {
            Kind::Xero => {
                let (path, list, key) = match kind {
                    DocKind::Invoice => ("Invoices", "Invoices", "InvoiceID"),
                    DocKind::CreditNote => ("CreditNotes", "CreditNotes", "CreditNoteID"),
                };
                match self
                    .send(
                        reqwest::Method::POST,
                        &format!("/{path}/{}", segment(id)),
                        &json!({ list: [{ key: id, "Status": "VOIDED" }] }),
                        None,
                    )
                    .await
                {
                    Err(e @ ProviderError::Refused { status: 400, .. }) => {
                        if self.xero_gone(path, list, id).await? {
                            Ok(())
                        } else {
                            Err(e)
                        }
                    }
                    other => other.map(|_| ()),
                }
            }
            Kind::QuickBooks => {
                let entity = match kind {
                    DocKind::Invoice => "Invoice",
                    DocKind::CreditNote => "CreditMemo",
                };
                let token = match self.sync_token(entity, id).await {
                    Err(ProviderError::Refused { status: 404, .. }) => return Ok(()),
                    other => other?,
                };
                // Intuit voids invoices, and deletes credit memos.
                let op = match kind {
                    DocKind::Invoice => "void",
                    DocKind::CreditNote => "delete",
                };
                let mut url = self.url(&format!("/{}", entity.to_ascii_lowercase()));
                url.query_pairs_mut().append_pair("operation", op);
                provider::send_json::<Value>(
                    self.request(reqwest::Method::POST, url)
                        .header("content-type", "application/json")
                        .body(json!({ "Id": id, "SyncToken": token }).to_string()),
                )
                .await
                .map(|_| ())
            }
        };
        match done {
            Err(ProviderError::Refused { status: 404, .. }) => Ok(()),
            other => other,
        }
    }

    /// Records a payment against an invoice; returns its id.
    pub async fn create_payment(&self, p: &Payment, key: &str) -> Result<String, ProviderError> {
        match self.kind {
            Kind::Xero => {
                let account = p.account.as_deref().ok_or_else(|| ProviderError::Refused {
                    status: 400,
                    message: "choose the account payments are paid into".into(),
                })?;
                let mut pay = json!({
                    "Invoice": { "InvoiceID": p.invoice },
                    "Account": { "AccountID": account },
                    "Date": p.date.to_string(),
                    "Amount": decimal(p.amount_minor, p.exponent),
                });
                if let Some(r) = &p.reference {
                    pay["Reference"] = json!(r);
                }
                let v = self
                    .send(
                        reqwest::Method::PUT,
                        "/Payments",
                        &json!({ "Payments": [pay] }),
                        Some(key),
                    )
                    .await?;
                v.pointer("/Payments/0/PaymentID")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| invalid("payment id"))
            }
            Kind::QuickBooks => {
                let mut pay = json!({
                    "CustomerRef": { "value": p.contact },
                    "TotalAmt": decimal(p.amount_minor, p.exponent),
                    "TxnDate": p.date.to_string(),
                    "Line": [{
                        "Amount": decimal(p.amount_minor, p.exponent),
                        "LinkedTxn": [{ "TxnId": p.invoice, "TxnType": "Invoice" }],
                    }],
                });
                if let Some(a) = &p.account {
                    pay["DepositToAccountRef"] = json!({ "value": a });
                }
                if let Some(r) = &p.reference {
                    pay["PaymentRefNum"] = json!(r.chars().take(21).collect::<String>());
                }
                let v = self
                    .send(reqwest::Method::POST, "/payment", &pay, Some(key))
                    .await?;
                v.pointer("/Payment/Id")
                    .and_then(|v| text(&json!({ "Id": v }), "Id"))
                    .ok_or_else(|| invalid("payment id"))
            }
        }
    }

    /// Takes a payment back. One already gone counts as done.
    pub async fn delete_payment(&self, id: &str) -> Result<(), ProviderError> {
        let done = match self.kind {
            Kind::Xero => match self
                .send(
                    reqwest::Method::POST,
                    &format!("/Payments/{}", segment(id)),
                    &json!({ "Payments": [{ "PaymentID": id, "Status": "DELETED" }] }),
                    None,
                )
                .await
            {
                Err(e @ ProviderError::Refused { status: 400, .. }) => {
                    if self.xero_gone("Payments", "Payments", id).await? {
                        Ok(())
                    } else {
                        Err(e)
                    }
                }
                other => other.map(|_| ()),
            },
            Kind::QuickBooks => {
                let token = match self.sync_token("Payment", id).await {
                    Err(ProviderError::Refused { status: 404, .. }) => return Ok(()),
                    other => other?,
                };
                let mut url = self.url("/payment");
                url.query_pairs_mut().append_pair("operation", "delete");
                provider::send_json::<Value>(
                    self.request(reqwest::Method::POST, url)
                        .header("content-type", "application/json")
                        .body(json!({ "Id": id, "SyncToken": token }).to_string()),
                )
                .await
                .map(|_| ())
            }
        };
        match done {
            Err(ProviderError::Refused { status: 404, .. }) => Ok(()),
            other => other,
        }
    }

    /// Sets a credit note against the invoice it credits; returns what
    /// stands for that (Xero's allocation, Intuit's linking payment).
    #[allow(clippy::too_many_arguments)]
    pub async fn apply_credit(
        &self,
        credit: &str,
        invoice: &str,
        contact: &str,
        amount_minor: i64,
        exponent: u32,
        date: NaiveDate,
        key: &str,
    ) -> Result<String, ProviderError> {
        match self.kind {
            Kind::Xero => {
                let v = self
                    .send(
                        reqwest::Method::PUT,
                        &format!("/CreditNotes/{}/Allocations", segment(credit)),
                        &json!({ "Allocations": [{
                            "Amount": decimal(amount_minor, exponent),
                            "Invoice": { "InvoiceID": invoice },
                            "Date": date.to_string(),
                        }] }),
                        Some(key),
                    )
                    .await?;
                v.pointer("/Allocations/0/AllocationID")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| invalid("allocation id"))
            }
            Kind::QuickBooks => {
                // A payment of nothing that sets one against the other.
                let amount = decimal(amount_minor, exponent);
                let pay = json!({
                    "CustomerRef": { "value": contact },
                    "TotalAmt": 0,
                    "TxnDate": date.to_string(),
                    "Line": [
                        { "Amount": amount, "LinkedTxn": [{ "TxnId": invoice, "TxnType": "Invoice" }] },
                        { "Amount": amount, "LinkedTxn": [{ "TxnId": credit, "TxnType": "CreditMemo" }] },
                    ],
                });
                let v = self
                    .send(reqwest::Method::POST, "/payment", &pay, Some(key))
                    .await?;
                v.pointer("/Payment/Id")
                    .and_then(|v| text(&json!({ "Id": v }), "Id"))
                    .ok_or_else(|| invalid("payment id"))
            }
        }
    }

    /// What is left of a QuickBooks credit memo to set against invoices:
    /// QuickBooks may have set it against the customer's oldest invoices
    /// itself ("Automatically apply credits"). Xero never does.
    pub async fn remaining_credit(
        &self,
        credit: &str,
        exponent: u32,
    ) -> Result<Option<i64>, ProviderError> {
        match self.kind {
            Kind::Xero => Ok(None),
            Kind::QuickBooks => {
                let v = self
                    .get(&format!("/creditmemo/{}", segment(credit)))
                    .await?;
                Ok(v.get("CreditMemo")
                    .and_then(|m| m.get("RemainingCredit").or_else(|| m.get("Balance")))
                    .map(|r| minor(Some(r), exponent)))
            }
        }
    }

    /// Whether QuickBooks sets new credits against the customer's oldest
    /// invoices itself, rather than the ones they credit.
    pub async fn applies_credits_itself(&self) -> Result<bool, ProviderError> {
        match self.kind {
            Kind::Xero => Ok(false),
            Kind::QuickBooks => {
                let v = self.get("/preferences").await?;
                Ok(v.pointer("/Preferences/SalesFormsPrefs/AutoApplyCredit")
                    .and_then(Value::as_bool)
                    .unwrap_or(false))
            }
        }
    }

    /// Takes back what [`Self::apply_credit`] did, so the credit note can
    /// be voided.
    pub async fn unapply_credit(&self, credit: &str, applied: &str) -> Result<(), ProviderError> {
        match self.kind {
            Kind::Xero => {
                let url = self.url(&format!(
                    "/CreditNotes/{}/Allocations/{}",
                    segment(credit),
                    segment(applied)
                ));
                match provider::send_raw(self.request(reqwest::Method::DELETE, url)).await {
                    Ok(_) | Err(ProviderError::Refused { status: 404, .. }) => Ok(()),
                    Err(e) => Err(e),
                }
            }
            Kind::QuickBooks => self.delete_payment(applied).await,
        }
    }

    /// Where these invoices stand. Ids the books no longer have are left out.
    pub async fn invoice_states(&self, ids: &[String]) -> Result<Vec<InvoiceState>, ProviderError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = match self.kind {
            Kind::Xero => {
                let mut url = self.url("/Invoices");
                url.query_pairs_mut().append_pair("IDs", &ids.join(","));
                let v: Value = provider::send_json(self.request(reqwest::Method::GET, url)).await?;
                v.get("Invoices")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            }
            Kind::QuickBooks => {
                let list: Vec<String> = ids.iter().map(|i| quoted(i)).collect();
                self.query(
                    "Invoice",
                    &format!(
                        "select * from Invoice where Id in ({}) maxresults {}",
                        list.join(", "),
                        ids.len()
                    ),
                )
                .await?
            }
        };
        Ok(rows
            .iter()
            .filter_map(|r| {
                let currency = match self.kind {
                    Kind::Xero => text(r, "CurrencyCode"),
                    Kind::QuickBooks => r
                        .pointer("/CurrencyRef/value")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                }
                .unwrap_or_default();
                let exp = (self.exponent)(&currency);
                Some(match self.kind {
                    Kind::Xero => InvoiceState {
                        id: text(r, "InvoiceID")?,
                        total_minor: minor(r.get("Total"), exp),
                        due_minor: minor(r.get("AmountDue"), exp),
                        voided: matches!(
                            r.get("Status").and_then(Value::as_str),
                            Some("VOIDED" | "DELETED")
                        ),
                    },
                    Kind::QuickBooks => InvoiceState {
                        id: text(r, "Id")?,
                        total_minor: minor(r.get("TotalAmt"), exp),
                        due_minor: minor(r.get("Balance"), exp),
                        voided: false,
                    },
                })
            })
            .collect())
    }

    /// One page (from 1) of sales invoices dated before `before`, oldest
    /// first: authorised or paid ones (Xero), all but empty ones (Intuit
    /// keeps a voided invoice with nothing on it).
    pub async fn past_invoices(
        &self,
        before: NaiveDate,
        page: u32,
    ) -> Result<PastPage, ProviderError> {
        let page = page.max(1);
        match self.kind {
            Kind::Xero => {
                let mut url = self.url("/Invoices");
                url.query_pairs_mut()
                    .append_pair(
                        "where",
                        &format!(
                            "Type==\"ACCREC\"&&Date<DateTime({},{},{})",
                            before.format("%Y"),
                            before.format("%-m"),
                            before.format("%-d")
                        ),
                    )
                    .append_pair("Statuses", "AUTHORISED,PAID")
                    .append_pair("order", "Date,InvoiceNumber")
                    .append_pair("page", &page.to_string());
                let v: Value = provider::send_json(self.request(reqwest::Method::GET, url)).await?;
                let rows = v
                    .get("Invoices")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let more = rows.len() >= PAGE_SIZE;
                let invoices = rows
                    .iter()
                    .filter_map(|r| {
                        let currency = text(r, "CurrencyCode")?;
                        let exp = (self.exponent)(&currency);
                        Some(PastInvoice {
                            id: text(r, "InvoiceID")?,
                            number: text(r, "InvoiceNumber")?,
                            contact_id: r.get("Contact").and_then(|c| text(c, "ContactID")),
                            contact_name: r
                                .get("Contact")
                                .and_then(|c| text(c, "Name"))
                                .unwrap_or_else(|| "Unknown customer".into()),
                            date: date(r, "DateString")?,
                            due: date(r, "DueDateString"),
                            total_minor: minor(r.get("Total"), exp),
                            tax_minor: minor(r.get("TotalTax"), exp),
                            due_minor: minor(r.get("AmountDue"), exp),
                            lines: r
                                .get("LineItems")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .map(|l| PastLine {
                                    description: text(l, "Description").unwrap_or_default(),
                                    quantity: l
                                        .get("Quantity")
                                        .and_then(Value::as_f64)
                                        .unwrap_or(1.0),
                                    unit_minor: minor(l.get("UnitAmount"), exp),
                                    amount_minor: minor(l.get("LineAmount"), exp),
                                })
                                .collect(),
                            currency,
                        })
                    })
                    .collect();
                Ok(PastPage { invoices, more })
            }
            Kind::QuickBooks => {
                let start = (page - 1) as usize * PAGE_SIZE + 1;
                let rows = self
                    .query(
                        "Invoice",
                        &format!(
                            "select * from Invoice where TxnDate < '{before}' orderby TxnDate, Id startposition {start} maxresults {PAGE_SIZE}"
                        ),
                    )
                    .await?;
                let more = rows.len() >= PAGE_SIZE;
                let invoices = rows
                    .iter()
                    .filter_map(|r| {
                        let currency = r
                            .pointer("/CurrencyRef/value")
                            .and_then(Value::as_str)
                            .unwrap_or("USD")
                            .to_owned();
                        let exp = (self.exponent)(&currency);
                        let total = minor(r.get("TotalAmt"), exp);
                        if total <= 0 {
                            return None;
                        }
                        let id = text(r, "Id")?;
                        Some(PastInvoice {
                            number: text(r, "DocNumber").unwrap_or_else(|| format!("QB-{id}")),
                            id,
                            contact_id: r
                                .pointer("/CustomerRef/value")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            contact_name: r
                                .pointer("/CustomerRef/name")
                                .and_then(Value::as_str)
                                .unwrap_or("Unknown customer")
                                .to_owned(),
                            date: date(r, "TxnDate")?,
                            due: date(r, "DueDate"),
                            total_minor: total,
                            tax_minor: minor(r.pointer("/TxnTaxDetail/TotalTax"), exp),
                            due_minor: minor(r.get("Balance"), exp),
                            lines: r
                                .get("Line")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .filter(|l| {
                                    l.get("DetailType").and_then(Value::as_str)
                                        == Some("SalesItemLineDetail")
                                })
                                .map(|l| PastLine {
                                    description: text(l, "Description").unwrap_or_default(),
                                    quantity: l
                                        .pointer("/SalesItemLineDetail/Qty")
                                        .and_then(Value::as_f64)
                                        .unwrap_or(1.0),
                                    unit_minor: minor(
                                        l.pointer("/SalesItemLineDetail/UnitPrice"),
                                        exp,
                                    ),
                                    amount_minor: minor(l.get("Amount"), exp),
                                })
                                .collect(),
                            currency,
                        })
                    })
                    .collect();
                Ok(PastPage { invoices, more })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_follow_the_currency() {
        assert_eq!(decimal(12_345, 2), json!(123.45));
        assert_eq!(decimal(1500, 0), json!(1500.0));
        assert_eq!(minor(Some(&json!(123.45)), 2), 12_345);
        assert_eq!(minor(Some(&json!("0.1")), 2), 10);
        assert_eq!(minor(Some(&json!(1500)), 0), 1500);
        assert_eq!(minor(None, 2), 0);
    }

    #[test]
    fn intuit_queries_are_quoted() {
        assert_eq!(quoted("O'Brien \\ Co"), "'O\\'Brien \\\\ Co'");
    }

    #[test]
    fn xero_dates_are_read_from_their_strings() {
        let v =
            json!({ "DateString": "2026-03-01T00:00:00", "Date": "/Date(1772323200000+0000)/" });
        assert_eq!(date(&v, "DateString"), NaiveDate::from_ymd_opt(2026, 3, 1));
        assert_eq!(date(&v, "Missing"), None);
    }
}
