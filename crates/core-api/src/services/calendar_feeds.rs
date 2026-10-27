//! Calendar feeds: a secret iCal link (RFC 5545) to the workspace's
//! bookings, or one talent's, for Google Calendar, Outlook or Apple
//! Calendar to subscribe to. A link works until revoked, or until the
//! person who made it leaves the workspace. Fees are never included.

use chrono::{DateTime, Duration, Utc};
use core_db::calendar_feeds::Feed;
use core_domain::crypto::{hash_token, new_token};
use core_domain::{BookingStatus, Ctx, Permission};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use super::bookings::BookingView;
use super::crm::Check;
use super::{Change, begin, record};
use crate::AppState;
use crate::error::{ApiError, ApiResult};

/// How far back and ahead a feed reaches.
const PAST_DAYS: i64 = 90;
const FUTURE_DAYS: i64 = 730;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FeedView {
    pub id: Uuid,
    pub name: String,
    /// One talent's bookings, or the whole workspace's when absent.
    pub talent_id: Option<Uuid>,
    pub talent_name: Option<String>,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

impl From<Feed> for FeedView {
    fn from(f: Feed) -> Self {
        Self {
            id: f.id,
            name: f.name,
            talent_id: f.talent_id,
            talent_name: f.talent_name,
            created_by: f.created_by,
            created_at: f.created_at,
            last_used_at: f.last_used_at,
        }
    }
}

/// The caller's feeds; admins see everyone's.
pub async fn list(state: &AppState, ctx: &Ctx) -> ApiResult<Vec<FeedView>> {
    ctx.authorize(Permission::BookingRead)?;
    let all = ctx.authorize(Permission::ConfigWrite).is_ok();
    let me = ctx.user_id().map(|u| u.0);
    let mut tx = begin(state, ctx).await?;
    let rows = core_db::calendar_feeds::list(&mut tx).await?;
    tx.commit().await?;
    Ok(rows
        .into_iter()
        .filter(|f| all || Some(f.created_by) == me)
        .map(Into::into)
        .collect())
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct NewFeed {
    pub name: String,
    /// Only this talent's bookings.
    pub talent_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CreatedFeed {
    pub feed: FeedView,
    /// The link to subscribe to. Shown once; keep it private.
    pub url: String,
}

pub async fn create(state: &AppState, ctx: &Ctx, new: NewFeed) -> ApiResult<CreatedFeed> {
    ctx.authorize(Permission::BookingRead)?;
    let user = ctx.user_id().ok_or_else(|| {
        ApiError::forbidden(
            "person_required",
            "calendar links belong to a person, not a key",
        )
    })?;
    let mut check = Check::default();
    let name = check.required("name", &new.name, 100);
    let mut tx = begin(state, ctx).await?;
    if let Some(t) = new.talent_id
        && core_db::talent::get(&mut tx, t).await?.is_none()
    {
        check.issue("talent_id", "there is no such talent");
    }
    check.finish()?;
    let token = new_token();
    let id = core_domain::ids::new_id();
    core_db::calendar_feeds::insert(&mut tx, id, new.talent_id, &name, &hash_token(&token), user)
        .await?;
    let feed: FeedView = core_db::calendar_feeds::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?
        .into();
    record(
        &mut tx,
        ctx,
        Change {
            topic: "calendar_feed.created",
            resource: "calendar_feed",
            object_id: Some(id),
            before: None,
            after: Some(json!({ "name": feed.name, "talent_id": feed.talent_id })),
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(CreatedFeed {
        feed,
        url: state
            .config
            .app_url(&format!("/api/v1/public/calendar/{token}.ics"), &[]),
    })
}

/// Stops a link working. Its maker or an admin may.
pub async fn revoke(state: &AppState, ctx: &Ctx, id: Uuid) -> ApiResult<()> {
    ctx.authorize(Permission::BookingRead)?;
    let mut tx = begin(state, ctx).await?;
    let feed = core_db::calendar_feeds::get(&mut tx, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if ctx.user_id().map(|u| u.0) != Some(feed.created_by) {
        ctx.authorize(Permission::ConfigWrite)?;
    }
    core_db::calendar_feeds::revoke(&mut tx, id).await?;
    record(
        &mut tx,
        ctx,
        Change {
            topic: "calendar_feed.revoked",
            resource: "calendar_feed",
            object_id: Some(id),
            before: Some(json!({ "name": feed.name, "talent_id": feed.talent_id })),
            after: None,
            event_extra: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The feed behind a link, as iCal text. `None` when the link does not work.
pub async fn render_public(state: &AppState, token: &str) -> ApiResult<Option<String>> {
    let mut tx = core_db::begin(&state.db, None, None).await?;
    let Some(found) = core_db::calendar_feeds::find(&mut tx, &hash_token(token)).await? else {
        tx.commit().await?;
        return Ok(None);
    };
    core_db::set_context(&mut tx, Some(found.workspace_id), None).await?;
    let now = Utc::now();
    let statuses = [
        BookingStatus::Hold,
        BookingStatus::Pending,
        BookingStatus::Confirmed,
        BookingStatus::Completed,
    ];
    let (rows, _) = core_db::bookings::list(
        &mut tx,
        core_db::bookings::Filter {
            list: core_db::crm::ListParams {
                q: None,
                searchable: &[],
                tag: None,
                custom: None,
                sort: "starts_at",
                sort_custom: None,
                desc: false,
                offset: 0,
                limit: 5000,
            },
            from: Some(now - Duration::days(PAST_DAYS)),
            to: Some(now + Duration::days(FUTURE_DAYS)),
            statuses: &statuses,
            talent_id: found.talent_id,
            organisation_id: None,
            contact_id: None,
            enquiry_id: None,
        },
    )
    .await?;
    let ids: Vec<Uuid> = rows.iter().map(|b| b.id).collect();
    let talent = core_db::bookings::talent_of(&mut tx, &ids).await?;
    let workspace = core_db::workspaces::get(&mut tx, found.workspace_id).await?;
    tx.commit().await?;
    let host = state
        .config
        .public_base_url
        .host_str()
        .unwrap_or("onhold.invalid")
        .to_owned();
    let calendar_name = match workspace {
        Some(w) => format!("{} · {}", w.name, found.name),
        None => found.name.clone(),
    };
    let events: Vec<Event> = rows
        .into_iter()
        .map(|b| {
            let names = super::links::talent_by_owner(talent.clone(), b.id);
            let url = state.config.app_url(&format!("/bookings/{}", b.id), &[]);
            Event::from_booking(&super::bookings::view(b, names), &host, url)
        })
        .collect();
    Ok(Some(ical(&calendar_name, &events, now)))
}

/// One VEVENT.
#[derive(Debug, Clone)]
pub struct Event {
    pub uid: String,
    pub summary: String,
    pub status: &'static str,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    /// All-day events give dates in this zone.
    pub all_day: Option<chrono_tz::Tz>,
    pub location: Option<String>,
    pub description: String,
    pub url: String,
    pub sequence: i64,
    pub updated_at: DateTime<Utc>,
}

impl Event {
    fn from_booking(b: &BookingView, host: &str, url: String) -> Self {
        let (status, prefix) = match b.status {
            BookingStatus::Hold => ("TENTATIVE", "HOLD: "),
            BookingStatus::Pending => ("TENTATIVE", "PENDING: "),
            _ => ("CONFIRMED", ""),
        };
        let talent: Vec<&str> = b.talent.iter().map(|t| t.name.as_str()).collect();
        let mut description = format!("Status: {}", b.status);
        if !talent.is_empty() {
            description.push_str(&format!("\nTalent: {}", talent.join(", ")));
        }
        for s in &b.schedule {
            let tz: chrono_tz::Tz = b.timezone.parse().unwrap_or(chrono_tz::UTC);
            description.push_str(&format!(
                "\n{}: {}",
                s.label,
                s.at.with_timezone(&tz).format("%H:%M")
            ));
        }
        Self {
            uid: format!("{}@{host}", b.id),
            summary: format!("{prefix}{}", b.title),
            status,
            starts_at: b.starts_at,
            ends_at: b.ends_at,
            all_day: b
                .all_day
                .then(|| b.timezone.parse().unwrap_or(chrono_tz::UTC)),
            location: b.venue_name.clone().or(b.venue_text.clone()),
            description,
            url,
            sequence: b.version,
            updated_at: b.updated_at,
        }
    }
}

/// Escapes text for an iCal property value.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// Folds a content line at 75 octets, never inside a character.
pub fn fold(line: &str) -> String {
    let mut out = String::with_capacity(line.len() + line.len() / 70 * 3);
    let mut width = 0;
    for c in line.chars() {
        let len = c.len_utf8();
        if width + len > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += len;
    }
    out.push_str("\r\n");
    out
}

fn stamp(at: DateTime<Utc>) -> String {
    at.format("%Y%m%dT%H%M%SZ").to_string()
}

/// A whole calendar.
pub fn ical(name: &str, events: &[Event], now: DateTime<Utc>) -> String {
    let mut out = String::new();
    let mut line = |s: String| out.push_str(&fold(&s));
    line("BEGIN:VCALENDAR".into());
    line("VERSION:2.0".into());
    line("PRODID:-//OnHold//Bookings//EN".into());
    line("CALSCALE:GREGORIAN".into());
    line("METHOD:PUBLISH".into());
    line(format!("X-WR-CALNAME:{}", escape(name)));
    for e in events {
        line("BEGIN:VEVENT".into());
        line(format!("UID:{}", escape(&e.uid)));
        line(format!("DTSTAMP:{}", stamp(now)));
        match e.all_day {
            Some(tz) => {
                let start = e.starts_at.with_timezone(&tz).date_naive();
                // DTEND is exclusive: the day after the last day.
                let last = (e.ends_at - Duration::seconds(1))
                    .with_timezone(&tz)
                    .date_naive();
                let end = last.max(start) + Duration::days(1);
                line(format!("DTSTART;VALUE=DATE:{}", start.format("%Y%m%d")));
                line(format!("DTEND;VALUE=DATE:{}", end.format("%Y%m%d")));
            }
            None => {
                line(format!("DTSTART:{}", stamp(e.starts_at)));
                line(format!("DTEND:{}", stamp(e.ends_at)));
            }
        }
        line(format!("SUMMARY:{}", escape(&e.summary)));
        line(format!("STATUS:{}", e.status));
        if let Some(l) = &e.location {
            line(format!("LOCATION:{}", escape(l)));
        }
        line(format!("DESCRIPTION:{}", escape(&e.description)));
        line(format!("URL:{}", e.url));
        line(format!("SEQUENCE:{}", e.sequence));
        line(format!("LAST-MODIFIED:{}", stamp(e.updated_at)));
        line("END:VEVENT".into());
    }
    line("END:VCALENDAR".into());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn text_is_escaped_and_long_lines_folded() {
        assert_eq!(escape("a;b,c\\d\ne"), "a\\;b\\,c\\\\d\\ne");
        let long = format!("SUMMARY:{}", "é".repeat(60));
        let folded = fold(&long);
        for part in folded.split("\r\n").filter(|p| !p.is_empty()) {
            assert!(part.len() <= 75, "{} octets", part.len());
        }
        assert_eq!(folded.replace("\r\n ", "").trim_end(), long);
    }

    #[test]
    fn all_day_events_use_dates_in_their_zone() {
        let tz = chrono_tz::Europe::London;
        let starts = tz.with_ymd_and_hms(2027, 7, 3, 0, 0, 0).unwrap().to_utc();
        let ends = tz.with_ymd_and_hms(2027, 7, 5, 0, 0, 0).unwrap().to_utc();
        let e = Event {
            uid: "x@h".into(),
            summary: "Festival".into(),
            status: "CONFIRMED",
            starts_at: starts,
            ends_at: ends,
            all_day: Some(tz),
            location: None,
            description: "Status: confirmed".into(),
            url: "https://example.com/bookings/x".into(),
            sequence: 1,
            updated_at: starts,
        };
        let cal = ical("Test", &[e], starts);
        assert!(cal.contains("DTSTART;VALUE=DATE:20270703\r\n"));
        assert!(cal.contains("DTEND;VALUE=DATE:20270705\r\n"));
        assert!(cal.starts_with("BEGIN:VCALENDAR\r\n"));
        assert!(cal.ends_with("END:VCALENDAR\r\n"));
    }
}
