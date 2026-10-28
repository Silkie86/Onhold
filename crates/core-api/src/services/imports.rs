//! Importing talent, organisations or contacts from a spreadsheet, or from
//! the export of the app a workspace is moving from.
//!
//! 1. Upload the CSV or contact cards (vCard): it is parsed, kept, and
//!    answered with its columns, a sample and a suggested mapping of columns
//!    to fields. Exports from apps we know (Google Contacts, Outlook,
//!    HubSpot...) are recognised and mapped without guessing, and what the
//!    file holds can be left for the upload to work out.
//! 2. Dry run with a mapping: every row is read and checked exactly as the
//!    real run would, nothing is written, and the result says how many rows
//!    would be created, updated, skipped or refused, and why, with the rows
//!    at fault so they can be corrected (or left out) before the run.
//! 3. Run: the worker imports in batches, recording progress, so a large
//!    file neither times out nor half-finishes; a crash resumes where it
//!    stopped. The file is dropped when the run ends.
//!
//! Rows matching an existing record (contacts by email or phone,
//! organisations and talent by name) are skipped, updated or created
//! anyway, as chosen. Imported records do not trigger automation rules
//! unless asked, so an import never mass-mails anyone by surprise.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use core_db::Tx;
use core_db::jobs::NewJob;
use core_domain::crm::{email_key, name_key, phone_key};
use core_domain::{Actor, Ctx, Money, Permission, TalentKind, UserId, WorkspaceId};
use core_engines::fields::FieldDef;
use core_engines::import::{self as engine, DateOrder, Reading, Target};
use core_engines::{import_sources, vcard};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use super::contacts::{self, Channel, ContactPatch, NewContact};
use super::organisations::{
    self, Address, ItemInput, NewOrganisation, OrganisationPatch, Relationship,
};
use super::pitching::{ItemKind, PitchingSettings, Status};
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
    /// Corrections made while checking: cells read as these values instead.
    #[serde(default)]
    pub edits: Vec<CellEdit>,
    /// Spreadsheet rows to leave out.
    #[serde(default)]
    pub skip_rows: Vec<usize>,
}

/// One cell corrected before importing.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CellEdit {
    /// The spreadsheet row (the header is row 1), as problems give it.
    pub row: usize,
    /// The column, counting from 0.
    pub column: usize,
    pub value: String,
}

/// The most corrections one import takes, and the longest corrected cell.
const MAX_EDITS: usize = 5_000;
const MAX_EDIT_CHARS: usize = 10_000;

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            on_match: OnMatch::Skip,
            date_order: None,
            default_currency: None,
            create_organisations: true,
            tags: Vec::new(),
            run_rules: false,
            edits: Vec::new(),
            skip_rows: Vec::new(),
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
    /// The row in the file as uploaded (the header is row 1). For a
    /// workbook of several sheets read together, the row in the joined
    /// table; `at` says where it is in its own sheet.
    pub row: usize,
    /// Where the row is in its workbook when several sheets were read:
    /// `Europe, row 5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
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
    /// The app the file was recognised as an export of, if any.
    pub source: Option<ImportSource>,
    /// A workbook's sheets, and which of them are in the import.
    pub sheets: Vec<ImportSheet>,
}

/// One sheet of an uploaded workbook.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportSheet {
    pub name: String,
    /// Rows under its header.
    pub rows: usize,
    /// Its rows are imported, with its name in the `Sheet` column. A sheet
    /// whose columns are unlike the first one's (notes, totals) is left out.
    pub included: bool,
}

/// An app whose export was recognised.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportSource {
    /// `google`, `outlook`, `vcard`, `hubspot`, `salesforce`, `pipedrive`,
    /// `mailchimp` or `linkedin`.
    pub key: String,
    pub label: String,
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
    /// The first rows with problems (up to 100), corrections applied, to
    /// correct or leave out.
    pub problem_rows: Vec<ProblemRow>,
    /// Contacts an organisation import would add: the person each row
    /// names, unless they are already there.
    pub new_contacts: usize,
    /// Rows that would write something, but are worth a look first (up to
    /// 100): notes saying a place closed or not to contact them, a contact
    /// with no name or left out. Leave them out with `skip_rows`.
    pub to_check: Vec<ProblemRow>,
    /// All the rows to check, of which `to_check` holds the first.
    pub to_check_total: usize,
    /// How values that needed reading were read, each once with the rows
    /// giving it (up to 100 read, and up to 100 that could not be):
    /// `Amber` as a status, a sheet name as a region, `Holland` as the
    /// Netherlands.
    pub readings: Vec<ValueReading>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ValueReading {
    pub field: String,
    pub text: String,
    /// What it was read as; `null` when it could not be, and is left out.
    pub read_as: Option<String>,
    /// What to do about it, when there is something to do.
    pub hint: Option<String>,
    pub rows: usize,
}

const MAX_READINGS: usize = 100;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProblemRow {
    /// The row in the file as uploaded (the header is row 1), as
    /// corrections and `skip_rows` give it.
    pub row: usize,
    /// Where it is in its workbook when several sheets were read.
    pub at: Option<String>,
    /// Its cells, in column order.
    pub cells: Vec<String>,
    /// What is wrong with it.
    pub issues: Vec<ImportIssue>,
}

const MAX_PROBLEM_ROWS: usize = 100;

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct NewImport {
    /// `talent`, `organisation` or `contact`, or `auto` to work it out
    /// from the file.
    pub entity: String,
    pub filename: Option<String>,
    /// The file's text: CSV with a header row, separated by commas,
    /// semicolons or tabs, or contact cards (vCard, `.vcf`). Leave it out
    /// when sending `workbook`.
    #[serde(default)]
    pub csv: String,
    /// An Excel workbook (`.xlsx`), base64: its first visible sheet with
    /// anything on it is imported, with every other sheet that has the same
    /// columns (the sheet's name in a `Sheet` column, and each row's fill
    /// colour in a `Colour` column). Up to 7 MB.
    pub workbook: Option<String>,
}

/// The largest workbook accepted (it travels as base64, a third larger).
pub const MAX_WORKBOOK_BYTES: usize = 7 * 1024 * 1024;

/// Asks the upload to work out what the file holds.
pub const AUTO: &str = "auto";

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
    /// Spellings that suggest it only when no other column does.
    weak: &'static [&'static str],
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
        weak: &[],
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
        weak: &[],
    }
}

const fn name(synonyms: &'static [&'static str]) -> CoreTarget {
    weak_name(synonyms, &[])
}

/// The name, with spellings that name it only on a sheet with no other
/// name column (`Agency` beside `Artist Name` is the artist's agency).
const fn weak_name(synonyms: &'static [&'static str], weak: &'static [&'static str]) -> CoreTarget {
    CoreTarget {
        key: "name",
        label: "Name",
        many: true,
        required: true,
        synonyms,
        weak,
    }
}

const TAGS: CoreTarget = many("tags", "Tags", &["labels", "tag", "label", "categories"]);
const LINK_SYNONYMS: &[&str] = &[
    "website",
    "web",
    "url",
    "site",
    "instagram",
    "facebook",
    "twitter",
    "linkedin",
    "socials",
    "social",
    "social media",
    "links",
];
const NOTES: CoreTarget = many(
    "notes",
    "Notes",
    &["note", "comments", "comment", "description"],
);

fn core_targets(entity: &str) -> Vec<CoreTarget> {
    match entity {
        talent::ENTITY => vec![
            weak_name(
                &[
                    "stage name",
                    "artist",
                    "artist name",
                    "act",
                    "act name",
                    "talent",
                    "talent name",
                    "band name",
                    "dj name",
                ],
                &["performer", "band", "dj", "client", "client name", "roster"],
            ),
            t("kind", "Kind", &[]),
            t(
                "talent_type",
                "Type",
                &[
                    "talent type",
                    "category",
                    "genre type",
                    "act type",
                    "artist type",
                    "discipline",
                ],
            ),
            t(
                "legal_name",
                "Legal name",
                &["real name", "full legal name", "birth name"],
            ),
            many(
                "bio",
                "Bio",
                &[
                    "biography",
                    "about",
                    "description",
                    "short bio",
                    "press bio",
                    "artist bio",
                ],
            ),
            t(
                "base_location",
                "Based in",
                &[
                    "location",
                    "base",
                    "city",
                    "hometown",
                    "home town",
                    "home city",
                    "home base",
                    "based",
                ],
            ),
            t(
                "default_fee",
                "Default fee",
                &[
                    "fee",
                    "rate",
                    "price",
                    "booking fee",
                    "standard fee",
                    "asking fee",
                    "minimum fee",
                    "guarantee",
                ],
            ),
            t(
                "commission_percent",
                "Commission %",
                &[
                    "commission",
                    "commission percent",
                    "commission rate",
                    "agency commission",
                ],
            ),
            t("status", "Status", &["active"]),
            many(
                "tags",
                "Tags",
                // Not `label`: an artist's label is a record label.
                &["labels", "tag", "categories", "genre", "genres", "style"],
            ),
        ],
        organisations::ENTITY => vec![
            weak_name(
                &[
                    "organisation",
                    "organization",
                    "company",
                    "company name",
                    "venue",
                    "venue name",
                    "organisation name",
                    "organization name",
                    "business",
                ],
                &["account name", "promoter", "festival", "agency"],
            ),
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
            t(
                "address.region",
                "County / state",
                &["state", "county", "province", "county state"],
            ),
            t(
                "address.postcode",
                "Postcode",
                &["post code", "postal code", "zip", "zip code"],
            ),
            t("address.country", "Country", &["country code"]),
            // Pitching: where things stand with them.
            t(
                "region",
                "Region",
                &[
                    "sheet",
                    "territory",
                    "area",
                    "market",
                    "zone",
                    "pitching region",
                ],
            ),
            t(
                "relationship",
                "Client or prospect",
                &[
                    "client or prospect",
                    "client prospect",
                    "relationship",
                    "client status",
                ],
            ),
            t(
                "status",
                "Status",
                &[
                    "colour",
                    "color",
                    "stage",
                    "pitch status",
                    "pitching status",
                    "progress",
                ],
            ),
            t(
                "follow_up_on",
                "Follow up on",
                &[
                    "follow up",
                    "follow up date",
                    "next follow up",
                    "chase",
                    "chase date",
                    "next contact",
                    "next chase",
                    "follow up by",
                ],
            ),
            t(
                "do_not_contact",
                "Do not contact",
                &["dnc", "dont contact", "no contact", "do not email"],
            ),
            many(
                "buys_from_others",
                "Buys from others",
                &[
                    "buys from",
                    "other agencies",
                    "competitors",
                    "works with",
                    "books from",
                ],
            ),
            many(
                "interested_in",
                "Interested in",
                &["interested", "interest", "booked before", "asked about"],
            ),
            many(
                "potential_matches",
                "Potential matches",
                &[
                    "potential match",
                    "matches",
                    "suggested",
                    "suggestions",
                    "could book",
                ],
            ),
            // The person to talk to there, added as one of its contacts.
            many(
                "contact.name",
                "Contact name",
                &[
                    "contact",
                    "contact name",
                    "contact person",
                    "booker",
                    "booker name",
                    "promoter name",
                    "first name",
                    "last name",
                    "surname",
                ],
            ),
            many(
                "contact.emails",
                "Contact email",
                &[
                    "email",
                    "e-mail",
                    "email address",
                    "contact email",
                    "booker email",
                    "emails",
                ],
            ),
            many(
                "contact.phones",
                "Contact phone",
                &[
                    "phone",
                    "telephone",
                    "tel",
                    "mobile",
                    "phone number",
                    "contact phone",
                    "mobile number",
                ],
            ),
            t(
                "contact.role",
                "Contact role",
                &["role", "job title", "position", "contact role"],
            ),
            many("contact.links", "Links", LINK_SYNONYMS),
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
                "middle name",
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
                    "primary email",
                    "email address 1",
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
                    "mobile phone",
                    "business phone",
                    "primary phone",
                    "cell phone",
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
            many("links", "Links", LINK_SYNONYMS),
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
                    weak_synonyms: c.weak.iter().map(|s| (*s).to_owned()).collect(),
                    many: c.many,
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
                weak_synonyms: Vec::new(),
                many: false,
            },
        ));
    }
    out
}

// Upload and inspect ------------------------------------------------------------

/// The suggested mapping: where a recognised export says each column goes,
/// else the general guess, with a one-column field given once.
fn suggest(
    headers: &[String],
    targets: &[(ImportTarget, Target)],
    source: Option<&import_sources::Source>,
) -> Vec<Option<String>> {
    let engine_targets: Vec<Target> = targets.iter().map(|(_, t)| t.clone()).collect();
    let guessed = engine::suggest_mapping(headers, &engine_targets);
    let Some(source) = source else {
        return guessed;
    };
    let known = |k: &String| engine_targets.iter().any(|t| &t.key == k);
    let columns = source
        .columns_for(headers)
        .into_iter()
        .zip(guessed)
        .map(|(said, guess)| match said {
            Some(target) => target.filter(known),
            None => guess,
        })
        .collect();
    engine::once_each(columns, &engine_targets)
}

/// What a file of unknown contents most likely holds: the kind of record
/// whose fields its columns match best, counting each field once and a
/// name column most, contacts when it is a tie. (A roster with agents'
/// emails and phones is still talent, by its artist name and fee.)
fn likely_entity(headers: &[String]) -> &'static str {
    let mut best = (contacts::ENTITY, 0);
    for entity in [contacts::ENTITY, talent::ENTITY, organisations::ENTITY] {
        let targets = targets(entity, &[]);
        let mut fields: HashSet<String> = suggest(headers, &targets, None)
            .into_iter()
            .flatten()
            .collect();
        // An organisation's contact person counts once the sheet says more
        // about the organisation than its name (a pitching list); otherwise
        // a sheet of people with their company is contacts.
        if entity == organisations::ENTITY
            && !fields.iter().any(|f| {
                !matches!(f.as_str(), "name" | "notes" | "tags") && !f.starts_with("contact.")
            })
        {
            fields.retain(|f| !f.starts_with("contact."));
        }
        let matched = fields.len() + if fields.contains("name") { 10 } else { 0 };
        if matched > best.1 {
            best = (entity, matched);
        }
    }
    best.0
}

pub async fn upload(state: &AppState, ctx: &Ctx, mut new: NewImport) -> ApiResult<ImportPreview> {
    if new.entity != AUTO {
        ctx.authorize(write_permission(&new.entity)?)?;
    }
    if new.csv.len() > MAX_CSV_BYTES {
        return Err(ApiError::invalid(
            "csv",
            format!("the file is larger than {} MB", MAX_CSV_BYTES / 1024 / 1024),
        ));
    }
    let filename = super::optional_text("filename", new.filename.take(), 255)?;
    let mut sheets = Vec::new();
    if let Some(encoded) = new.workbook.take() {
        let bytes = data_encoding::BASE64
            .decode(encoded.trim().as_bytes())
            .map_err(|_| ApiError::invalid("workbook", "send the file as base64"))?;
        if bytes.len() > MAX_WORKBOOK_BYTES {
            return Err(ApiError::invalid(
                "workbook",
                format!(
                    "the file is larger than {} MB",
                    MAX_WORKBOOK_BYTES / 1024 / 1024
                ),
            ));
        }
        if !core_engines::workbook::is_workbook(&bytes) {
            return Err(ApiError::invalid(
                "workbook",
                "this is not an Excel (.xlsx) workbook; save it as .xlsx or CSV",
            ));
        }
        // Unpacking and reading takes a while: off the request threads.
        let book = tokio::task::spawn_blocking(move || core_engines::workbook::read(&bytes))
            .await
            .map_err(ApiError::internal)?
            .map_err(|m| ApiError::invalid("workbook", m))?;
        new.csv = book.csv;
        sheets = book
            .sheets
            .into_iter()
            .map(|s| ImportSheet {
                name: s.name,
                rows: s.rows,
                included: s.included,
            })
            .collect();
        if new.csv.len() > MAX_CSV_BYTES {
            return Err(ApiError::invalid(
                "workbook",
                format!(
                    "the sheet is larger than {} MB as text",
                    MAX_CSV_BYTES / 1024 / 1024
                ),
            ));
        }
    }
    // Contact cards are kept as the table they read as: smaller, without
    // their photos, and read again like any spreadsheet.
    let (table, csv, source) = if vcard::is_vcard(&new.csv) {
        let table = vcard::parse(&new.csv).map_err(|m| ApiError::invalid("csv", m))?;
        let csv = vcard::to_csv(&table).map_err(ApiError::internal)?;
        (table, csv, Some(&import_sources::VCARD))
    } else {
        let table = engine::parse_csv(&new.csv).map_err(|m| ApiError::invalid("csv", m))?;
        let source = import_sources::detect(&table.headers);
        (table, new.csv, source)
    };
    if table.rows.is_empty() {
        return Err(ApiError::invalid(
            "csv",
            "the file has no rows under its header",
        ));
    }
    let entity = match (new.entity.as_str(), source) {
        (AUTO, Some(s)) => s.entity.to_owned(),
        (AUTO, None) => likely_entity(&table.headers).to_owned(),
        (chosen, _) => chosen.to_owned(),
    };
    if new.entity == AUTO {
        ctx.authorize(write_permission(&entity)?)?;
    }
    // A recognised export of other records (a HubSpot companies file
    // uploaded as contacts) is mapped as a spreadsheet would be.
    let source = source.filter(|s| s.entity == entity);
    let mut tx = begin(state, ctx).await?;
    let defs = super::fields::engine_defs(&mut tx, &entity).await?;
    let targets = targets(&entity, &defs);
    let suggested = suggest(&table.headers, &targets, source);
    let id = core_domain::ids::new_id();
    core_db::imports::insert(
        &mut tx,
        id,
        core_db::imports::New {
            entity: &entity,
            filename: filename.as_deref(),
            csv: &csv,
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
        source: source.map(|s| ImportSource {
            key: s.key.to_owned(),
            label: s.label.to_owned(),
        }),
        sheets,
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

/// A row read into the record it describes. One lives at a time, so its
/// size does not matter.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
enum Record {
    Talent(NewTalent),
    /// With the person the row names there, if it names one.
    Organisation(NewOrganisation, Option<NewContact>),
    /// With the organisation's name, if the row gives one.
    Contact(NewContact, Option<String>),
}

impl Record {
    fn name(&self) -> &str {
        match self {
            Record::Talent(n) => &n.name,
            Record::Organisation(n, _) => &n.name,
            Record::Contact(n, _) => &n.name,
        }
    }
}

/// A problem or a thing to check: the field, if there is one, and what.
type RowNote = (Option<String>, String);

/// How a cell was read.
#[derive(Clone)]
struct ValueRead {
    field: String,
    text: String,
    read_as: Option<String>,
    hint: Option<String>,
}

fn value_read(field: &str, text: &str, read_as: Option<String>, hint: Option<&str>) -> ValueRead {
    ValueRead {
        field: field.to_owned(),
        text: text.to_owned(),
        read_as,
        hint: hint.map(str::to_owned),
    }
}

/// A row, read: the record, the fields the row gave values for, any
/// problems reading it, things worth a second look, and how values that
/// needed interpreting were read.
struct ReadRow {
    record: Record,
    present: HashSet<String>,
    issues: Vec<RowNote>,
    notes: Vec<RowNote>,
    readings: Vec<ValueRead>,
}

/// Everything fixed for one import: the mapping, the field definitions and
/// how to read cells.
struct Reader<'a> {
    entity: &'a str,
    columns: &'a [Option<String>],
    defs: &'a [FieldDef],
    reading: Reading,
    extra_tags: Vec<String>,
    settings: &'a PitchingSettings,
    /// Talent by name key, for the talent an organisation is interested in.
    talent: &'a HashMap<String, Uuid>,
    /// Cells already read, by field and text: a list repeats its regions,
    /// colours and countries on every row.
    memo: std::cell::RefCell<HashMap<(&'static str, String), Memo>>,
}

/// A cell read once: the key it gives, and how it was read.
type Memo = (Option<String>, Option<ValueRead>);

/// The most distinct cells remembered while reading.
const MAX_MEMO: usize = 10_000;

fn joined(values: &[&str]) -> String {
    values.join(" ")
}

/// Links in cells, separated by spaces, semicolons, commas or bars. Far
/// more than a contact holds stops early: the check refuses them anyway.
fn split_links(cells: Option<&Vec<&str>>) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for link in cells
        .into_iter()
        .flatten()
        .flat_map(|c| c.split(|ch: char| ch.is_whitespace() || matches!(ch, ';' | ',' | '|')))
        .filter(|l| !l.is_empty())
    {
        if seen.insert(link) {
            out.push(link.to_owned());
            if out.len() > 2 * contacts::MAX_LINKS {
                break;
            }
        }
    }
    out
}

/// Whether a contact's name only stands in for one: an email address or a
/// number, as a person with no name is called.
fn is_stand_in(name: &str) -> bool {
    name.contains('@') || !name.chars().any(char::is_alphabetic)
}

/// Phrases in a row's notes that say not to contact them.
const DO_NOT_CONTACT_PHRASES: &[&str] = &[
    "do not contact",
    "don't contact",
    "dont contact",
    "dnc",
    "do not email",
    "don't email",
    "unsubscribe",
    "unsubscribed",
];

/// Phrases in a row's notes that make it worth a look before importing.
const CHECK_PHRASES: &[&str] = &[
    "closed",
    "closing down",
    "shut down",
    "out of business",
    "no longer",
    "moved to",
    "has moved",
    "wrong number",
    "bounced",
    "duplicate",
    "tbc",
    "tbd",
    "not sure",
    "check",
];

/// What a row's notes say: whether not to contact them, and the first
/// phrase worth a look. Phrases match whole words, whatever the case.
fn scan_notes(notes: &str) -> (bool, Option<&'static str>) {
    let words = |s: &str| -> String {
        let cleaned: String = s
            .to_lowercase()
            .replace('\u{2019}', "'")
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '\'' {
                    c
                } else {
                    ' '
                }
            })
            .collect();
        format!(
            " {} ",
            cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
        )
    };
    let text = words(notes);
    // The phrases are written as `words` leaves them.
    let has = |p: &str| text.contains(&format!(" {p} "));
    (
        DO_NOT_CONTACT_PHRASES.iter().any(|p| has(p)),
        CHECK_PHRASES.iter().find(|p| has(p)).copied(),
    )
}

/// The region a cell names, by its key or name, whatever the case, and
/// without what its name says in brackets (`Europe` for "Europe (incl. UK)").
fn region_named<'s>(
    settings: &'s PitchingSettings,
    raw: &str,
) -> Option<&'s super::pitching::Region> {
    let key = name_key(raw);
    settings.regions.iter().find(|r| {
        let short = r.name.split('(').next().unwrap_or_default();
        r.key == key.replace(' ', "_") || name_key(&r.name) == key || name_key(short) == key
    })
}

/// The status a cell names: by key, by label, or by colour (`Amber`, a
/// row's fill colour) as the first status of that colour.
fn status_named<'s>(settings: &'s PitchingSettings, raw: &str) -> Option<&'s Status> {
    let key = name_key(raw);
    let colour = match key.as_str() {
        "orange" | "yellow" => "amber",
        "gray" => "grey",
        "violet" => "purple",
        other => other,
    };
    settings
        .statuses
        .iter()
        .find(|s| s.key == key.replace(' ', "_") || name_key(&s.label) == key)
        .or_else(|| {
            settings
                .statuses
                .iter()
                .find(|s| s.colour.as_str() == colour)
        })
}

/// Client or prospect, however a sheet puts it.
fn relationship_named(raw: &str) -> Option<Relationship> {
    Relationship::parse(raw).or_else(|| match name_key(raw).as_str() {
        "existing" | "current" | "existing client" | "current client" | "booked before" => {
            Some(Relationship::Client)
        }
        "target" | "new" | "potential" | "cold" | "warm" | "new lead" => {
            Some(Relationship::Prospect)
        }
        _ => None,
    })
}

/// How a country cell was read, when that took some reading: a name other
/// than the country's own, a city, a near miss, or nothing at all.
fn country_read(raw: &str) -> Option<ValueRead> {
    use core_engines::countries::{How, find};
    let Some(found) = find(raw) else {
        return Some(value_read(
            "address.country",
            raw,
            None,
            Some("not a country OnHold knows; kept as typed"),
        ));
    };
    let c = found.country;
    let plain = found.how == How::Name
        && found.others.is_empty()
        && (raw.trim().eq_ignore_ascii_case(c.name) || raw.trim().eq_ignore_ascii_case(c.code));
    if plain {
        return None;
    }
    let mut read_as = c.name.to_owned();
    match found.how {
        How::Typo => read_as.push_str(" (a near miss)"),
        How::City => read_as.push_str(" (from the city)"),
        How::Name => {}
    }
    let hint = (!found.others.is_empty()).then(|| {
        let others: Vec<&str> = found.others.iter().map(|o| o.name).collect();
        format!(
            "also names {}; correct the rows if needed",
            others.join(", ")
        )
    });
    Some(ValueRead {
        field: "address.country".into(),
        text: raw.to_owned(),
        read_as: Some(read_as),
        hint,
    })
}

impl Reader<'_> {
    /// Reads a cell of a field once, reusing what it gave before.
    fn memo(&self, field: &'static str, raw: &str, read: impl FnOnce() -> Memo) -> Memo {
        let key = (field, raw.to_owned());
        if let Some(m) = self.memo.borrow().get(&key) {
            return m.clone();
        }
        let m = read();
        let mut memo = self.memo.borrow_mut();
        if memo.len() < MAX_MEMO {
            memo.insert(key, m.clone());
        }
        m
    }

    #[allow(clippy::too_many_lines)]
    fn read(&self, row: &[String]) -> ReadRow {
        let mut cells: HashMap<&str, Vec<&str>> = HashMap::new();
        for (target, cell) in self.columns.iter().zip(row) {
            if let Some(t) = target
                && !cell.trim().is_empty()
            {
                cells.entry(t.as_str()).or_default().push(cell.trim());
            }
        }
        let mut present: HashSet<String> = cells.keys().map(|k| (*k).to_owned()).collect();
        let mut issues: Vec<RowNote> = Vec::new();
        let mut notes: Vec<RowNote> = Vec::new();
        let mut readings: Vec<ValueRead> = Vec::new();
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
        let mut tags: Vec<String> = list("tags")
            .iter()
            .filter_map(|t| engine::clean_label(t))
            .collect();
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
        let row_notes = lines("notes");
        let (notes_say_dnc, notes_check) = row_notes.as_deref().map_or((false, None), scan_notes);
        if let Some(phrase) = notes_check {
            notes.push((
                Some("notes".into()),
                format!("The notes say \u{201c}{phrase}\u{201d}: worth a look"),
            ));
        }

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
                if let Some(raw) = &address.country
                    && let (_, Some(read)) =
                        self.memo("address.country", raw, || (None, country_read(raw)))
                {
                    readings.push(read);
                }
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
                let addresses = if address.is_empty() {
                    Vec::new()
                } else {
                    vec![address]
                };
                let mut by_memo = |field: &'static str, raw: String, read: &dyn Fn() -> Memo| {
                    let (key, how) = self.memo(field, &raw, read);
                    readings.extend(how);
                    if key.is_none() {
                        present.remove(field);
                    }
                    key
                };
                // A region the country gives anyway is let go when the
                // organisation is created, so it follows the country.
                let region = text("region").and_then(|raw| {
                    by_memo("region", raw.clone(), &|| match region_named(self.settings, &raw) {
                        Some(r) => (
                            Some(r.key.clone()),
                            Some(value_read("region", &raw, Some(r.name.clone()), None)),
                        ),
                        None => {
                            let is_country =
                                core_engines::countries::find(&raw).is_some_and(|f| {
                                    f.how == core_engines::countries::How::Name
                                        && f.others.is_empty()
                                });
                            let hint = if is_country {
                                "a country, not a region: import this column as Country instead"
                            } else {
                                "not one of your regions: add it in Settings \u{2192} Pitching, or leave it"
                            };
                            (None, Some(value_read("region", &raw, None, Some(hint))))
                        }
                    })
                });
                let status = text("status").and_then(|raw| {
                    by_memo("status", raw.clone(), &|| {
                        let found = status_named(self.settings, &raw);
                        (
                            found.map(|s| s.key.clone()),
                            Some(value_read(
                                "status",
                                &raw,
                                found.map(|s| s.label.clone()),
                                found.is_none().then_some(
                                    "not one of your statuses: add it in Settings \u{2192} Pitching, or leave it",
                                ),
                            )),
                        )
                    })
                });
                let relationship = text("relationship").and_then(|raw| {
                    let found = relationship_named(&raw);
                    readings.push(value_read(
                        "relationship",
                        &raw,
                        found.map(|r| match r {
                            Relationship::Client => "Client".to_owned(),
                            Relationship::Prospect => "Prospect".to_owned(),
                        }),
                        found
                            .is_none()
                            .then_some("not client or prospect: left to OnHold"),
                    ));
                    if found.is_none() {
                        present.remove("relationship");
                    }
                    found
                });
                let follow_up_on = text("follow_up_on").and_then(|raw| {
                    engine::parse_date(&raw, self.reading.date_order).or_else(|| {
                        issue("follow_up_on", format!("`{raw}` is not a date"));
                        None
                    })
                });
                let mut do_not_contact = flag("do_not_contact", &mut issue);
                if notes_say_dnc && !present.contains("do_not_contact") {
                    do_not_contact = true;
                    present.insert("do_not_contact".into());
                    notes.push((
                        Some("do_not_contact".into()),
                        "The notes say not to contact them, so they are marked do not contact"
                            .into(),
                    ));
                }
                let mut items = |field: &str| -> Vec<ItemInput> {
                    let mut out: Vec<ItemInput> = Vec::new();
                    for n in list(field) {
                        match self.talent.get(&name_key(&n)) {
                            Some(id) => out.push(ItemInput {
                                kind: ItemKind::Talent,
                                id: *id,
                            }),
                            None => readings.push(value_read(
                                field,
                                &n,
                                None,
                                Some("no talent by this name: left out"),
                            )),
                        }
                    }
                    out
                };
                let interested_in = items("interested_in");
                let potential_matches = items("potential_matches");

                // The person to talk to there. With no name, they go by
                // their email or number until someone names them.
                let emails = list("contact.emails");
                let phones = list("contact.phones");
                let person_name = text("contact.name").or_else(|| {
                    let stand_in = emails.first().or(phones.first()).cloned()?;
                    notes.push((
                        Some("contact.name".into()),
                        format!("The contact has no name, so they are called {stand_in} for now"),
                    ));
                    Some(stand_in)
                });
                let person = person_name.map(|name| NewContact {
                    name,
                    emails,
                    phones,
                    role: text("contact.role"),
                    links: split_links(cells.get("contact.links")),
                    tags: self.extra_tags.clone(),
                    ..Default::default()
                });

                Record::Organisation(
                    NewOrganisation {
                        name: text("name").unwrap_or_default(),
                        org_type: text("org_type"),
                        addresses,
                        tax_id: text("tax_id"),
                        default_currency: text("default_currency"),
                        payment_terms_days,
                        notes: row_notes,
                        tags,
                        region,
                        relationship,
                        status,
                        follow_up_on,
                        do_not_contact,
                        buys_from_others: list("buys_from_others"),
                        interested_in,
                        potential_matches,
                        custom,
                    },
                    person,
                )
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
                if notes_say_dnc {
                    notes.push((
                        Some("notes".into()),
                        "The notes say not to contact them: check what they agreed to".into(),
                    ));
                }
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
                        notes: row_notes,
                        tags,
                        custom,
                        links: split_links(cells.get("links")),
                    },
                    text("organisation"),
                )
            }
        };
        ReadRow {
            record,
            present,
            issues,
            notes,
            readings,
        }
    }
}

// Matching ---------------------------------------------------------------------------

/// What rows are matched against: existing records, plus those created
/// earlier in the same import.
#[derive(Default)]
struct Index {
    by_name: HashMap<String, Uuid>,
    /// Contacts, also an organisation's people.
    by_email: HashMap<String, Uuid>,
    by_phone: HashMap<String, Uuid>,
    /// Contacts by name at their organisation: the same name at the same
    /// place is the same person.
    by_name_at: HashMap<(String, Uuid), Uuid>,
    /// Organisations by name, for contacts.
    organisations: HashMap<String, Uuid>,
    /// Contacts' name keys, and whether the name stands in for one (an
    /// email or number), to tell people sharing an address apart.
    names: HashMap<Uuid, (String, bool)>,
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
                index.load_contacts(tx).await?;
            }
            _ => {
                index.load_contacts(tx).await?;
                for (id, name) in core_db::organisations::names(tx).await? {
                    index.organisations.entry(name_key(&name)).or_insert(id);
                }
            }
        }
        Ok(index)
    }

    async fn load_contacts(&mut self, tx: &mut Tx) -> ApiResult<()> {
        for k in core_db::contacts::keys(tx).await? {
            for e in k.emails {
                self.by_email.entry(e).or_insert(k.id);
            }
            for p in k.phone_keys {
                self.by_phone.entry(p).or_insert(k.id);
            }
            if let Some(org) = k.organisation_id {
                self.by_name_at
                    .entry((name_key(&k.name), org))
                    .or_insert(k.id);
            }
            self.names
                .insert(k.id, (name_key(&k.name), is_stand_in(&k.name)));
        }
        Ok(())
    }

    /// A contact by any of their emails or numbers, else by name at the
    /// organisation.
    fn find_contact(&self, c: &NewContact, organisation: Option<Uuid>) -> Option<Uuid> {
        c.emails
            .iter()
            .find_map(|e| self.by_email.get(&email_key(e)))
            .or_else(|| {
                c.phones
                    .iter()
                    .filter_map(|p| phone_key(p))
                    .find_map(|k| self.by_phone.get(&k))
            })
            .or_else(|| organisation.and_then(|org| self.by_name_at.get(&(name_key(&c.name), org))))
            .copied()
    }

    /// The person a row names at an organisation: by name there, else by
    /// an email or number when the names agree (or either is only a stand
    /// in), since a venue's people often share its address and number.
    fn find_person(&self, c: &NewContact, organisation: Uuid) -> Option<Uuid> {
        let key = name_key(&c.name);
        if let Some(id) = self.by_name_at.get(&(key.clone(), organisation)) {
            return Some(*id);
        }
        let row_stand_in = is_stand_in(&c.name);
        c.emails
            .iter()
            .filter_map(|e| self.by_email.get(&email_key(e)))
            .chain(
                c.phones
                    .iter()
                    .filter_map(|p| phone_key(p))
                    .filter_map(|k| self.by_phone.get(&k)),
            )
            .copied()
            .find(|id| {
                row_stand_in
                    || self
                        .names
                        .get(id)
                        .is_none_or(|(name, stand_in)| *stand_in || *name == key)
            })
    }

    fn add_contact(&mut self, c: &NewContact, organisation: Option<Uuid>, id: Uuid) {
        self.names
            .insert(id, (name_key(&c.name), is_stand_in(&c.name)));
        for e in &c.emails {
            self.by_email.entry(email_key(e)).or_insert(id);
        }
        for k in c.phones.iter().filter_map(|p| phone_key(p)) {
            self.by_phone.entry(k).or_insert(id);
        }
        if let Some(org) = organisation {
            self.by_name_at
                .entry((name_key(&c.name), org))
                .or_insert(id);
        }
    }

    fn organisation_named(&self, name: Option<&str>) -> Option<Uuid> {
        name.and_then(|n| self.organisations.get(&name_key(n)).copied())
    }

    fn find(&self, record: &Record) -> Option<Uuid> {
        match record {
            Record::Contact(c, org) => {
                self.find_contact(c, self.organisation_named(org.as_deref()))
            }
            other => self.by_name.get(&name_key(other.name())).copied(),
        }
    }

    fn add(&mut self, record: &Record, id: Uuid) {
        match record {
            Record::Contact(c, org) => {
                self.add_contact(c, self.organisation_named(org.as_deref()), id);
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
    /// The record is there and left alone; the person the row names at an
    /// organisation is still added to it.
    Skip(Option<(Uuid, NewContact)>),
    Fail(Vec<RowNote>),
}

/// The import's mapping, options and field definitions, checked.
struct Setup {
    entity: String,
    columns: Vec<Option<String>>,
    options: ImportOptions,
    defs: Vec<FieldDef>,
    /// Organisations: the contacts' fields, for the person a row names.
    contact_defs: Vec<FieldDef>,
    reading: Reading,
    extra_tags: Vec<String>,
    /// Corrections by spreadsheet row: (column, value).
    edits: HashMap<usize, Vec<(usize, String)>>,
    skip_rows: HashSet<usize>,
    settings: PitchingSettings,
    talent: HashMap<String, Uuid>,
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
    if plan.options.edits.len() > MAX_EDITS {
        check.issue(
            "options.edits",
            format!("make at most {MAX_EDITS} corrections in one import"),
        );
    }
    if plan.options.skip_rows.len() > engine::MAX_ROWS {
        check.issue("options.skip_rows", "too many rows to leave out");
    }
    let mut edits: HashMap<usize, Vec<(usize, String)>> = HashMap::new();
    for (i, e) in plan.options.edits.iter().enumerate() {
        if e.column >= import.headers.len() {
            check.issue(
                format!("options.edits[{i}].column"),
                "there is no such column",
            );
        } else if e.value.chars().count() > MAX_EDIT_CHARS {
            check.issue(format!("options.edits[{i}].value"), "this is too long");
        } else {
            let cells = edits.entry(e.row).or_default();
            // A later correction of the same cell wins.
            cells.retain(|(c, _)| *c != e.column);
            cells.push((e.column, e.value.trim().to_owned()));
        }
    }
    let skip_rows: HashSet<usize> = plan.options.skip_rows.iter().copied().collect();
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
    let (settings, contact_defs, talent) = if import.entity == organisations::ENTITY {
        let mut talent = HashMap::new();
        for (id, name) in core_db::talent::names(tx).await? {
            talent.entry(name_key(&name)).or_insert(id);
        }
        (
            super::pitching::settings_in(tx).await?.0,
            super::fields::engine_defs(tx, contacts::ENTITY).await?,
            talent,
        )
    } else {
        (PitchingSettings::default(), Vec::new(), HashMap::new())
    };
    Ok(Setup {
        entity: import.entity.clone(),
        columns: plan.columns,
        options: plan.options,
        defs,
        contact_defs,
        reading,
        extra_tags,
        edits,
        skip_rows,
        settings,
        talent,
    })
}

impl Setup {
    /// The row with its corrections, or `None` when it is left out.
    fn row<'r>(&self, number: usize, row: &'r [String]) -> Option<Cow<'r, [String]>> {
        if self.skip_rows.contains(&number) {
            return None;
        }
        Some(match self.edits.get(&number) {
            None => Cow::Borrowed(row),
            Some(cells) => {
                let mut row = row.to_vec();
                for (column, value) in cells {
                    if let Some(cell) = row.get_mut(*column) {
                        cell.clone_from(value);
                    }
                }
                Cow::Owned(row)
            }
        })
    }

    fn reader(&self) -> Reader<'_> {
        Reader {
            entity: &self.entity,
            columns: &self.columns,
            defs: &self.defs,
            reading: self.reading.clone(),
            extra_tags: self.extra_tags.clone(),
            settings: &self.settings,
            talent: &self.talent,
            memo: std::cell::RefCell::default(),
        }
    }

    /// Reads and checks one row, and decides what to do with it. Also
    /// gives what is worth a look in it, and how its values were read.
    fn plan(
        &self,
        reader: &Reader<'_>,
        index: &Index,
        row: &[String],
    ) -> (Plan, Vec<RowNote>, Vec<ValueRead>) {
        let mut read = reader.read(row);
        let mut issues = read.issues;
        let check = match &mut read.record {
            Record::Talent(n) => talent::check_new(n, &self.defs).1,
            Record::Organisation(n, person) => {
                let mut c = organisations::check_new(n, &self.defs, &self.settings).1;
                if let Some(p) = person {
                    let found = contacts::check_new(p, &self.contact_defs).1.issues;
                    // Contact fields a sheet of organisations cannot fill
                    // leave the person out, not the organisation.
                    if let Some(i) = found.iter().find(|i| i.field.starts_with("custom")) {
                        read.notes.push((
                            Some("contact.name".into()),
                            format!(
                                "{} was left out: their {} is required for every contact",
                                p.name,
                                i.field.trim_start_matches("custom.")
                            ),
                        ));
                        *person = None;
                    } else {
                        for i in found {
                            c.issue(format!("contact.{}", i.field), i.message);
                        }
                    }
                }
                c
            }
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
        let plan = if issues.is_empty() {
            match (index.find(&read.record), self.options.on_match) {
                (Some(id), OnMatch::Skip) => Plan::Skip(match read.record {
                    Record::Organisation(_, Some(person)) => Some((id, person)),
                    _ => None,
                }),
                (Some(id), OnMatch::Update) => Plan::Update(id, read.record, read.present),
                (_, _) => {
                    let mut record = read.record;
                    // A new organisation follows its country's region
                    // unless the sheet gives another.
                    if let Record::Organisation(n, _) = &mut record
                        && n.region.is_some()
                    {
                        let (country, _) = organisations::place(&json!(n.addresses));
                        let from_country = self.settings.region_of(country.as_deref(), None);
                        if from_country.map(|r| &r.key) == n.region.as_ref() {
                            n.region = None;
                        }
                    }
                    Plan::Create(record)
                }
            }
        } else {
            Plan::Fail(issues)
        };
        (plan, read.notes, read.readings)
    }
}

fn issues_for(
    row: usize,
    at: Option<&String>,
    list: Vec<(Option<String>, String)>,
) -> Vec<ImportIssue> {
    list.into_iter()
        .map(|(field, message)| ImportIssue {
            row,
            at: at.cloned(),
            field,
            message,
        })
        .collect()
}

/// The columns a workbook of several sheets gains for where each row came
/// from: its sheet, and its row there.
fn sheet_columns(headers: &[String]) -> Option<(usize, usize)> {
    let row = headers
        .iter()
        .rposition(|h| h == core_engines::workbook::SHEET_ROW_COLUMN)?;
    Some((row.checked_sub(1)?, row))
}

/// Where a row is in its workbook: `Europe, row 5`.
fn place_of(columns: Option<(usize, usize)>, row: &[String]) -> Option<String> {
    let (sheet, number) = columns?;
    Some(format!("{}, row {}", row.get(sheet)?, row.get(number)?))
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
        Record::Organisation(n, person) => json!({
            "name": n.name,
            "org_type": n.org_type,
            "addresses": n.addresses,
            "tax_id": n.tax_id,
            "default_currency": n.default_currency,
            "payment_terms_days": n.payment_terms_days,
            "region": n.region,
            "relationship": n.relationship,
            "status": n.status,
            "follow_up_on": n.follow_up_on,
            "do_not_contact": n.do_not_contact,
            "buys_from_others": n.buys_from_others,
            "interested_in": n.interested_in.iter().map(|i| i.id).collect::<Vec<_>>(),
            "potential_matches": n.potential_matches.iter().map(|i| i.id).collect::<Vec<_>>(),
            "contact": person.as_ref().map(|p| json!({
                "name": p.name,
                "emails": p.emails,
                "phones": p.phones,
                "role": p.role,
                "links": p.links,
            })),
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
            "links": n.links,
            "tags": n.tags,
            "custom": n.custom,
        }),
    }
}

/// What running an import takes: writing its records; for contacts that
/// may create the organisations they name, writing organisations; for
/// organisations with a contact person, writing contacts.
fn permissions_needed(
    entity: &str,
    options: &ImportOptions,
    columns: &[Option<String>],
) -> ApiResult<std::collections::BTreeSet<Permission>> {
    let mut needed = std::collections::BTreeSet::from([write_permission(entity)?]);
    if entity == contacts::ENTITY && options.create_organisations {
        needed.insert(Permission::OrganisationWrite);
    }
    if entity == organisations::ENTITY && has_person(columns) {
        needed.insert(Permission::ContactWrite);
    }
    Ok(needed)
}

/// Whether an organisation import names a person at each place.
fn has_person(columns: &[Option<String>]) -> bool {
    columns.iter().flatten().any(|c| c.starts_with("contact."))
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
    for p in permissions_needed(&import.entity, &plan.options, &plan.columns)? {
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
    let sheet_columns = sheet_columns(&table.headers);
    let mut out = DryRunResult {
        create: 0,
        update: 0,
        skip: 0,
        fail: 0,
        new_organisations: Vec::new(),
        issues: Vec::new(),
        sample: Vec::new(),
        problem_rows: Vec::new(),
        new_contacts: 0,
        to_check: Vec::new(),
        to_check_total: 0,
        readings: Vec::new(),
    };
    let mut reading_at: HashMap<(String, String), usize> = HashMap::new();
    // Values that could not be read have room of their own, so plenty of
    // ones that could never crowd them out.
    let (mut read_kept, mut unread_kept) = (0, 0);
    for (i, row) in table.rows.iter().enumerate() {
        let number = table.row_numbers[i];
        let Some(row) = setup.row(number, row) else {
            out.skip += 1;
            continue;
        };
        let (plan, notes, reads) = setup.plan(&reader, &index, &row);
        for r in reads {
            match reading_at.get(&(r.field.clone(), r.text.clone())) {
                Some(&k) => out.readings[k].rows += 1,
                None if (if r.read_as.is_some() {
                    read_kept
                } else {
                    unread_kept
                }) < MAX_READINGS =>
                {
                    if r.read_as.is_some() {
                        read_kept += 1;
                    } else {
                        unread_kept += 1;
                    }
                    reading_at.insert((r.field.clone(), r.text.clone()), out.readings.len());
                    out.readings.push(ValueReading {
                        field: r.field,
                        text: r.text,
                        read_as: r.read_as,
                        hint: r.hint,
                        rows: 1,
                    });
                }
                None => {}
            }
        }
        let at = place_of(sheet_columns, &row);
        // Notes are about what the row would write: all of them for a new
        // or updated record, only its person's for one left alone.
        let notes: Vec<RowNote> = match &plan {
            Plan::Create(_) | Plan::Update(..) => notes,
            Plan::Skip(Some(_)) => notes
                .into_iter()
                .filter(|(f, _)| f.as_deref().is_some_and(|f| f.starts_with("contact.")))
                .collect(),
            Plan::Skip(None) | Plan::Fail(_) => Vec::new(),
        };
        if !notes.is_empty() {
            out.to_check_total += 1;
            if out.to_check.len() < MAX_PROBLEM_ROWS {
                out.to_check.push(ProblemRow {
                    row: number,
                    at: at.clone(),
                    cells: row.to_vec(),
                    issues: issues_for(number, at.as_ref(), notes),
                });
            }
        }
        match plan {
            Plan::Create(record) | Plan::Update(_, record, _) => {
                if out.sample.len() < SAMPLE_ROWS {
                    out.sample.push(record_json(&record));
                }
                count(&mut out, &mut index, &setup, &record);
            }
            Plan::Skip(Some((org, person))) => {
                if count_person(&mut out, &mut index, &setup, org, &person) {
                    out.update += 1;
                } else {
                    out.skip += 1;
                }
            }
            Plan::Skip(None) => out.skip += 1,
            Plan::Fail(list) => {
                out.fail += 1;
                let issues = issues_for(number, at.as_ref(), list);
                if out.issues.len() < MAX_DRY_RUN_ISSUES {
                    out.issues.extend(issues.iter().cloned());
                    out.issues.truncate(MAX_DRY_RUN_ISSUES);
                }
                if out.problem_rows.len() < MAX_PROBLEM_ROWS {
                    out.problem_rows.push(ProblemRow {
                        row: number,
                        at,
                        cells: row.into_owned(),
                        issues,
                    });
                }
            }
        }
    }
    Ok(out)
}

/// Counts a planned create or update, as the run would see it: a later row
/// matching this one finds it.
fn count(out: &mut DryRunResult, index: &mut Index, setup: &Setup, record: &Record) {
    // The organisation a contact names first, so the contact is known at it.
    if let Record::Contact(_, Some(org)) = record {
        let key = name_key(org);
        if let std::collections::hash_map::Entry::Vacant(slot) = index.organisations.entry(key) {
            slot.insert(core_domain::ids::new_id());
            if out.new_organisations.len() < 50 {
                out.new_organisations.push(org.clone());
            }
        }
    }
    let id = match index.find(record) {
        Some(id) if setup.options.on_match == OnMatch::Update => {
            out.update += 1;
            id
        }
        _ => {
            out.create += 1;
            let id = core_domain::ids::new_id();
            index.add(record, id);
            id
        }
    };
    if let Record::Organisation(_, Some(person)) = record {
        count_person(out, index, setup, id, person);
    }
}

/// Counts the person a row names at an organisation; whether it changes
/// anything.
fn count_person(
    out: &mut DryRunResult,
    index: &mut Index,
    setup: &Setup,
    org: Uuid,
    person: &NewContact,
) -> bool {
    if index.find_person(person, org).is_some() {
        return setup.options.on_match == OnMatch::Update;
    }
    out.new_contacts += 1;
    index.add_contact(person, Some(org), core_domain::ids::new_id());
    true
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
    columns: &[Option<String>],
) -> ApiResult<Option<Ctx>> {
    let mut permissions = permissions_needed(entity, options, columns)?;
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

/// Lists in the row added to the record's own, unless that makes more than
/// it may hold.
fn merged_upto(current: &[String], extra: &[String], max: usize) -> Vec<String> {
    let out = merged(current, extra);
    if out.len() > max {
        current.to_vec()
    } else {
        out
    }
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
    on_match: OnMatch,
    extra: &Value,
) -> ApiResult<Result<&'static str, Vec<RowNote>>> {
    let outcome = match plan {
        Plan::Skip(None) => return Ok(Ok("skipped")),
        Plan::Fail(issues) => return Ok(Err(issues)),
        // The organisation is left alone; gaining its person updates it.
        Plan::Skip(Some((org, person))) => {
            add_person(tx, ctx, index, org, &person, on_match, extra)
                .await
                .map(|added| if added { "updated" } else { "skipped" })
        }
        Plan::Create(record) => create(tx, ctx, index, record, on_match, extra)
            .await
            .map(|()| "created"),
        Plan::Update(id, record, present) => {
            update(tx, ctx, index, id, record, &present, on_match, extra)
                .await
                .map(|()| "updated")
        }
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

/// Adds the person a row names to their organisation, unless they are
/// there already (by email, number, or name there), in which case they are
/// updated or left as chosen. Whether anything changed.
async fn add_person(
    tx: &mut Tx,
    ctx: &Ctx,
    index: &mut Index,
    org: Uuid,
    person: &NewContact,
    on_match: OnMatch,
    extra: &Value,
) -> ApiResult<bool> {
    let id = match index.find_person(person, org) {
        Some(_) if on_match != OnMatch::Update => return Ok(false),
        Some(id) => {
            ctx.authorize(Permission::ContactWrite)?;
            let current = contacts::load(tx, id).await?.ok_or(ApiError::NotFound)?;
            let emails: Vec<String> = person.emails.iter().map(|e| email_key(e)).collect();
            // Lists that would grow past what a contact holds stay as they
            // are, so the organisation written before is never left without
            // its row.
            let patch = ContactPatch {
                // Named at last, when they went by their email or number.
                name: (is_stand_in(&current.name) && !is_stand_in(&person.name))
                    .then(|| person.name.clone()),
                organisation_id: current.organisation_id.is_none().then_some(Some(org)),
                emails: Some(merged_upto(&current.emails, &emails, contacts::MAX_EMAILS)),
                phones: Some(merged_upto(
                    &current.phones,
                    &person.phones,
                    contacts::MAX_PHONES,
                )),
                links: Some(merged_upto(
                    &current.links,
                    &person.links,
                    contacts::MAX_LINKS,
                )),
                role: person.role.clone().map(Some),
                tags: Some(merged_upto(&current.tags, &person.tags, crm::MAX_TAGS)),
                ..Default::default()
            };
            contacts::update_in_tx(tx, ctx, id, patch, None, Some(extra.clone())).await?;
            id
        }
        None => {
            ctx.authorize(Permission::ContactWrite)?;
            let new = NewContact {
                organisation_id: Some(org),
                ..person.clone()
            };
            contacts::create_in_tx(tx, ctx, new, Some(extra.clone()))
                .await?
                .id
        }
    };
    index.add_contact(person, Some(org), id);
    Ok(true)
}

async fn create(
    tx: &mut Tx,
    ctx: &Ctx,
    index: &mut Index,
    record: Record,
    on_match: OnMatch,
    extra: &Value,
) -> ApiResult<()> {
    let id = match &record {
        Record::Talent(n) => {
            talent::create_in_tx(tx, ctx, n.clone(), Some(extra.clone()))
                .await?
                .id
        }
        Record::Organisation(n, person) => {
            // Checked first, so an organisation is not left without its person.
            if person.is_some() {
                ctx.authorize(Permission::ContactWrite)?;
            }
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
    if let Record::Organisation(_, Some(person)) = &record {
        add_person(tx, ctx, index, id, person, on_match, extra).await?;
    }
    Ok(())
}

/// The talent or projects on a list, as they are given.
fn item_inputs(refs: &[organisations::ItemRef]) -> Vec<ItemInput> {
    refs.iter()
        .map(|r| ItemInput {
            kind: r.kind,
            id: r.id,
        })
        .collect()
}

/// Items in the row are added to the record's list.
fn merged_items(current: &[organisations::ItemRef], extra: &[ItemInput]) -> Vec<ItemInput> {
    let mut out = item_inputs(current);
    for e in extra {
        if !out.contains(e) {
            out.push(*e);
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
async fn update(
    tx: &mut Tx,
    ctx: &Ctx,
    index: &mut Index,
    id: Uuid,
    record: Record,
    present: &HashSet<String>,
    on_match: OnMatch,
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
        Record::Organisation(n, person) => {
            if person.is_some() {
                ctx.authorize(Permission::ContactWrite)?;
            }
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
                // A region it is in anyway, by its country, is not pinned.
                region: given(present, "region", &n.region)
                    .filter(|r| current.region_picked || *r != current.region),
                relationship: given(present, "relationship", &n.relationship),
                status: given(present, "status", &n.status),
                follow_up_on: given(present, "follow_up_on", &n.follow_up_on),
                do_not_contact: has("do_not_contact").then_some(n.do_not_contact),
                buys_from_others: has("buys_from_others")
                    .then(|| merged(&current.buys_from_others, &n.buys_from_others)),
                interested_in: has("interested_in")
                    .then(|| merged_items(&current.interested_in, &n.interested_in)),
                potential_matches: has("potential_matches")
                    .then(|| merged_items(&current.potential_matches, &n.potential_matches)),
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
                links: has("links").then(|| merged(&current.links, &n.links)),
                notes: given(present, "notes", &n.notes),
                tags: Some(merged(&current.tags, &n.tags)),
                custom: custom(&n.custom),
            };
            contacts::update_in_tx(tx, ctx, id, patch, None, Some(extra.clone())).await?;
        }
    }
    index.add(&record, id);
    if let Record::Organisation(_, Some(person)) = &record {
        add_person(tx, ctx, index, id, person, on_match, extra).await?;
    }
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
        &setup.columns,
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
    let sheet_columns = sheet_columns(&table.headers);
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
            let Some(row) = setup.row(table.row_numbers[i], row) else {
                counts.skipped += 1;
                continue;
            };
            let (plan, _, _) = setup.plan(&reader, &index, &row);
            match apply(
                &mut tx,
                &ctx,
                &mut index,
                plan,
                setup.options.on_match,
                &extra,
            )
            .await?
            {
                Ok("created") => counts.created += 1,
                Ok("updated") => counts.updated += 1,
                Ok(_) => counts.skipped += 1,
                Err(list) => {
                    counts.failed += 1;
                    if stored < MAX_STORED_ISSUES {
                        let at = place_of(sheet_columns, &row);
                        let found = issues_for(table.row_numbers[i], at.as_ref(), list);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_are_read_for_what_to_check() {
        assert_eq!(scan_notes("DNC - asked to be removed"), (true, None));
        assert_eq!(scan_notes("Don\u{2019}t contact again"), (true, None));
        assert_eq!(
            scan_notes("Closed for refurb till May"),
            (false, Some("closed"))
        );
        // Whole words only.
        assert_eq!(
            scan_notes("Enclosed: the rider; checkout at 11"),
            (false, None)
        );
        assert_eq!(scan_notes("loves jazz"), (false, None));
    }

    #[test]
    fn statuses_and_regions_are_found_however_a_sheet_puts_them() {
        let s = PitchingSettings::default();
        let status = |raw: &str| status_named(&s, raw).map(|x| x.key.as_str());
        assert_eq!(status("talking"), Some("talking"));
        assert_eq!(status("Booked"), Some("booked"));
        assert_eq!(status("Booked or worked with"), Some("booked"));
        assert_eq!(status("Orange"), Some("talking"));
        assert_eq!(status("gray"), Some("inactive"));
        assert_eq!(status("red"), None);
        let region = |raw: &str| region_named(&s, raw).map(|x| x.key.as_str());
        assert_eq!(region("Europe"), Some("europe"));
        assert_eq!(
            region("north central america"),
            Some("north_central_america")
        );
        assert_eq!(region("UK & Ireland"), None);
        assert_eq!(relationship_named("Lead"), Some(Relationship::Prospect));
        assert_eq!(relationship_named("existing"), Some(Relationship::Client));
        assert_eq!(relationship_named("maybe"), None);
    }

    #[test]
    fn people_sharing_a_venues_address_are_told_apart() {
        let grand = Uuid::from_u128(1);
        let mut index = Index::default();
        let person = |name: &str, email: &str| NewContact {
            name: name.into(),
            emails: vec![email.into()],
            ..Default::default()
        };
        index.add_contact(
            &person("Alice", "bookings@grand.test"),
            Some(grand),
            Uuid::from_u128(2),
        );
        index.add_contact(
            &person("owl@bar.test", "owl@bar.test"),
            None,
            Uuid::from_u128(3),
        );
        let found = |p: NewContact| index.find_person(&p, grand).map(|id| id.as_u128());
        assert_eq!(found(person("Bob", "bookings@grand.test")), None);
        assert_eq!(found(person("alice", "bookings@grand.test")), Some(2));
        assert_eq!(found(person("Alice", "alice@home.test")), Some(2));
        // A stand-in name on either side matches by address.
        assert_eq!(
            found(person("bookings@grand.test", "bookings@grand.test")),
            Some(2)
        );
        assert_eq!(found(person("Jane Owl", "owl@bar.test")), Some(3));
    }

    #[test]
    fn links_split_on_spaces_and_separators() {
        let cells = vec![
            "https://a.test  @owl; https://b.test",
            "https://a.test|x.test",
        ];
        assert_eq!(
            split_links(Some(&cells)),
            ["https://a.test", "@owl", "https://b.test", "x.test"]
        );
        assert!(split_links(None).is_empty());
    }
}
