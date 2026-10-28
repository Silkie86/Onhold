//! Exports from the apps people move to OnHold from, recognised by their
//! columns, so a switch needs no column matching by hand. Each source says
//! what its file holds and where its columns go, including the columns to
//! leave out that a general guess would get wrong (Outlook's `Title` is
//! Mr or Ms, not a job title). Columns a source does not list are guessed
//! as for any spreadsheet.

use crate::import::{normalize_header, unnumbered_header};

/// A known export.
#[derive(Debug)]
pub struct Source {
    pub key: &'static str,
    /// What to call it on screen.
    pub label: &'static str,
    /// What its rows are: `contact`, `organisation` or `talent`.
    pub entity: &'static str,
    /// Headers that must all be there, numbering ignored.
    signature: &'static [&'static str],
    /// Where its columns go, numbering ignored (`E-mail - Value` covers
    /// `E-mail 1 - Value`, `E-mail 2 - Value`...); `None` leaves one out.
    columns: &'static [(&'static str, Option<&'static str>)],
}

const NAME: Option<&str> = Some("name");
const EMAILS: Option<&str> = Some("emails");
const PHONES: Option<&str> = Some("phones");
const ORGANISATION: Option<&str> = Some("organisation");
const ROLE: Option<&str> = Some("role");
const NOTES: Option<&str> = Some("notes");
const TAGS: Option<&str> = Some("tags");
const SKIP: Option<&str> = None;

/// Google Contacts, as exported since 2023.
const GOOGLE: &[(&str, Option<&str>)] = &[
    ("First Name", NAME),
    ("Middle Name", NAME),
    ("Last Name", NAME),
    ("Name Prefix", SKIP),
    ("Name Suffix", SKIP),
    ("Nickname", SKIP),
    ("File As", SKIP),
    ("Phonetic First Name", SKIP),
    ("Phonetic Middle Name", SKIP),
    ("Phonetic Last Name", SKIP),
    ("Organization Name", ORGANISATION),
    ("Organization Title", ROLE),
    ("Organization Department", SKIP),
    ("Notes", NOTES),
    ("Labels", TAGS),
    ("E-mail - Label", SKIP),
    ("E-mail - Value", EMAILS),
    ("Phone - Label", SKIP),
    ("Phone - Value", PHONES),
];

/// Google Contacts' older "Google CSV".
const GOOGLE_LEGACY: &[(&str, Option<&str>)] = &[
    ("Name", NAME),
    ("Given Name", SKIP),
    ("Additional Name", SKIP),
    ("Family Name", SKIP),
    ("Yomi Name", SKIP),
    ("Name Prefix", SKIP),
    ("Name Suffix", SKIP),
    ("Nickname", SKIP),
    ("Short Name", SKIP),
    ("Maiden Name", SKIP),
    ("Organization - Name", ORGANISATION),
    ("Organization - Title", ROLE),
    ("Organization - Type", SKIP),
    ("Notes", NOTES),
    ("Group Membership", TAGS),
    ("E-mail - Type", SKIP),
    ("E-mail - Value", EMAILS),
    ("Phone - Type", SKIP),
    ("Phone - Value", PHONES),
];

/// Outlook (desktop and Outlook.com) and Microsoft 365.
const OUTLOOK: &[(&str, Option<&str>)] = &[
    ("Title", SKIP),
    ("First Name", NAME),
    ("Middle Name", NAME),
    ("Last Name", NAME),
    ("Suffix", SKIP),
    ("Nickname", SKIP),
    ("Company", ORGANISATION),
    ("Department", SKIP),
    ("Job Title", ROLE),
    ("E-mail Address", EMAILS),
    ("E-mail Display Name", SKIP),
    ("E-mail Type", SKIP),
    ("Mobile Phone", PHONES),
    ("Business Phone", PHONES),
    ("Home Phone", PHONES),
    ("Primary Phone", PHONES),
    ("Other Phone", PHONES),
    ("Company Main Phone", PHONES),
    ("Business Fax", SKIP),
    ("Home Fax", SKIP),
    ("Other Fax", SKIP),
    ("Notes", NOTES),
    ("Categories", TAGS),
];

const HUBSPOT_CONTACTS: &[(&str, Option<&str>)] = &[
    ("Record ID", SKIP),
    ("First Name", NAME),
    ("Last Name", NAME),
    ("Email", EMAILS),
    ("Additional email addresses", EMAILS),
    ("Phone Number", PHONES),
    ("Mobile Phone Number", PHONES),
    ("Company Name", ORGANISATION),
    ("Associated Company", ORGANISATION),
    ("Job Title", ROLE),
    ("Contact owner", SKIP),
    ("Lead Status", SKIP),
    ("Lifecycle Stage", SKIP),
];

const HUBSPOT_COMPANIES: &[(&str, Option<&str>)] = &[
    ("Record ID", SKIP),
    ("Company name", NAME),
    ("Company Domain Name", SKIP),
    ("Street Address", Some("address.line1")),
    ("Street Address", Some("address.line2")),
    ("City", Some("address.city")),
    ("State/Region", Some("address.region")),
    ("Postal Code", Some("address.postcode")),
    ("Country/Region", Some("address.country")),
    ("Industry", Some("org_type")),
    ("Type", Some("org_type")),
    ("Company owner", SKIP),
    ("Description", NOTES),
];

const SALESFORCE_CONTACTS: &[(&str, Option<&str>)] = &[
    ("Salutation", SKIP),
    ("First Name", NAME),
    ("Last Name", NAME),
    ("Title", ROLE),
    ("Account Name", ORGANISATION),
    ("Email", EMAILS),
    ("Phone", PHONES),
    ("Mobile", PHONES),
    ("Home Phone", PHONES),
    ("Other Phone", PHONES),
    ("Fax", SKIP),
    ("Contact Owner", SKIP),
    ("Description", NOTES),
];

const SALESFORCE_ACCOUNTS: &[(&str, Option<&str>)] = &[
    ("Account Name", NAME),
    ("Type", Some("org_type")),
    ("Billing Street", Some("address.line1")),
    ("Billing City", Some("address.city")),
    ("Billing State/Province", Some("address.region")),
    ("Billing Zip/Postal Code", Some("address.postcode")),
    ("Billing Country", Some("address.country")),
    ("Shipping Street", SKIP),
    ("Shipping City", SKIP),
    ("Shipping State/Province", SKIP),
    ("Shipping Zip/Postal Code", SKIP),
    ("Shipping Country", SKIP),
    ("Account Owner", SKIP),
    ("Description", NOTES),
];

const PIPEDRIVE_PEOPLE: &[(&str, Option<&str>)] = &[
    ("Person - Name", NAME),
    ("Person - First name", SKIP),
    ("Person - Last name", SKIP),
    ("Person - Email - Work", EMAILS),
    ("Person - Email - Home", EMAILS),
    ("Person - Email - Other", EMAILS),
    ("Person - Phone - Work", PHONES),
    ("Person - Phone - Home", PHONES),
    ("Person - Phone - Mobile", PHONES),
    ("Person - Phone - Other", PHONES),
    ("Person - Organization", ORGANISATION),
    ("Person - Job title", ROLE),
    ("Person - Labels", TAGS),
    ("Person - Label", TAGS),
    ("Person - Owner", SKIP),
];

const PIPEDRIVE_ORGANISATIONS: &[(&str, Option<&str>)] = &[
    ("Organization - Name", NAME),
    ("Organization - Address", Some("address.line1")),
    ("Organization - Labels", TAGS),
    ("Organization - Label", TAGS),
    ("Organization - Owner", SKIP),
];

const MAILCHIMP: &[(&str, Option<&str>)] = &[
    ("Email Address", EMAILS),
    ("First Name", NAME),
    ("Last Name", NAME),
    ("Phone Number", PHONES),
    ("Address", SKIP),
    ("Birthday", SKIP),
    ("TAGS", TAGS),
];

const LINKEDIN: &[(&str, Option<&str>)] = &[
    ("First Name", NAME),
    ("Last Name", NAME),
    ("URL", SKIP),
    ("Email Address", EMAILS),
    ("Company", ORGANISATION),
    ("Position", ROLE),
    ("Connected On", SKIP),
];

/// The exports recognised, most particular first.
pub const SOURCES: &[Source] = &[
    Source {
        key: "google",
        label: "Google Contacts",
        entity: "contact",
        signature: &[
            "First Name",
            "Last Name",
            "Organization Name",
            "E-mail - Value",
        ],
        columns: GOOGLE,
    },
    Source {
        key: "google",
        label: "Google Contacts",
        entity: "contact",
        signature: &["Name", "Given Name", "Family Name", "Group Membership"],
        columns: GOOGLE_LEGACY,
    },
    Source {
        key: "outlook",
        label: "Outlook",
        entity: "contact",
        signature: &[
            "First Name",
            "Last Name",
            "E-mail Address",
            "Business Phone",
        ],
        columns: OUTLOOK,
    },
    Source {
        key: "hubspot",
        label: "HubSpot",
        entity: "organisation",
        signature: &["Record ID", "Company name", "Company Domain Name"],
        columns: HUBSPOT_COMPANIES,
    },
    Source {
        key: "hubspot",
        label: "HubSpot",
        entity: "contact",
        signature: &["Record ID", "First Name", "Last Name", "Email"],
        columns: HUBSPOT_CONTACTS,
    },
    Source {
        key: "salesforce",
        label: "Salesforce",
        entity: "organisation",
        signature: &["Account Name", "Billing Street", "Billing City"],
        columns: SALESFORCE_ACCOUNTS,
    },
    Source {
        key: "salesforce",
        label: "Salesforce",
        entity: "contact",
        signature: &["First Name", "Last Name", "Account Name", "Email"],
        columns: SALESFORCE_CONTACTS,
    },
    Source {
        key: "pipedrive",
        label: "Pipedrive",
        entity: "contact",
        signature: &["Person - Name"],
        columns: PIPEDRIVE_PEOPLE,
    },
    Source {
        key: "pipedrive",
        label: "Pipedrive",
        entity: "organisation",
        signature: &["Organization - Name"],
        columns: PIPEDRIVE_ORGANISATIONS,
    },
    Source {
        key: "mailchimp",
        label: "Mailchimp",
        entity: "contact",
        signature: &["Email Address", "First Name", "Last Name", "MEMBER_RATING"],
        columns: MAILCHIMP,
    },
    Source {
        key: "linkedin",
        label: "LinkedIn connections",
        entity: "contact",
        signature: &["First Name", "Last Name", "Connected On"],
        columns: LINKEDIN,
    },
];

/// Contact cards from Apple Contacts, iCloud, Android or Outlook: read by
/// [`crate::vcard`], whose columns the general guess maps.
pub const VCARD: Source = Source {
    key: "vcard",
    label: "Contact cards (Apple, iCloud, Android, Outlook)",
    entity: "contact",
    signature: &[],
    columns: &[],
};

/// The export these headers come from, if it is one we know.
pub fn detect(headers: &[String]) -> Option<&'static Source> {
    let have: Vec<String> = headers.iter().map(|h| unnumbered_header(h)).collect();
    SOURCES.iter().find(|s| {
        s.signature
            .iter()
            .all(|sig| have.contains(&unnumbered_header(sig)))
    })
}

impl Source {
    /// For each header: `Some(target)` where this source says the column
    /// goes (`Some(None)` to leave it out), `None` where it does not say.
    /// Where it lists a header twice (HubSpot's two `Street Address`
    /// columns), the second column takes the second target.
    pub fn columns_for(&self, headers: &[String]) -> Vec<Option<Option<String>>> {
        let mut used = vec![false; self.columns.len()];
        headers
            .iter()
            .map(|h| {
                let exact = normalize_header(h);
                let bare = unnumbered_header(h);
                let unused = |i: &usize| !used[*i];
                let entries = || self.columns.iter().enumerate();
                let found = entries()
                    .filter(|(i, _)| unused(i))
                    .find(|(_, (name, _))| normalize_header(name) == exact)
                    .or_else(|| {
                        entries()
                            .filter(|(i, _)| unused(i))
                            .find(|(_, (name, _))| unnumbered_header(name) == bare)
                    })
                    .or_else(|| entries().find(|(_, (name, _))| unnumbered_header(name) == bare))?;
                used[found.0] = true;
                Some(found.1.1.map(str::to_owned))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(s: &str) -> Vec<String> {
        s.split(',').map(str::to_owned).collect()
    }

    #[test]
    fn exports_are_recognised_by_their_columns() {
        let google = headers(
            "First Name,Middle Name,Last Name,Organization Name,Organization Title,Labels,E-mail 1 - Label,E-mail 1 - Value,E-mail 2 - Label,E-mail 2 - Value,Phone 1 - Label,Phone 1 - Value",
        );
        let s = detect(&google).expect("google");
        assert_eq!((s.key, s.entity), ("google", "contact"));
        let cols = s.columns_for(&google);
        let get = |i: usize| cols[i].clone().flatten();
        assert_eq!(get(0).as_deref(), Some("name"));
        assert_eq!(get(2).as_deref(), Some("name"));
        assert_eq!(get(4).as_deref(), Some("role"));
        assert_eq!(cols[6], Some(None));
        assert_eq!(get(7).as_deref(), Some("emails"));
        assert_eq!(get(9).as_deref(), Some("emails"));
        assert_eq!(get(11).as_deref(), Some("phones"));

        let outlook = headers(
            "Title,First Name,Middle Name,Last Name,Company,Job Title,Business Phone,Mobile Phone,E-mail Address,E-mail 2 Address,Categories",
        );
        let s = detect(&outlook).expect("outlook");
        assert_eq!(s.key, "outlook");
        let cols = s.columns_for(&outlook);
        // Outlook's Title is Mr or Ms, so it is left out, not a role.
        assert_eq!(cols[0], Some(None));
        assert_eq!(cols[9], Some(Some("emails".into())));

        let hubspot = headers(
            "Record ID,Company name,Company Domain Name,Street Address,Street Address 2,City",
        );
        let s = detect(&hubspot).expect("hubspot companies");
        assert_eq!((s.key, s.entity), ("hubspot", "organisation"));
        let cols = s.columns_for(&hubspot);
        assert_eq!(cols[3], Some(Some("address.line1".into())));
        assert_eq!(cols[4], Some(Some("address.line2".into())));

        let linkedin =
            headers("First Name,Last Name,URL,Email Address,Company,Position,Connected On");
        assert_eq!(detect(&linkedin).map(|s| s.key), Some("linkedin"));

        assert!(detect(&headers("Name,Email,Phone")).is_none());
    }
}
