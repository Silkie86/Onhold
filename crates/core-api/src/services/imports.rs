//! Importing talent, organisations or contacts from a spreadsheet.
//!
//! 1. Upload the CSV: it is parsed, kept, and answered with its columns, a
//!    sample and a suggested mapping of columns to fields.
//! 2. Dry run with a mapping: every row is read and checked exactly as the
//!    real run would, nothing is written, and the result says how many rows
//!    would be created, updated, skipped or refused, and why.
//! 3. Run: the worker imports in batches, recording progress, so a large
//!    file neither times out nor half-finishes; a crash resumes where it
//!    stopped. The file is dropped when the run ends.
//!
//! Rows matching an existing record (contacts by email or phone,
//! organisations and talent by name) are skipped, updated or created
//! anyway, as chosen. Imported records do not trigger automation rules
//! unless asked, so an import never mass-mails anyone by surprise.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use core_db::Tx;
use core_db::jobs::NewJob;
use core_domain::crm::{email_key, name_key, phone_key};
use core_domain::{Actor, Ctx, Money, Permission, TalentKind, UserId, WorkspaceId};
use core_engines::fields::FieldDef;
use core_engines::import::{self as engine, DateOrder, Reading, Target};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::contacts::{self, Channel, ContactPatch, NewContact};
use super::organisations::{self, Address, NewOrganisation, OrganisationPatch};
use super::talent::{self, NewTalent, TalentPatch, TalentStatus};
use super::{begin, crm};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

pub const JOB_IMPORT: &str = "crm.import";
/// The largest file accepted.
pub const MAX_CSV_BYTES: usize = 10 * 1024 * 1024;
/// Rows written per transaction.
const BATCH: usize = 200;
/// Problems kept on the import record, and shown by a dry run.
const MAX_STORED_ISSUES: i32 = 1000;
const MAX_DRY_RUN_ISSUES: usize = 200;
/// Sample rows returned on upload and dry run.
const SAMPLE_ROWS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnMatch {
    /// Leave the existing record alone.
    #[default]
    Skip,
    /// Fill the existing record from the row: values in the row replace
    /// its values, lists (emails, phones, tags) are added to.
    Update,
    /// Create a new record anyway.
    Create,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportOptions {
    /// What to do with a row that matches an existing record.
    #[serde(default)]
    pub on_match: OnMatch,
    /// How to read dates like 07/03/2027; default from the workspace's locale.
    pub date_order: Option<DateOrder>,
    /// For amounts with no currency; default the workspace's.
    pub default_currency: Option<String>,
    /// Contacts: create organisations the sheet names that do not exist yet
    /// (default true). Otherwise such rows are refused.
    #[serde(default = "yes")]
    pub create_organisations: bool,
    /// Tags added to every imported record.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Let automation rules react to the imported records (default false).
    #[serde(default)]
    pub run_rules: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            on_match: OnMatch::Skip,
            date_order: None,
            default_currency: None,
            create_organisations: true,
            tags: Vec::new(),
            run_rules: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ImportPlan {
    /// The field each column feeds, in column order (`null` to ignore a
    /// column): a key from the upload's `targets`. Several columns may feed
    /// `name` (joined with spaces) or a list field.
    pub columns: Vec<Option<String>>,
    #[serde(default)]
    pub options: ImportOptions,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportTarget {
    pub key: String,
    pub label: String,
    /// Several columns may feed it.
    pub many: bool,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportIssue {
    /// The spreadsheet row (the header is row 1).
    pub row: usize,
    /// The field at fault, when there is one.
    pub field: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportView {
    pub id: Uuid,
    pub entity: String,
    pub filename: Option<String>,
    /// `uploaded`, `queued`, `running`, `done` or `failed`.
    pub status: String,
    pub headers: Vec<String>,
    pub row_count: i32,
    #[schema(value_type = Option<ImportPlan>)]
    pub plan: Option<Value>,
    pub created: i32,
    pub updated: i32,
    pub skipped: i32,
    pub failed: i32,
    /// Rows dealt with so far.
    pub processed: i32,
    /// The first problems found (up to 1,000).
    pub issues: Vec<ImportIssue>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub created_by: Option<UserId>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl From<core_db::imports::Import> for ImportView {
    fn from(i: core_db::imports::Import) -> Self {
        let plan = i
            .mapping
            .map(|columns| json!({ "columns": columns, "options": i.options }));
        Self {
            id: i.id,
            entity: i.entity,
            filename: i.filename,
            status: i.status,
            headers: i.headers,
            row_count: i.row_count,
            plan,
            created: i.created_count,
            updated: i.updated_count,
            skipped: i.skipped_count,
            failed: i.failed_count,
            processed: i.processed,
            issues: serde_json::from_value(i.issues).unwrap_or_default(),
            error: i.error,
            created_at: i.created_at,
            created_by: i.created_by,
            started_at: i.started_at,
            finished_at: i.finished_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportPreview {
    pub import: ImportView,
    /// The first rows, as text.
    pub sample: Vec<Vec<String>>,
    /// The field each column most likely feeds (`null` when unsure).
    pub suggested: Vec<Option<String>>,
    /// The fields columns can feed.
    pub targets: Vec<ImportTarget>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DryRunResult {
    pub create: usize,
    pub update: usize,
    pub skip: usize,
    pub fail: usize,
    /// Organisations a contact import would create.
    pub new_organisations: Vec<String>,
    /// The first problems (up to 200).
    pub issues: Vec<ImportIssue>,
    /// The first records as they would be saved.
    #[schema(value_type = Vec<Object>)]
    pub sample: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct NewImport {
    /// `talent`, `organisation` or `contact`.
    pub entity: String,
    pub filename: Option<String>,
    /// The file's text: CSV with a header row, separated by commas,
    /// semicolons or tabs. Save spreadsheets as CSV (UTF-8).
    pub csv: String,
}

fn write_permission(entity: &str) -> ApiResult<Permission> {
    super::records::write_permission(entity).ok_or_else(|| {
        ApiError::invalid(
            "entity",
            format!("import one of: {}", crm::ENTITIES.join(", ")),
        )
    })
}

// Targets ---------------------------------------------------------------------

struct CoreTarget {
    key: &'static str,
    label: &'static str,
    many: bool,
    required: bool,
    synonyms: &'static [&'static str],
}

const fn t(
    key: &'static str,
    label: &'static str,
    synonyms: &'static [&'static str],
) -> CoreTarget {
    CoreTarget {
        key,
        label,
        many: false,
        required: false,
        synonyms,
    }
}

const fn many(
    key: &'static str,
    label: &'static str,
    synonyms: &'static [&'static str],
) -> CoreTarget {
    CoreTarget {
        key,
        label,
        many: true,
        required: false,
        synonyms,
    }
}

const fn name(synonyms: &'static [&'static str]) -> CoreTarget {
    CoreTarget {
        key: "name",
        label: "Name",
        many: true,
        required: true,
        synonyms,
    }
}

const TAGS: CoreTarget = many("tags", "Tags", &["labels", "tag", "label", "categories"]);
const NOTES: CoreTarget = many(
    "notes",
    "Notes",
    &["note", "comments", "comment", "description"],
);

fn core_targets(entity: &str) -> Vec<CoreTarget> {
    match entity {
        talent::ENTITY => vec![
            name(&[
                "stage name",
                "artist",
                "artist name",
                "act",
                "act name",
                "talent",
            ]),
            t("kind", "Kind", &[]),
            t(
                "talent_type",
                "Type",
                &["talent type", "category", "genre type"],
            ),
            t(
                "legal_name",
                "Legal name",
                &["real name", "full legal name"],
            ),
            many("bio", "Bio", &["biography", "about", "description"]),
            t(
                "base_location",
                "Based in",
                &["location", "base", "city", "hometown", "based"],
            ),
            t("default_fee", "Default fee", &["fee", "rate", "price"]),
            t(
                "commission_percent",
                "Commission %",
                &["commission", "commission percent"],
            ),
            t("status", "Status", &["active"]),
            TAGS,
        ],
        organisations::ENTITY => vec![
            name(&[
                "organisation",
                "organization",
                "company",
                "company name",
                "venue",
                "venue name",
                "organisation name",
                "organization name",
                "business",
            ]),
            t(
                "org_type",
                "Type",
                &["organisation type", "organization type", "category"],
            ),
            t(
                "tax_id",
                "Tax / VAT number",
                &[
                    "vat",
                    "vat number",
                    "vat no",
                    "tax number",
                    "tax id",
                    "ein",
                    "abn",
                ],
            ),
            t("default_currency", "Currency", &["default currency"]),
            t(
                "payment_terms_days",
                "Payment terms (days)",
                &["payment terms", "terms"],
            ),
            t(
                "address.line1",
                "Address line 1",
                &["address", "address 1", "street", "street address"],
            ),
            t("address.line2", "Address line 2", &["address 2"]),
            t("address.city", "City", &["town"]),
            t("address.region", "Region", &["state", "county", "province"]),
            t(
                "address.postcode",
                "Postcode",
                &["post code", "postal code", "zip", "zip code"],
            ),
            t("address.country", "Country", &["country code"]),
            NOTES,
            TAGS,
        ],
        _ => vec![
            name(&[
                "full name",
                "contact name",
                "contact",
                "first name",
                "last name",
                "surname",
                "given name",
                "family name",
                "forename",
            ]),
            many(
                "emails",
                "Email",
                &[
                    "email address",
                    "e-mail",
                    "mail",
                    "work email",
                    "personal email",
                    "emails",
                    "email 2",
                    "other email",
                ],
            ),
            many(
                "phones",
                "Phone",
                &[
                    "phone number",
                    "telephone",
                    "tel",
                    "mobile",
                    "cell",
                    "mobile number",
                    "landline",
                    "phones",
                    "work phone",
                    "home phone",
                ],
            ),
            t("role", "Role", &["job title", "title", "position", "job"]),
            t(
                "organisation",
                "Organisation",
                &[
                    "organization",
                    "company",
                    "company name",
                    "venue",
                    "employer",
                    "account",
                    "organisation name",
                    "organization name",
                ],
            ),
            t(
                "preferred_channel",
                "Preferred channel",
                &["channel", "contact preference"],
            ),
            t(
                "email_opt_in",
                "Email marketing",
                &["email opt in", "newsletter", "marketing"],
            ),
            t(
                "sms_opt_in",
                "Agreed to texts",
                &["sms opt in", "sms marketing"],
            ),
            t(
                "whatsapp_opt_in",
                "Agreed to WhatsApp",
                &["whatsapp opt in", "whatsapp marketing"],
            ),
            NOTES,
            TAGS,
        ],
    }
}

fn targets(entity: &str, defs: &[FieldDef]) -> Vec<(ImportTarget, Target)> {
    let mut out: Vec<(ImportTarget, Target)> = core_targets(entity)
        .into_iter()
        .map(|c| {
            (
                ImportTarget {
                    key: c.key.into(),
                    label: c.label.into(),
                    many: c.many,
                    required: c.required,
                },
                Target {
                    key: c.key.into(),
                    label: c.label.into(),
                    synonyms: c.synonyms.iter().map(|s| (*s).to_owned()).collect(),
                },
            )
        })
        .collect();
    for d in defs.iter().filter(|d| !d.archived) {
        let key = format!("custom.{}", d.key);
        out.push((
            ImportTarget {
                key: key.clone(),
                label: d.label.clone(),
                many: false,
                required: d.required,
            },
            Target {
                key,
                label: d.label.clone(),
                synonyms: vec![d.key.clone()],
            },
        ));
    }
    out
}

// Upload and inspect ------------------------------------------------------------

pub async fn upload(state: &AppState, ctx: &Ctx, new: NewImport) -> ApiResult<ImportPreview> {
    ctx.authorize(write_permission(&new.entity)?)?;
    if new.csv.len() > MAX_CSV_BYTES {
        return Err(ApiError::invalid(
            "csv",
            format!("the file is larger than {} MB", MAX_CSV_BYTES / 1024 / 1024),
        ));
    }
    let filename = super::optional_text("filename", new.filename, 255)?;
    let table = engine::parse_csv(&new.csv).map_err(|m| ApiError::invalid("csv", m))?;
    if table.rows.is_empty() {
        return Err(ApiError::invalid(
            "csv",
            "the file has no rows under its header",
        ));
    }
    let mut tx = begin(state, ctx).await?;
    let defs = super::fields::engine_defs(&mut tx, &new.entity).await?;
    let targets = targets(&new.entity, &defs);
    let engine_targets: Vec<Target> = targets.iter().map(|(_, t)| t.clone()).collect();
    let suggested = engine::suggest_mapping(&table.headers, &engine_targets);
    let id = core_domain::ids::new_id();
    core_db::imports::insert(
        &mut tx,
        id,
        core_db::imports::New {
            entity: &new.entity,
            filename: filename.as_deref(),
            csv: &new.csv,
            headers: &table.headers,
            row_count: i32::try_from(table.rows.len()).unwrap_or(i32::MAX),
        },
        ctx.user_id(),
    )
    .await?;
    let import = core_db::imports::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(ImportPreview {
        import: import.into(),
        sample: table.rows.into_iter().take(SAMPLE_ROWS).collect(),
        suggested,
        targets: targets.into_iter().map(|(t, _)| t).collect(),
    })
}

pub async fn get(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<ImportView> {
    let mut tx = begin(state, ctx).await?;
    let import = core_db::imports::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    ctx.authorize(write_permission(&import.entity)?)?;
    Ok(import.into())
}

pub async fn list(
    state: &AppState,
    ctx: &Ctx,
    before: Option<Uuid>,
    limit: i64,
) -> ApiResult<Vec<ImportView>> {
    let mut tx = begin(state, ctx).await?;
    let rows = core_db::imports::list(&mut tx, before, limit).await?;
    tx.commit().await?;
    Ok(rows
        .into_iter()
        .filter(|i| {
            super::records::write_permission(&i.entity)
                .is_some_and(|p| ctx.permissions.contains(&p))
        })
        .map(Into::into)
        .collect())
}

// Reading rows --------------------------------------------------------------------

/// A row read into the record it describes.
#[derive(Debug, Clone)]
enum Record {
    Talent(NewTalent),
    Organisation(NewOrganisation),
    /// With the organisation's name, if the row gives one.
    Contact(NewContact, Option<String>),
}

impl Record {
    fn name(&self) -> &str {
        match self {
            Record::Talent(n) => &n.name,
            Record::Organisation(n) => &n.name,
            Record::Contact(n, _) => &n.name,
        }
    }
}

/// A row, read: the record, the fields the row gave values for, and any
/// problems reading it.
struct ReadRow {
    record: Record,
    present: HashSet<String>,
    issues: Vec<(Option<String>, String)>,
}

/// Everything fixed for one import: the mapping, the field definitions and
/// how to read cells.
struct Reader<'a> {
    entity: &'a str,
    columns: &'a [Option<String>],
    defs: &'a [FieldDef],
    reading: Reading,
    extra_tags: Vec<String>,
}

fn joined(values: &[&str]) -> String {
    values.join(" ")
}

impl Reader<'_> {
    fn read(&self, row: &[String]) -> ReadRow {
        let mut cells: HashMap<&str, Vec<&str>> = HashMap::new();
        for (target, cell) in self.columns.iter().zip(row) {
            if let Some(t) = target
                && !cell.trim().is_empty()
            {
                cells.entry(t.as_str()).or_default().push(cell.trim());
            }
        }
        let present: HashSet<String> = cells.keys().map(|k| (*k).to_owned()).collect();
        let mut issues: Vec<(Option<String>, String)> = Vec::new();
        let mut issue =
            |field: &str, message: String| issues.push((Some(field.to_owned()), message));
        let text = |k: &str| cells.get(k).map(|v| joined(v));
        let lines = |k: &str| cells.get(k).map(|v| v.join("\n"));
        let list = |k: &str| -> Vec<String> {
            cells
                .get(k)
                .map(|v| v.iter().flat_map(|c| engine::split_list(c)).collect())
                .unwrap_or_default()
        };

        let mut custom = Map::new();
        for d in self.defs.iter().filter(|d| !d.archived) {
            let key = format!("custom.{}", d.key);
            if let Some(raw) = text(&key) {
                match engine::coerce(d, &raw, &self.reading) {
                    Ok(Some(v)) => {
                        custom.insert(d.key.clone(), v);
                    }
                    Ok(None) => {}
                    Err(m) => issue(&key, m),
                }
            }
        }
        let mut tags = list("tags");
        tags.extend(self.extra_tags.iter().cloned());
        let flag = |k: &str, issue: &mut dyn FnMut(&str, String)| -> bool {
            match text(k) {
                None => false,
                Some(raw) => engine::parse_bool(&raw).unwrap_or_else(|| {
                    issue(k, format!("`{raw}` is not yes or no"));
                    false
                }),
            }
        };

        let record = match self.entity {
            talent::ENTITY => {
                let kind = text("kind").and_then(|raw| {
                    TalentKind::parse(&raw).or_else(|| {
                        issue(
                            "kind",
                            format!("`{raw}` is not person, act, space or equipment"),
                        );
                        None
                    })
                });
                let status =
                    text("status").and_then(|raw| match raw.trim().to_lowercase().as_str() {
                        "active" | "yes" | "y" | "true" | "1" => Some(TalentStatus::Active),
                        "inactive" | "no" | "n" | "false" | "0" => Some(TalentStatus::Inactive),
                        _ => {
                            issue("status", format!("`{raw}` is not active or inactive"));
                            None
                        }
                    });
                let default_fee = text("default_fee").and_then(|raw| {
                    engine::parse_money(&raw, &self.reading.default_currency)
                        .map_err(|m| issue("default_fee", m))
                        .ok()
                });
                let commission_percent = text("commission_percent").and_then(|raw| {
                    engine::parse_number(raw.trim_end_matches('%')).or_else(|| {
                        issue("commission_percent", format!("`{raw}` is not a percentage"));
                        None
                    })
                });
                Record::Talent(NewTalent {
                    kind,
                    name: text("name").unwrap_or_default(),
                    legal_name: text("legal_name"),
                    talent_type: text("talent_type"),
                    bio: lines("bio"),
                    base_location: text("base_location"),
                    default_fee: default_fee.map(|m| Money {
                        minor: m.minor,
                        currency: m.currency,
                    }),
                    commission_percent,
                    status,
                    portal_user_id: None,
                    tags,
                    custom,
                })
            }
            organisations::ENTITY => {
                let address = Address {
                    label: None,
                    line1: text("address.line1"),
                    line2: text("address.line2"),
                    city: text("address.city"),
                    region: text("address.region"),
                    postcode: text("address.postcode"),
                    country: text("address.country"),
                };
                let payment_terms_days = text("payment_terms_days").and_then(|raw| {
                    let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
                    digits.parse().ok().or_else(|| {
                        issue(
                            "payment_terms_days",
                            format!("`{raw}` is not a number of days"),
                        );
                        None
                    })
                });
                Record::Organisation(NewOrganisation {
                    name: text("name").unwrap_or_default(),
                    org_type: text("org_type"),
                    addresses: if address.is_empty() {
                        Vec::new()
                    } else {
                        vec![address]
                    },
                    tax_id: text("tax_id"),
                    default_currency: text("default_currency"),
                    payment_terms_days,
                    notes: lines("notes"),
                    tags,
                    custom,
                })
            }
            _ => {
                let preferred_channel = text("preferred_channel").and_then(|raw| {
                    Channel::parse(&raw).or_else(|| {
                        issue(
                            "preferred_channel",
                            format!("`{raw}` is not email, phone, sms or whatsapp"),
                        );
                        None
                    })
                });
                let email_opt_in = flag("email_opt_in", &mut issue);
                let sms_opt_in = flag("sms_opt_in", &mut issue);
                let whatsapp_opt_in = flag("whatsapp_opt_in", &mut issue);
                Record::Contact(
                    NewContact {
                        name: text("name").unwrap_or_default(),
                        organisation_id: None,
                        emails: list("emails"),
                        phones: list("phones"),
                        role: text("role"),
                        preferred_channel,
                        email_opt_in,
                        sms_opt_in,
                        whatsapp_opt_in,
                        notes: lines("notes"),
                        tags,
                        custom,
                    },
                    text("organisation"),
                )
            }
        };
        ReadRow {
            record,
            present,
            issues,
        }
    }
}

// Matching ---------------------------------------------------------------------------

/// What rows are matched against: existing records, plus those created
/// earlier in the same import.
#[derive(Default)]
struct Index {
    by_name: HashMap<String, Uuid>,
    by_email: HashMap<String, Uuid>,
    by_phone: HashMap<String, Uuid>,
    /// Contacts only: organisations by name.
    organisations: HashMap<String, Uuid>,
}

impl Index {
    async fn load(tx: &mut Tx, entity: &str) -> ApiResult<Self> {
        let mut index = Index::default();
        match entity {
            talent::ENTITY => {
                for (id, name) in core_db::talent::names(tx).await? {
                    index.by_name.entry(name_key(&name)).or_insert(id);
                }
            }
            organisations::ENTITY => {
                for (id, name) in core_db::organisations::names(tx).await? {
                    index.by_name.entry(name_key(&name)).or_insert(id);
                }
            }
            _ => {
                for k in core_db::contacts::keys(tx).await? {
                    for e in k.emails {
                        index.by_email.entry(e).or_insert(k.id);
                    }
                    for p in k.phone_keys {
                        index.by_phone.entry(p).or_insert(k.id);
                    }
                }
                for (id, name) in core_db::organisations::names(tx).await? {
                    index.organisations.entry(name_key(&name)).or_insert(id);
                }
            }
        }
        Ok(index)
    }

    fn find(&self, record: &Record) -> Option<Uuid> {
        match record {
            Record::Contact(c, _) => c
                .emails
                .iter()
                .find_map(|e| self.by_email.get(&email_key(e)))
                .or_else(|| {
                    c.phones
                        .iter()
                        .filter_map(|p| phone_key(p))
                        .find_map(|k| self.by_phone.get(&k))
                })
                .copied(),
            other => self.by_name.get(&name_key(other.name())).copied(),
        }
    }

    fn add(&mut self, record: &Record, id: Uuid) {
        match record {
            Record::Contact(c, _) => {
                for e in &c.emails {
                    self.by_email.entry(email_key(e)).or_insert(id);
                }
                for k in c.phones.iter().filter_map(|p| phone_key(p)) {
                    self.by_phone.entry(k).or_insert(id);
                }
            }
            other => {
                self.by_name.entry(name_key(other.name())).or_insert(id);
            }
        }
    }
}

/// What a row will do.
enum Plan {
    Create(Record),
    Update(Uuid, Record, HashSet<String>),
    Skip,
    Fail(Vec<(Option<String>, String)>),
}

/// The import's mapping, options and field definitions, checked.
struct Setup {
    entity: String,
    columns: Vec<Option<String>>,
    options: ImportOptions,
    defs: Vec<FieldDef>,
    reading: Reading,
    extra_tags: Vec<String>,
}

async fn setup(
    tx: &mut Tx,
    import: &core_db::imports::Import,
    plan: ImportPlan,
) -> ApiResult<Setup> {
    let defs = super::fields::engine_defs(tx, &import.entity).await?;
    let targets = targets(&import.entity, &defs);
    let mut check = crm::Check::default();
    if plan.columns.len() != import.headers.len() {
        check.issue(
            "columns",
            format!(
                "give one entry per column ({}), null to ignore it",
                import.headers.len()
            ),
        );
    }
    let mut seen: HashSet<&str> = HashSet::new();
    for (i, c) in plan.columns.iter().enumerate() {
        let Some(key) = c else { continue };
        match targets.iter().find(|(t, _)| &t.key == key) {
            None => check.issue(
                format!("columns[{i}]"),
                format!("there is no field `{key}`"),
            ),
            Some((t, _)) if !t.many && !seen.insert(key.as_str()) => check.issue(
                format!("columns[{i}]"),
                format!("only one column can feed {}", t.label),
            ),
            Some(_) => {}
        }
    }
    if !plan.columns.iter().flatten().any(|c| c == "name") {
        check.issue("columns", "choose the column (or columns) with the name");
    }
    let extra_tags = check.tags(&plan.options.tags);
    let currency = check.currency(
        "options.default_currency",
        plan.options.default_currency.as_deref(),
    );
    check.finish()?;

    let w = sqlx::query!(
        "SELECT default_currency, default_timezone, locale FROM workspaces WHERE id = app_workspace_id()"
    )
    .fetch_one(&mut **tx)
    .await?;
    let reading = Reading {
        date_order: plan
            .options
            .date_order
            .unwrap_or_else(|| DateOrder::for_locale(&w.locale)),
        default_currency: currency.unwrap_or_else(|| w.default_currency.trim().to_owned()),
        timezone: w.default_timezone.parse().unwrap_or(chrono_tz::Tz::UTC),
    };
    Ok(Setup {
        entity: import.entity.clone(),
        columns: plan.columns,
        options: plan.options,
        defs,
        reading,
        extra_tags,
    })
}

impl Setup {
    fn reader(&self) -> Reader<'_> {
        Reader {
            entity: &self.entity,
            columns: &self.columns,
            defs: &self.defs,
            reading: self.reading.clone(),
            extra_tags: self.extra_tags.clone(),
        }
    }

    /// Reads and checks one row, and decides what to do with it.
    fn plan(&self, reader: &Reader<'_>, index: &Index, row: &[String]) -> Plan {
        let read = reader.read(row);
        let mut issues = read.issues;
        let check = match &read.record {
            Record::Talent(n) => talent::check_new(n, &self.defs).1,
            Record::Organisation(n) => organisations::check_new(n, &self.defs).1,
            Record::Contact(n, org) => {
                let mut c = contacts::check_new(n, &self.defs).1;
                if let Some(org) = org
                    && !self.options.create_organisations
                    && !index.organisations.contains_key(&name_key(org))
                {
                    c.issue("organisation", format!("there is no organisation `{org}`"));
                }
                c
            }
        };
        issues.extend(check.issues.into_iter().map(|i| (Some(i.field), i.message)));
        if !issues.is_empty() {
            return Plan::Fail(issues);
        }
        match (index.find(&read.record), self.options.on_match) {
            (Some(_), OnMatch::Skip) => Plan::Skip,
            (Some(id), OnMatch::Update) => Plan::Update(id, read.record, read.present),
            _ => Plan::Create(read.record),
        }
    }
}

fn issues_for(row: usize, list: Vec<(Option<String>, String)>) -> Vec<ImportIssue> {
    list.into_iter()
        .map(|(field, message)| ImportIssue {
            row,
            field,
            message,
        })
        .collect()
}

fn record_json(record: &Record) -> Value {
    match record {
        Record::Talent(n) => json!({
            "kind": n.kind.unwrap_or_default(),
            "name": n.name,
            "legal_name": n.legal_name,
            "talent_type": n.talent_type,
            "base_location": n.base_location,
            "default_fee": n.default_fee,
            "commission_percent": n.commission_percent,
            "status": n.status.unwrap_or_default(),
            "tags": n.tags,
            "custom": n.custom,
        }),
        Record::Organisation(n) => json!({
            "name": n.name,
            "org_type": n.org_type,
            "addresses": n.addresses,
            "tax_id": n.tax_id,
            "default_currency": n.default_currency,
            "payment_terms_days": n.payment_terms_days,
            "tags": n.tags,
            "custom": n.custom,
        }),
        Record::Contact(n, org) => json!({
            "name": n.name,
            "organisation": org,
            "emails": n.emails,
            "phones": n.phones,
            "role": n.role,
            "preferred_channel": n.preferred_channel,
            "tags": n.tags,
            "custom": n.custom,
        }),
    }
}

/// What running an import takes: writing its records, and for contacts
/// that may create the organisations they name, writing organisations.
fn permissions_needed(
    entity: &str,
    options: &ImportOptions,
) -> ApiResult<std::collections::BTreeSet<Permission>> {
    let mut needed = std::collections::BTreeSet::from([write_permission(entity)?]);
    if entity == contacts::ENTITY && options.create_organisations {
        needed.insert(Permission::OrganisationWrite);
    }
    Ok(needed)
}

async fn load_for_plan(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    plan: &ImportPlan,
) -> ApiResult<(Tx, core_db::imports::Import, String)> {
    let mut tx = begin(state, ctx).await?;
    let import = core_db::imports::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    for p in permissions_needed(&import.entity, &plan.options)? {
        ctx.authorize(p)?;
    }
    if import.status != "uploaded" {
        return Err(ApiError::conflict(
            "import_started",
            "this import has already been run; upload the file again to import it again",
        ));
    }
    let csv = core_db::imports::csv(&mut tx, id)
        .await?
        .ok_or_else(|| ApiError::conflict("import_started", "the file is no longer kept"))?;
    Ok((tx, import, csv))
}

/// Reads and checks every row as the run would, writing nothing.
pub async fn dry_run(
    state: &AppState,
    ctx: &Ctx,
    id: Uuid,
    plan: ImportPlan,
) -> ApiResult<DryRunResult> {
    let (mut tx, import, csv) = load_for_plan(state, ctx, id, &plan).await?;
    let setup = setup(&mut tx, &import, plan).await?;
    let mut index = Index::load(&mut tx, &import.entity).await?;
    tx.commit().await?;

    let table = engine::parse_csv(&csv).map_err(|m| ApiError::invalid("csv", m))?;
    let reader = setup.reader();
    let mut out = DryRunResult {
        create: 0,
        update: 0,
        skip: 0,
        fail: 0,
        new_organisations: Vec::new(),
        issues: Vec::new(),
        sample: Vec::new(),
    };
    for (i, row) in table.rows.iter().enumerate() {
        match setup.plan(&reader, &index, row) {
            Plan::Create(record) | Plan::Update(_, record, _) if out.sample.len() < SAMPLE_ROWS => {
                out.sample.push(record_json(&record));
                count(&mut out, &mut index, &setup, &record, i);
            }
            Plan::Create(record) | Plan::Update(_, record, _) => {
                count(&mut out, &mut index, &setup, &record, i);
            }
            Plan::Skip => out.skip += 1,
            Plan::Fail(list) => {
                out.fail += 1;
                if out.issues.len() < MAX_DRY_RUN_ISSUES {
                    out.issues.extend(issues_for(table.row_numbers[i], list));
                    out.issues.truncate(MAX_DRY_RUN_ISSUES);
                }
            }
        }
    }
    Ok(out)
}

/// Counts a planned create or update, as the run would see it: a later row
/// matching this one finds it.
fn count(out: &mut DryRunResult, index: &mut Index, setup: &Setup, record: &Record, i: usize) {
    match index.find(record) {
        Some(_) if setup.options.on_match == OnMatch::Update => out.update += 1,
        _ => {
            out.create += 1;
            index.add(record, Uuid::from_u128(i as u128 + 1));
        }
    }
    if let Record::Contact(_, Some(org)) = record {
        let key = name_key(org);
        if let std::collections::hash_map::Entry::Vacant(slot) = index.organisations.entry(key) {
            slot.insert(Uuid::nil());
            if out.new_organisations.len() < 50 {
                out.new_organisations.push(org.clone());
            }
        }
    }
}

/// Queues the import for the worker.
pub async fn run(state: &AppState, ctx: &Ctx, id: Uuid, plan: ImportPlan) -> ApiResult<ImportView> {
    let (mut tx, import, _) = load_for_plan(state, ctx, id, &plan).await?;
    // Checked now, so a bad mapping fails here rather than in the worker.
    let setup = setup(&mut tx, &import, plan).await?;
    let columns = serde_json::to_value(&setup.columns).map_err(ApiError::internal)?;
    let options = serde_json::to_value(&setup.options).map_err(ApiError::internal)?;
    if !core_db::imports::queue(&mut tx, id, &columns, &options).await? {
        return Err(ApiError::conflict(
            "import_started",
            "this import has already been run",
        ));
    }
    let payload = json!({ "import_id": id });
    let key = id.to_string();
    core_db::jobs::enqueue(
        &mut tx,
        NewJob::new(Some(ctx.workspace_id), JOB_IMPORT, &payload).dedupe(&key),
    )
    .await?;
    let import = core_db::imports::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(import.into())
}

// The worker's part --------------------------------------------------------------------

/// Who an import acts as: the member who started it, with what they may
/// still do of what importing needs (an API key's permissions were checked
/// when it queued the run). `None` once that member may no longer write
/// the records at all, or has left.
async fn import_ctx(
    tx: &mut Tx,
    workspace: WorkspaceId,
    by: Option<UserId>,
    entity: &str,
    options: &ImportOptions,
) -> ApiResult<Option<Ctx>> {
    let mut permissions = permissions_needed(entity, options)?;
    if let Some(user) = by {
        let Some(role) = core_db::members::role_of(tx, user).await? else {
            return Ok(None);
        };
        let current = core_db::workspaces::role_permissions(tx, role).await?;
        permissions.retain(|p| current.contains(p));
        if !permissions.contains(&write_permission(entity)?) {
            return Ok(None);
        }
    }
    Ok(Some(Ctx {
        workspace_id: workspace,
        actor: match by {
            Some(user_id) => Actor::User {
                user_id,
                impersonator: None,
            },
            None => Actor::System,
        },
        role: None,
        permissions,
        meta: core_domain::authz::RequestMeta::default(),
    }))
}

/// Lists in the row are added to the record's own.
fn merged(current: &[String], extra: &[String]) -> Vec<String> {
    let mut out = current.to_vec();
    for e in extra {
        if !out.contains(e) {
            out.push(e.clone());
        }
    }
    out
}

/// Some(value) for the fields the row gave, so updates touch nothing else.
fn given<T: Clone>(present: &HashSet<String>, key: &str, value: &Option<T>) -> Option<Option<T>> {
    present.contains(key).then(|| value.clone())
}

/// Carries out one row's plan. Errors other than database trouble are the
/// row's problems, reported and skipped.
async fn apply(
    tx: &mut Tx,
    ctx: &Ctx,
    index: &mut Index,
    plan: Plan,
    extra: &Value,
) -> ApiResult<Result<&'static str, Vec<(Option<String>, String)>>> {
    let outcome = match plan {
        Plan::Skip => return Ok(Ok("skipped")),
        Plan::Fail(issues) => return Ok(Err(issues)),
        Plan::Create(record) => create(tx, ctx, index, record, extra)
            .await
            .map(|()| "created"),
        Plan::Update(id, record, present) => update(tx, ctx, index, id, record, &present, extra)
            .await
            .map(|()| "updated"),
    };
    match outcome {
        Ok(o) => Ok(Ok(o)),
        Err(err @ ApiError::Internal(_)) => Err(err),
        Err(ApiError::Invalid(issues)) => Ok(Err(issues
            .into_iter()
            .map(|i| (Some(i.field), i.message))
            .collect())),
        Err(other) => Ok(Err(vec![(None, other.to_string())])),
    }
}

async fn organisation_for(
    tx: &mut Tx,
    ctx: &Ctx,
    index: &mut Index,
    name: Option<&str>,
    extra: &Value,
) -> ApiResult<Option<Uuid>> {
    let Some(name) = name else { return Ok(None) };
    let key = name_key(name);
    if let Some(id) = index.organisations.get(&key) {
        return Ok(Some(*id));
    }
    ctx.authorize(Permission::OrganisationWrite)?;
    let org = organisations::create_in_tx(
        tx,
        ctx,
        NewOrganisation {
            name: name.to_owned(),
            ..Default::default()
        },
        Some(extra.clone()),
    )
    .await?;
    index.organisations.insert(key, org.id);
    Ok(Some(org.id))
}

async fn create(
    tx: &mut Tx,
    ctx: &Ctx,
    index: &mut Index,
    record: Record,
    extra: &Value,
) -> ApiResult<()> {
    let id = match &record {
        Record::Talent(n) => {
            talent::create_in_tx(tx, ctx, n.clone(), Some(extra.clone()))
                .await?
                .id
        }
        Record::Organisation(n) => {
            organisations::create_in_tx(tx, ctx, n.clone(), Some(extra.clone()))
                .await?
                .id
        }
        Record::Contact(n, org) => {
            let organisation_id = organisation_for(tx, ctx, index, org.as_deref(), extra).await?;
            let new = NewContact {
                organisation_id,
                ..n.clone()
            };
            contacts::create_in_tx(tx, ctx, new, Some(extra.clone()))
                .await?
                .id
        }
    };
    index.add(&record, id);
    Ok(())
}

async fn update(
    tx: &mut Tx,
    ctx: &Ctx,
    index: &mut Index,
    id: Uuid,
    record: Record,
    present: &HashSet<String>,
    extra: &Value,
) -> ApiResult<()> {
    let has = |k: &str| present.contains(k);
    let custom = |c: &Map<String, Value>| (!c.is_empty()).then(|| c.clone());
    match &record {
        Record::Talent(n) => {
            let current = talent::load(tx, id).await?.ok_or(ApiError::NotFound)?;
            let patch = TalentPatch {
                kind: n.kind.filter(|_| has("kind")),
                name: has("name").then(|| n.name.clone()),
                legal_name: given(present, "legal_name", &n.legal_name),
                talent_type: given(present, "talent_type", &n.talent_type),
                bio: given(present, "bio", &n.bio),
                base_location: given(present, "base_location", &n.base_location),
                default_fee: given(present, "default_fee", &n.default_fee),
                commission_percent: given(present, "commission_percent", &n.commission_percent),
                status: n.status.filter(|_| has("status")),
                portal_user_id: None,
                tags: Some(merged(&current.tags, &n.tags)),
                custom: custom(&n.custom),
            };
            talent::update_in_tx(tx, ctx, id, patch, None, Some(extra.clone())).await?;
        }
        Record::Organisation(n) => {
            let current = organisations::load(tx, id)
                .await?
                .ok_or(ApiError::NotFound)?;
            let mut addresses = current.addresses.clone();
            for a in &n.addresses {
                if !addresses.contains(a) {
                    addresses.push(a.clone());
                }
            }
            let patch = OrganisationPatch {
                name: has("name").then(|| n.name.clone()),
                org_type: given(present, "org_type", &n.org_type),
                addresses: (!n.addresses.is_empty()).then_some(addresses),
                tax_id: given(present, "tax_id", &n.tax_id),
                default_currency: given(present, "default_currency", &n.default_currency),
                payment_terms_days: given(present, "payment_terms_days", &n.payment_terms_days),
                notes: given(present, "notes", &n.notes),
                tags: Some(merged(&current.tags, &n.tags)),
                custom: custom(&n.custom),
            };
            organisations::update_in_tx(tx, ctx, id, patch, None, Some(extra.clone())).await?;
        }
        Record::Contact(n, org) => {
            let current = contacts::load(tx, id).await?.ok_or(ApiError::NotFound)?;
            let organisation_id = match org {
                Some(name) => Some(organisation_for(tx, ctx, index, Some(name), extra).await?),
                None => None,
            };
            let emails: Vec<String> = n.emails.iter().map(|e| email_key(e)).collect();
            let patch = ContactPatch {
                name: has("name").then(|| n.name.clone()),
                organisation_id,
                emails: Some(merged(&current.emails, &emails)),
                phones: Some(merged(&current.phones, &n.phones)),
                role: given(present, "role", &n.role),
                preferred_channel: given(present, "preferred_channel", &n.preferred_channel),
                email_opt_in: has("email_opt_in").then_some(n.email_opt_in),
                sms_opt_in: has("sms_opt_in").then_some(n.sms_opt_in),
                whatsapp_opt_in: has("whatsapp_opt_in").then_some(n.whatsapp_opt_in),
                notes: given(present, "notes", &n.notes),
                tags: Some(merged(&current.tags, &n.tags)),
                custom: custom(&n.custom),
            };
            contacts::update_in_tx(tx, ctx, id, patch, None, Some(extra.clone())).await?;
        }
    }
    index.add(&record, id);
    Ok(())
}

/// `crm.import`: runs a queued import in batches, resuming after the last
/// batch that committed. Stops after `budget` and queues a job for the rest.
pub async fn execute(
    state: &AppState,
    workspace: WorkspaceId,
    id: Uuid,
    budget: std::time::Duration,
) -> ApiResult<()> {
    let deadline = std::time::Instant::now() + budget;
    let mut tx = core_db::begin(&state.db, Some(workspace), None).await?;
    let Some(import) = core_db::imports::get(&mut tx, id).await? else {
        return Ok(());
    };
    if !matches!(import.status.as_str(), "queued" | "running") {
        return Ok(());
    }
    let csv = core_db::imports::csv(&mut tx, id).await?;
    let plan: Option<ImportPlan> = import.mapping.as_ref().and_then(|columns| {
        serde_json::from_value(json!({ "columns": columns, "options": import.options })).ok()
    });
    let (Some(csv), Some(plan)) = (csv, plan) else {
        core_db::imports::finish(
            &mut tx,
            id,
            "failed",
            Some("the file or its mapping is missing"),
        )
        .await?;
        tx.commit().await?;
        return Ok(());
    };
    let table = match engine::parse_csv(&csv) {
        Ok(t) => t,
        Err(m) => {
            core_db::imports::finish(&mut tx, id, "failed", Some(&m)).await?;
            tx.commit().await?;
            return Ok(());
        }
    };
    let setup = match setup(&mut tx, &import, plan).await {
        Ok(s) => s,
        // The fields changed since the run was queued.
        Err(err @ ApiError::Invalid(_)) => {
            core_db::imports::finish(&mut tx, id, "failed", Some(&err_text(&err))).await?;
            tx.commit().await?;
            return Ok(());
        }
        Err(err) => return Err(err),
    };
    // Checked again by every job of a long import.
    let Some(ctx) = import_ctx(
        &mut tx,
        workspace,
        import.created_by,
        &import.entity,
        &setup.options,
    )
    .await?
    else {
        let error = "the member who started this import may no longer import these records";
        core_db::imports::finish(&mut tx, id, "failed", Some(error)).await?;
        tx.commit().await?;
        return Ok(());
    };
    core_db::imports::start(&mut tx, id).await?;
    tx.commit().await?;

    let extra = json!({ "import": { "id": id, "run_rules": setup.options.run_rules } });
    let reader = setup.reader();
    let mut stored = i32::try_from(import.issues.as_array().map_or(0, Vec::len)).unwrap_or(0);
    let start = usize::try_from(import.processed).unwrap_or(0);

    let mut tx = super::begin(state, &ctx).await?;
    let mut index = Index::load(&mut tx, &import.entity).await?;
    tx.commit().await?;

    for (b, batch) in table.rows.chunks(BATCH).enumerate() {
        let first = b * BATCH;
        if first + batch.len() <= start {
            continue;
        }
        let mut tx = super::begin(state, &ctx).await?;
        let mut counts = core_db::imports::Counts::default();
        let mut issues: Vec<ImportIssue> = Vec::new();
        for (j, row) in batch.iter().enumerate() {
            let i = first + j;
            if i < start {
                continue;
            }
            let plan = setup.plan(&reader, &index, row);
            match apply(&mut tx, &ctx, &mut index, plan, &extra).await? {
                Ok("created") => counts.created += 1,
                Ok("updated") => counts.updated += 1,
                Ok(_) => counts.skipped += 1,
                Err(list) => {
                    counts.failed += 1;
                    if stored < MAX_STORED_ISSUES {
                        let found = issues_for(table.row_numbers[i], list);
                        stored += i32::try_from(found.len()).unwrap_or(0);
                        issues.extend(found);
                    }
                }
            }
        }
        let processed = i32::try_from(first + batch.len()).unwrap_or(i32::MAX);
        let issues = serde_json::to_value(&issues).map_err(ApiError::internal)?;
        core_db::imports::progress(&mut tx, id, processed, counts, &issues, MAX_STORED_ISSUES)
            .await?;
        // Out of time with rows left: a fresh job carries on from here, so
        // no single job runs into the worker's timeout.
        let more = first + batch.len() < table.rows.len();
        if more && std::time::Instant::now() >= deadline {
            let payload = json!({ "import_id": id });
            let key = format!("{id}:{processed}");
            core_db::jobs::enqueue(
                &mut tx,
                NewJob::new(Some(workspace), JOB_IMPORT, &payload).dedupe(&key),
            )
            .await?;
            tx.commit().await?;
            return Ok(());
        }
        tx.commit().await?;
    }

    let mut tx = core_db::begin(&state.db, Some(workspace), None).await?;
    core_db::imports::finish(&mut tx, id, "done", None).await?;
    tx.commit().await?;
    Ok(())
}

fn err_text(err: &ApiError) -> String {
    match err {
        ApiError::Invalid(issues) => issues
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; "),
        other => other.to_string(),
    }
}

/// Marks an import failed when its job has given up for good.
pub async fn abandon(
    state: &AppState,
    workspace: WorkspaceId,
    id: Uuid,
    error: &str,
) -> ApiResult<()> {
    let mut tx = core_db::begin(&state.db, Some(workspace), None).await?;
    if let Some(i) = core_db::imports::get(&mut tx, id).await?
        && matches!(i.status.as_str(), "queued" | "running")
    {
        core_db::imports::finish(&mut tx, id, "failed", Some(error)).await?;
    }
    tx.commit().await?;
    Ok(())
}
