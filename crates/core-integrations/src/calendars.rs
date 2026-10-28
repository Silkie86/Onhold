//! When a person is busy, read from their own calendar without seeing what
//! they are doing: Google's free/busy query, the "show as" of Microsoft
//! calendar events (asking for nothing else), or a private iCal link
//! (Apple, Proton, Fastmail, or any calendar that publishes one). Only the
//! times are kept, merged into stretches.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use serde::Deserialize;
use serde_json::json;
use url::Url;

use crate::provider::{self, ProviderError, endpoint};

/// Google's APIs (the free/busy query and the account's address).
pub const GOOGLE_API: &str = "https://www.googleapis.com";

/// Seeing when a person's Google calendars are busy, and which account it is.
pub const GOOGLE_BUSY_SCOPES: &[&str] = &[
    "openid",
    "email",
    "https://www.googleapis.com/auth/calendar.freebusy",
];
/// Microsoft's least access to calendar events: no bodies, attendees or
/// attachments. OnHold asks only for the times and "show as".
pub const MICROSOFT_BUSY_SCOPES: &[&str] = &["offline_access", "User.Read", "Calendars.ReadBasic"];

/// Google answers one free/busy query for a limited stretch of time.
const GOOGLE_WINDOW_DAYS: i64 = 60;
/// Pages of Microsoft events read at most, of up to 500 each.
const GRAPH_PAGES: usize = 20;
/// The largest iCal file read, and the most events taken from it.
pub const ICAL_MAX_BYTES: usize = 8 * 1024 * 1024;
const ICAL_MAX_EVENTS: usize = 20_000;
/// Occurrences of one repeating event taken at most.
const MAX_OCCURRENCES: u16 = 2_000;
/// Stretches kept at most per calendar.
pub const MAX_BLOCKS: usize = 3_000;

/// A stretch of time the person is busy, or only perhaps busy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Busy {
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub tentative: bool,
}

/// Sorts stretches and joins those that overlap or touch, busy and
/// tentative separately; tentative time inside busy time is dropped.
pub fn merge(mut blocks: Vec<Busy>) -> Vec<Busy> {
    blocks.retain(|b| b.ends_at > b.starts_at);
    let join = |mut list: Vec<Busy>| {
        list.sort();
        let mut out: Vec<Busy> = Vec::with_capacity(list.len());
        for b in list {
            match out.last_mut() {
                Some(last) if b.starts_at <= last.ends_at => {
                    last.ends_at = last.ends_at.max(b.ends_at);
                }
                _ => out.push(b),
            }
        }
        out
    };
    let (tentative, busy): (Vec<Busy>, Vec<Busy>) = blocks.into_iter().partition(|b| b.tentative);
    let busy = join(busy);
    let tentative = join(tentative)
        .into_iter()
        .filter(|t| {
            !busy
                .iter()
                .any(|b| b.starts_at <= t.starts_at && b.ends_at >= t.ends_at)
        })
        .collect::<Vec<_>>();
    let mut all = busy;
    all.extend(tentative);
    all.sort();
    all.truncate(MAX_BLOCKS);
    all
}

fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

// Google ------------------------------------------------------------------------------

pub struct GoogleBusy {
    http: reqwest::Client,
    base: Url,
    token: String,
}

impl GoogleBusy {
    pub fn new(http: reqwest::Client, base: Url, access_token: String) -> Self {
        Self {
            http,
            base,
            token: access_token,
        }
    }

    /// The account's address.
    pub async fn email(&self) -> Result<String, ProviderError> {
        #[derive(Deserialize)]
        struct Info {
            email: Option<String>,
        }
        let info: Info = provider::send_json(
            self.http
                .get(endpoint(&self.base, "/oauth2/v3/userinfo"))
                .bearer_auth(&self.token),
        )
        .await?;
        info.email
            .filter(|e| e.contains('@'))
            .ok_or_else(|| ProviderError::Invalid("the account has no address".into()))
    }

    /// Busy times on the person's main calendar between `from` and `to`.
    pub async fn busy(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Busy>, ProviderError> {
        #[derive(Deserialize)]
        struct Range {
            start: DateTime<Utc>,
            end: DateTime<Utc>,
        }
        #[derive(Deserialize)]
        struct Calendar {
            #[serde(default)]
            busy: Vec<Range>,
            #[serde(default)]
            errors: Vec<serde_json::Value>,
        }
        #[derive(Deserialize)]
        struct Answer {
            #[serde(default)]
            calendars: HashMap<String, Calendar>,
        }
        let mut out = Vec::new();
        let mut start = from;
        while start < to {
            let end = (start + Duration::days(GOOGLE_WINDOW_DAYS)).min(to);
            let answer: Answer = provider::send_json(
                self.http
                    .post(endpoint(&self.base, "/calendar/v3/freeBusy"))
                    .bearer_auth(&self.token)
                    .json(&json!({
                        "timeMin": rfc3339(start),
                        "timeMax": rfc3339(end),
                        "items": [{ "id": "primary" }],
                    })),
            )
            .await?;
            let cal = answer
                .calendars
                .get("primary")
                .ok_or_else(|| ProviderError::Invalid("no answer for the calendar".into()))?;
            if let Some(e) = cal.errors.first() {
                let reason = e
                    .get("reason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("unknown");
                return Err(ProviderError::Refused {
                    status: 200,
                    message: format!("Google could not read the calendar ({reason})"),
                });
            }
            out.extend(cal.busy.iter().map(|r| Busy {
                starts_at: r.start,
                ends_at: r.end,
                tentative: false,
            }));
            start = end;
        }
        Ok(merge(out))
    }
}

// Microsoft ---------------------------------------------------------------------------

pub struct GraphBusy {
    http: reqwest::Client,
    base: Url,
    token: String,
}

impl GraphBusy {
    pub fn new(http: reqwest::Client, base: Url, access_token: String) -> Self {
        Self {
            http,
            base,
            token: access_token,
        }
    }

    /// Busy and tentative times on the person's calendar between `from`
    /// and `to`, repeating events included. Free and working-elsewhere
    /// time, and cancelled events, are left out.
    pub async fn busy(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Busy>, ProviderError> {
        #[derive(Deserialize)]
        struct When {
            #[serde(rename = "dateTime")]
            date_time: String,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Event {
            start: When,
            end: When,
            show_as: Option<String>,
            #[serde(default)]
            is_cancelled: bool,
        }
        #[derive(Deserialize)]
        struct Page {
            #[serde(default)]
            value: Vec<Event>,
            #[serde(rename = "@odata.nextLink")]
            next: Option<String>,
        }
        // Times come back in UTC, without an offset.
        let utc = |s: &str| -> Result<DateTime<Utc>, ProviderError> {
            let trimmed = s.trim_end_matches('Z');
            let naive = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S%.f")
                .map_err(|e| ProviderError::Invalid(format!("a time Microsoft sent: {e}")))?;
            Ok(Utc.from_utc_datetime(&naive))
        };
        let mut url = endpoint(&self.base, "/v1.0/me/calendarView");
        url.query_pairs_mut()
            .append_pair("startDateTime", &rfc3339(from))
            .append_pair("endDateTime", &rfc3339(to))
            .append_pair("$select", "start,end,showAs,isCancelled")
            .append_pair("$top", "500");
        let mut out = Vec::new();
        for _ in 0..GRAPH_PAGES {
            let page: Page = provider::send_json(
                self.http
                    .get(url.clone())
                    .bearer_auth(&self.token)
                    .header("prefer", "outlook.timezone=\"UTC\""),
            )
            .await?;
            for e in page.value {
                let tentative = match e.show_as.as_deref().unwrap_or("busy") {
                    "free" | "workingElsewhere" => continue,
                    "tentative" => true,
                    _ => false,
                };
                if e.is_cancelled {
                    continue;
                }
                out.push(Busy {
                    starts_at: utc(&e.start.date_time)?,
                    ends_at: utc(&e.end.date_time)?,
                    tentative,
                });
            }
            match page.next {
                // Graph's own link, kept on the configured host so a stand-in works.
                Some(next) => {
                    let next =
                        Url::parse(&next).map_err(|e| ProviderError::Invalid(e.to_string()))?;
                    let mut same = url.clone();
                    same.set_query(next.query());
                    url = same;
                }
                None => break,
            }
        }
        Ok(merge(out))
    }
}

// iCal links ----------------------------------------------------------------------------

/// Makes a pasted calendar link fetchable: `webcal://` is https.
pub fn ical_url(raw: &str) -> Option<Url> {
    let raw = raw.trim();
    let raw = match raw.split_once("://") {
        Some((scheme, rest))
            if scheme.eq_ignore_ascii_case("webcal") || scheme.eq_ignore_ascii_case("webcals") =>
        {
            format!("https://{rest}")
        }
        _ => raw.to_owned(),
    };
    let url = Url::parse(&raw).ok()?;
    (matches!(url.scheme(), "https" | "http")
        && url.host_str().is_some()
        && url.username().is_empty())
    .then_some(url)
}

/// Fetches an iCal file with a client from [`crate::http::public_client`],
/// following a few redirects (iCloud sends people to their own server),
/// each checked again.
pub async fn fetch_ical(
    http: &reqwest::Client,
    url: &Url,
    allow_private: bool,
) -> Result<String, ProviderError> {
    let mut url = url.clone();
    for _ in 0..4 {
        if !allow_private && (url.scheme() != "https" || crate::http::names_private_address(&url)) {
            return Err(ProviderError::Refused {
                status: 400,
                message: "the calendar link must be a public https address".into(),
            });
        }
        let mut res = http
            .get(url.clone())
            .header("accept", "text/calendar, */*;q=0.5")
            .send()
            .await
            .map_err(|e| ProviderError::Unavailable(e.without_url().to_string()))?;
        let status = res.status().as_u16();
        if (300..400).contains(&status) {
            let next = res
                .headers()
                .get("location")
                .and_then(|l| l.to_str().ok())
                .and_then(|l| url.join(l).ok())
                .ok_or_else(|| ProviderError::Invalid("a redirect without a location".into()))?;
            url = next;
            continue;
        }
        match status {
            200..=299 => {}
            401 | 403 | 404 | 410 => {
                return Err(ProviderError::Revoked(format!(
                    "the calendar link no longer works (status {status})"
                )));
            }
            429 => return Err(ProviderError::RateLimited { retry_after: None }),
            500..=599 => return Err(ProviderError::Unavailable(format!("status {status}"))),
            _ => {
                return Err(ProviderError::Refused {
                    status,
                    message: format!("the calendar link answered with status {status}"),
                });
            }
        }
        let mut body = Vec::new();
        while let Some(chunk) = res
            .chunk()
            .await
            .map_err(|e| ProviderError::Unavailable(e.without_url().to_string()))?
        {
            if body.len() + chunk.len() > ICAL_MAX_BYTES {
                return Err(ProviderError::Refused {
                    status,
                    message: "the calendar is too large to read".into(),
                });
            }
            body.extend_from_slice(&chunk);
        }
        let text = String::from_utf8_lossy(&body).into_owned();
        if !text.trim_start().starts_with("BEGIN:VCALENDAR") {
            return Err(ProviderError::Refused {
                status,
                message: "the link is not an iCal calendar".into(),
            });
        }
        return Ok(text);
    }
    Err(ProviderError::Refused {
        status: 310,
        message: "the calendar link redirects too many times".into(),
    })
}

/// One content line: its name, parameters and value.
struct Line<'a> {
    name: String,
    params: Vec<(String, String)>,
    value: &'a str,
}

impl Line<'_> {
    fn param(&self, key: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

fn unfold(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        match raw.as_bytes().first() {
            Some(b' ' | b'\t') if !out.is_empty() => {
                if let Some(last) = out.last_mut() {
                    last.push_str(&raw[1..]);
                }
            }
            _ if raw.is_empty() => {}
            _ => out.push(raw.to_owned()),
        }
    }
    out
}

fn parse_line(line: &str) -> Option<Line<'_>> {
    // The value starts at the first colon outside quotes.
    let mut quoted = false;
    let mut split = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => {
                split = Some(i);
                break;
            }
            _ => {}
        }
    }
    let split = split?;
    let (head, value) = (&line[..split], &line[split + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.trim().to_ascii_uppercase();
    let params = parts
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| {
            (
                k.trim().to_ascii_uppercase(),
                v.trim().trim_matches('"').to_owned(),
            )
        })
        .collect();
    Some(Line {
        name,
        params,
        value,
    })
}

/// Windows zone names some Microsoft calendars use, for the zones most
/// OnHold users are in. Anything else falls back to the calendar's zone.
const WINDOWS_ZONES: &[(&str, &str)] = &[
    ("GMT Standard Time", "Europe/London"),
    ("Greenwich Standard Time", "Atlantic/Reykjavik"),
    ("W. Europe Standard Time", "Europe/Berlin"),
    ("Romance Standard Time", "Europe/Paris"),
    ("Central Europe Standard Time", "Europe/Budapest"),
    ("Central European Standard Time", "Europe/Warsaw"),
    ("E. Europe Standard Time", "Europe/Chisinau"),
    ("FLE Standard Time", "Europe/Kiev"),
    ("GTB Standard Time", "Europe/Bucharest"),
    ("Russian Standard Time", "Europe/Moscow"),
    ("Eastern Standard Time", "America/New_York"),
    ("Central Standard Time", "America/Chicago"),
    ("Mountain Standard Time", "America/Denver"),
    ("US Mountain Standard Time", "America/Phoenix"),
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("Atlantic Standard Time", "America/Halifax"),
    ("E. South America Standard Time", "America/Sao_Paulo"),
    ("Argentina Standard Time", "America/Buenos_Aires"),
    ("Mexico Standard Time", "America/Mexico_City"),
    ("Central Standard Time (Mexico)", "America/Mexico_City"),
    ("South Africa Standard Time", "Africa/Johannesburg"),
    ("W. Central Africa Standard Time", "Africa/Lagos"),
    ("E. Africa Standard Time", "Africa/Nairobi"),
    ("Egypt Standard Time", "Africa/Cairo"),
    ("Arabian Standard Time", "Asia/Dubai"),
    ("Israel Standard Time", "Asia/Jerusalem"),
    ("Turkey Standard Time", "Europe/Istanbul"),
    ("India Standard Time", "Asia/Kolkata"),
    ("SE Asia Standard Time", "Asia/Bangkok"),
    ("Singapore Standard Time", "Asia/Singapore"),
    ("China Standard Time", "Asia/Shanghai"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("Korea Standard Time", "Asia/Seoul"),
    ("AUS Eastern Standard Time", "Australia/Sydney"),
    ("E. Australia Standard Time", "Australia/Brisbane"),
    ("W. Australia Standard Time", "Australia/Perth"),
    ("New Zealand Standard Time", "Pacific/Auckland"),
    ("UTC", "UTC"),
    ("Coordinated Universal Time", "UTC"),
];

fn zone(name: &str) -> Option<chrono_tz::Tz> {
    let name = name.trim().trim_start_matches('/');
    if let Ok(tz) = name.parse::<chrono_tz::Tz>() {
        return Some(tz);
    }
    WINDOWS_ZONES
        .iter()
        .find(|(w, _)| w.eq_ignore_ascii_case(name))
        .and_then(|(_, iana)| iana.parse().ok())
}

/// A start, end or exception time as written.
#[derive(Debug, Clone, Copy)]
enum When {
    Date(NaiveDate),
    Utc(DateTime<Utc>),
    Local(NaiveDateTime, chrono_tz::Tz),
}

impl When {
    fn parse(value: &str, tzid: Option<&str>, default: chrono_tz::Tz) -> Option<Self> {
        let v = value.trim();
        if v.len() == 8 {
            return NaiveDate::parse_from_str(v, "%Y%m%d").ok().map(Self::Date);
        }
        if let Some(z) = v.strip_suffix('Z') {
            let naive = NaiveDateTime::parse_from_str(z, "%Y%m%dT%H%M%S").ok()?;
            return Some(Self::Utc(Utc.from_utc_datetime(&naive)));
        }
        let naive = NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok()?;
        Some(Self::Local(naive, tzid.and_then(zone).unwrap_or(default)))
    }

    /// The zone repeats are counted in, and the local start.
    fn local(self, default: chrono_tz::Tz) -> (chrono_tz::Tz, NaiveDateTime) {
        match self {
            Self::Date(d) => (default, d.and_hms_opt(0, 0, 0).unwrap_or_default()),
            Self::Utc(at) => (chrono_tz::UTC, at.naive_utc()),
            Self::Local(n, tz) => (tz, n),
        }
    }

    fn instant(self, default: chrono_tz::Tz) -> Option<DateTime<Utc>> {
        let (tz, naive) = self.local(default);
        at_local(tz, naive)
    }
}

/// A local time as an instant; a time skipped by a clock change moves on
/// by an hour, a repeated one takes the first.
fn at_local(tz: chrono_tz::Tz, naive: NaiveDateTime) -> Option<DateTime<Utc>> {
    tz.from_local_datetime(&naive)
        .earliest()
        .or_else(|| {
            tz.from_local_datetime(&(naive + Duration::hours(1)))
                .earliest()
        })
        .map(|d| d.with_timezone(&Utc))
}

/// An RFC 5545 duration such as `PT1H30M`, `P1D` or `P2W`.
fn duration(value: &str) -> Option<Duration> {
    let v = value.trim();
    let (negative, v) = match v.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, v.strip_prefix('+').unwrap_or(v)),
    };
    let v = v.strip_prefix('P')?;
    let mut total = Duration::zero();
    let mut number = String::new();
    let mut in_time = false;
    for c in v.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => number.push(c),
            unit => {
                let n: i64 = number.parse().ok()?;
                number.clear();
                total += match (unit, in_time) {
                    ('W', _) => Duration::weeks(n),
                    ('D', _) => Duration::days(n),
                    ('H', true) => Duration::hours(n),
                    ('M', true) => Duration::minutes(n),
                    ('S', true) => Duration::seconds(n),
                    _ => return None,
                };
            }
        }
    }
    (number.is_empty()).then_some(if negative { -total } else { total })
}

#[derive(Default)]
struct Vevent<'a> {
    uid: Option<&'a str>,
    start: Option<When>,
    end: Option<When>,
    duration: Option<Duration>,
    rrule: Option<&'a str>,
    exdates: Vec<When>,
    rdates: Vec<When>,
    recurrence_id: Option<When>,
    cancelled: bool,
    free: bool,
    tentative: bool,
}

/// Busy times in an iCal calendar overlapping `[from, to)`, repeating
/// events counted out. Free ("transparent") and cancelled events are left
/// out; a moved or cancelled occurrence replaces the one it stands for.
pub fn ical_busy(
    text: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<Busy>, ProviderError> {
    let lines = unfold(text);
    let parsed: Vec<Line<'_>> = lines.iter().filter_map(|l| parse_line(l)).collect();
    // The calendar's own zone, for floating times and whole days.
    let default = parsed
        .iter()
        .find(|l| l.name == "X-WR-TIMEZONE")
        .and_then(|l| zone(l.value))
        .unwrap_or(chrono_tz::UTC);

    let mut events: Vec<Vevent<'_>> = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut current: Option<Vevent<'_>> = None;
    for l in &parsed {
        match l.name.as_str() {
            "BEGIN" => {
                let kind = l.value.trim().to_ascii_uppercase();
                if kind == "VEVENT" && stack.last().map(String::as_str) == Some("VCALENDAR") {
                    current = Some(Vevent::default());
                }
                stack.push(kind);
                continue;
            }
            "END" => {
                let kind = l.value.trim().to_ascii_uppercase();
                if kind == "VEVENT"
                    && stack.last().map(String::as_str) == Some("VEVENT")
                    && let Some(e) = current.take()
                {
                    events.push(e);
                    if events.len() > ICAL_MAX_EVENTS {
                        return Err(ProviderError::Refused {
                            status: 200,
                            message: "the calendar has too many events to read".into(),
                        });
                    }
                }
                stack.pop();
                continue;
            }
            _ => {}
        }
        // Only the event's own properties, not those of its alarms.
        if stack.last().map(String::as_str) != Some("VEVENT") {
            continue;
        }
        let Some(e) = current.as_mut() else { continue };
        let tzid = l.param("TZID");
        match l.name.as_str() {
            "UID" => e.uid = Some(l.value.trim()),
            "DTSTART" => e.start = When::parse(l.value, tzid, default),
            "DTEND" => e.end = When::parse(l.value, tzid, default),
            "DURATION" => e.duration = duration(l.value),
            "RRULE" => e.rrule = Some(l.value.trim()),
            "EXDATE" => e.exdates.extend(
                l.value
                    .split(',')
                    .filter_map(|v| When::parse(v, tzid, default)),
            ),
            "RDATE" if l.param("VALUE") != Some("PERIOD") => e.rdates.extend(
                l.value
                    .split(',')
                    .filter_map(|v| When::parse(v, tzid, default)),
            ),
            "RECURRENCE-ID" => e.recurrence_id = When::parse(l.value, tzid, default),
            "STATUS" => {
                let s = l.value.trim().to_ascii_uppercase();
                e.cancelled = s == "CANCELLED";
                e.tentative = s == "TENTATIVE";
            }
            "TRANSP" => e.free = l.value.trim().eq_ignore_ascii_case("TRANSPARENT"),
            _ => {}
        }
    }

    // Occurrences moved or cancelled one by one, by event and original start.
    let replaced: HashSet<(&str, DateTime<Utc>)> = events
        .iter()
        .filter_map(|e| Some((e.uid?, e.recurrence_id?.instant(default)?)))
        .collect();

    let mut out = Vec::new();
    for e in &events {
        if e.cancelled || e.free {
            continue;
        }
        let Some(start) = e.start else { continue };
        let length = match (e.end, e.duration) {
            (Some(end), _) => match (start.instant(default), end.instant(default)) {
                (Some(s), Some(en)) => en - s,
                _ => continue,
            },
            (None, Some(d)) => d,
            // A whole day without an end is one day; a moment is nothing.
            (None, None) => match start {
                When::Date(_) => Duration::days(1),
                _ => continue,
            },
        };
        if length <= Duration::zero() {
            continue;
        }
        let mut push = |s: DateTime<Utc>| {
            if s < to && s + length > from {
                out.push(Busy {
                    starts_at: s,
                    ends_at: s + length,
                    tentative: e.tentative,
                });
            }
        };
        let excluded: HashSet<DateTime<Utc>> = e
            .exdates
            .iter()
            .filter_map(|w| w.instant(default))
            .collect();
        let skip = |s: &DateTime<Utc>| {
            excluded.contains(s)
                || (e.recurrence_id.is_none() && e.uid.is_some_and(|u| replaced.contains(&(u, *s))))
        };
        let Some(first) = start.instant(default) else {
            continue;
        };
        match (e.rrule, e.recurrence_id) {
            (Some(rule), None) => {
                let (tz, local) = start.local(default);
                for s in occurrences(rule, tz, local, from - length, to)
                    .into_iter()
                    .chain(e.rdates.iter().filter_map(|w| w.instant(default)))
                {
                    if !skip(&s) {
                        push(s);
                    }
                }
            }
            _ => {
                if !skip(&first) {
                    push(first);
                }
                for s in e.rdates.iter().filter_map(|w| w.instant(default)) {
                    if !skip(&s) {
                        push(s);
                    }
                }
            }
        }
    }
    Ok(merge(out))
}

/// Starts of a repeating event between `after` and `before`. A rule that
/// cannot be read leaves just the first occurrence.
fn occurrences(
    rule: &str,
    tz: chrono_tz::Tz,
    local: NaiveDateTime,
    after: DateTime<Utc>,
    before: DateTime<Utc>,
) -> Vec<DateTime<Utc>> {
    let Some(first) = at_local(tz, local) else {
        return Vec::new();
    };
    let single = || {
        if first >= after && first < before {
            vec![first]
        } else {
            Vec::new()
        }
    };
    let rule = until_in_utc(rule, tz);
    let rtz: rrule::Tz = tz.into();
    let Some(start) = rtz.from_local_datetime(&local).earliest().or_else(|| {
        rtz.from_local_datetime(&(local + Duration::hours(1)))
            .earliest()
    }) else {
        return single();
    };
    let set = match rule
        .parse::<rrule::RRule<rrule::Unvalidated>>()
        .and_then(|r| r.build(start))
    {
        Ok(set) => set,
        Err(e) => {
            tracing::debug!(error = %e, rule = %rule, "a repeat rule could not be read");
            return single();
        }
    };
    set.after(after.with_timezone(&rtz))
        .before(before.with_timezone(&rtz))
        .all(MAX_OCCURRENCES)
        .dates
        .into_iter()
        .map(|d| d.with_timezone(&Utc))
        .collect()
}

/// Writes a rule's `UNTIL` as a UTC time, as RFC 5545 asks when the start
/// has a zone; calendars often give a date or a local time instead.
fn until_in_utc(rule: &str, tz: chrono_tz::Tz) -> String {
    rule.split(';')
        .map(|part| {
            let Some((k, v)) = part.split_once('=') else {
                return part.to_owned();
            };
            if !k.trim().eq_ignore_ascii_case("UNTIL") || v.ends_with('Z') {
                return part.to_owned();
            }
            let local = if v.len() == 8 {
                NaiveDate::parse_from_str(v, "%Y%m%d")
                    .ok()
                    .and_then(|d| d.and_hms_opt(23, 59, 59))
            } else {
                NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok()
            };
            match local.and_then(|l| at_local(tz, l)) {
                Some(at) => format!("UNTIL={}", at.format("%Y%m%dT%H%M%SZ")),
                None => part.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join(";")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .map(|d| d.to_utc())
            .expect("a test time")
    }

    fn busy(a: &str, b: &str) -> Busy {
        Busy {
            starts_at: at(a),
            ends_at: at(b),
            tentative: false,
        }
    }

    #[test]
    fn stretches_are_merged() {
        let t = |a: &str, b: &str| Busy {
            tentative: true,
            ..busy(a, b)
        };
        let merged = merge(vec![
            busy("2027-01-01T12:00:00Z", "2027-01-01T13:00:00Z"),
            busy("2027-01-01T10:00:00Z", "2027-01-01T11:00:00Z"),
            busy("2027-01-01T11:00:00Z", "2027-01-01T11:30:00Z"),
            t("2027-01-01T10:15:00Z", "2027-01-01T10:45:00Z"),
            t("2027-01-01T14:00:00Z", "2027-01-01T15:00:00Z"),
        ]);
        assert_eq!(
            merged,
            vec![
                busy("2027-01-01T10:00:00Z", "2027-01-01T11:30:00Z"),
                busy("2027-01-01T12:00:00Z", "2027-01-01T13:00:00Z"),
                t("2027-01-01T14:00:00Z", "2027-01-01T15:00:00Z"),
            ]
        );
    }

    #[test]
    fn durations_are_read() {
        assert_eq!(duration("PT1H30M"), Some(Duration::minutes(90)));
        assert_eq!(duration("P1D"), Some(Duration::days(1)));
        assert_eq!(duration("P2W"), Some(Duration::weeks(2)));
        assert_eq!(duration("P1DT2H"), Some(Duration::hours(26)));
        assert_eq!(duration("1H"), None);
    }

    #[test]
    fn webcal_links_become_https() {
        assert_eq!(
            ical_url(" webcal://p01-caldav.icloud.com/published/2/abc ").map(|u| u.to_string()),
            Some("https://p01-caldav.icloud.com/published/2/abc".into())
        );
        assert!(ical_url("ftp://x.test/a.ics").is_none());
        assert!(ical_url("https://user:pw@x.test/a.ics").is_none());
        assert!(ical_url("not a link").is_none());
    }

    const CALENDAR: &str = "BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
X-WR-TIMEZONE:Europe/London\r\n\
BEGIN:VEVENT\r\n\
UID:weekly@test\r\n\
SUMMARY:Rehearsal (never kept)\r\n\
DTSTART;TZID=Europe/London:20270301T190000\r\n\
DTEND;TZID=Europe/London:20270301T220000\r\n\
RRULE:FREQ=WEEKLY;BYDAY=MO;UNTIL=20270405\r\n\
EXDATE;TZID=Europe/London:20270315T190000\r\n\
BEGIN:VALARM\r\n\
TRIGGER:-PT15M\r\n\
DTSTART:20000101T000000Z\r\n\
END:VALARM\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:weekly@test\r\n\
RECURRENCE-ID;TZID=Europe/London:20270322T190000\r\n\
DTSTART;TZID=Europe/London:20270323T180000\r\n\
DTEND;TZID=Europe/London:20270323T200000\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:holiday@test\r\n\
DTSTART;VALUE=DATE:20270310\r\n\
DTEND;VALUE=DATE:20270312\r\n\
STATUS:TENTATIVE\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:free@test\r\n\
DTSTART:20270302T090000Z\r\n\
DURATION:PT1H\r\n\
TRANSP:TRANSPARENT\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:off@test\r\n\
DTSTART:20270303T090000Z\r\n\
DURATION:PT1H\r\n\
STATUS:CANCELLED\r\n\
END:VEVENT\r\n\
BEGIN:VEVENT\r\n\
UID:folded@test\r\n\
DTSTART;TZID=\"W. Europe Standard Time\":20270304T100000\r\n\
DTEND;TZID=\"W. Europe Standard Time\":2027030\r\n 4T113000\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

    #[test]
    fn ical_busy_times_are_counted_out() {
        let got = ical_busy(
            CALENDAR,
            at("2027-03-01T00:00:00Z"),
            at("2027-05-01T00:00:00Z"),
        )
        .expect("the calendar reads");
        assert_eq!(
            got,
            vec![
                // Mondays at 19:00 London: GMT, then BST from 28 March.
                busy("2027-03-01T19:00:00Z", "2027-03-01T22:00:00Z"),
                // The folded line, in Berlin time.
                busy("2027-03-04T09:00:00Z", "2027-03-04T10:30:00Z"),
                busy("2027-03-08T19:00:00Z", "2027-03-08T22:00:00Z"),
                // Two whole days in London, only perhaps busy.
                Busy {
                    tentative: true,
                    ..busy("2027-03-10T00:00:00Z", "2027-03-12T00:00:00Z")
                },
                // 15 March is excepted; 22 March moved to the 23rd.
                busy("2027-03-23T18:00:00Z", "2027-03-23T20:00:00Z"),
                busy("2027-03-29T18:00:00Z", "2027-03-29T21:00:00Z"),
                busy("2027-04-05T18:00:00Z", "2027-04-05T21:00:00Z"),
            ]
        );
        // Only what overlaps the window.
        let april = ical_busy(
            CALENDAR,
            at("2027-04-01T00:00:00Z"),
            at("2027-05-01T00:00:00Z"),
        )
        .expect("the calendar reads");
        assert_eq!(
            april,
            vec![busy("2027-04-05T18:00:00Z", "2027-04-05T21:00:00Z")]
        );
    }
}
