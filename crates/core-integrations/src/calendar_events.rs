//! Bookings written into a person's own Google or Microsoft calendar. OnHold
//! makes a calendar of its own in their account and writes only there, so
//! it never touches their other events: Google's `calendar.app.created`
//! scope allows exactly that; Microsoft has no narrower grant than
//! `Calendars.ReadWrite`, so OnHold keeps to the calendar it made.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

use crate::provider::{self, ProviderError, endpoint, segment};

/// Making a calendar of OnHold's own, and writing events in it.
pub const GOOGLE_WRITE_SCOPES: &[&str] = &[
    "openid",
    "email",
    "https://www.googleapis.com/auth/calendar.app.created",
];
pub const MICROSOFT_WRITE_SCOPES: &[&str] = &["offline_access", "User.Read", "Calendars.ReadWrite"];

/// One booking as an event.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EventBody {
    pub summary: String,
    pub description: String,
    pub location: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    /// Whole days, counted in this zone.
    pub all_day: Option<chrono_tz::Tz>,
    /// Holds and pending bookings: perhaps busy.
    pub tentative: bool,
    /// The booking in OnHold.
    pub url: String,
}

impl EventBody {
    /// The first day and the day after the last, for whole-day events.
    fn days(&self, tz: chrono_tz::Tz) -> (String, String) {
        let start = self.starts_at.with_timezone(&tz).date_naive();
        let last = (self.ends_at - Duration::seconds(1))
            .with_timezone(&tz)
            .date_naive();
        let end = last.max(start) + Duration::days(1);
        (
            start.format("%Y-%m-%d").to_string(),
            end.format("%Y-%m-%d").to_string(),
        )
    }

    fn google(&self) -> Value {
        let (start, end) = match self.all_day {
            Some(tz) => {
                let (s, e) = self.days(tz);
                (json!({ "date": s }), json!({ "date": e }))
            }
            None => (
                json!({ "dateTime": self.starts_at.to_rfc3339_opts(SecondsFormat::Secs, true) }),
                json!({ "dateTime": self.ends_at.to_rfc3339_opts(SecondsFormat::Secs, true) }),
            ),
        };
        json!({
            "summary": self.summary,
            "description": format!("{}\n\n{}", self.description, self.url),
            "location": self.location,
            "start": start,
            "end": end,
            "status": if self.tentative { "tentative" } else { "confirmed" },
            "transparency": "opaque",
            "source": { "title": "OnHold", "url": self.url },
            "reminders": { "useDefault": false },
        })
    }

    fn graph(&self) -> Value {
        let at = |d: DateTime<Utc>| d.format("%Y-%m-%dT%H:%M:%S").to_string();
        let (start, end, all_day) = match self.all_day {
            Some(tz) => {
                let (s, e) = self.days(tz);
                (
                    json!({ "dateTime": format!("{s}T00:00:00"), "timeZone": tz.name() }),
                    json!({ "dateTime": format!("{e}T00:00:00"), "timeZone": tz.name() }),
                    true,
                )
            }
            None => (
                json!({ "dateTime": at(self.starts_at), "timeZone": "UTC" }),
                json!({ "dateTime": at(self.ends_at), "timeZone": "UTC" }),
                false,
            ),
        };
        json!({
            "subject": self.summary,
            "body": { "contentType": "text", "content": format!("{}\n\n{}", self.description, self.url) },
            "location": { "displayName": self.location.clone().unwrap_or_default() },
            "start": start,
            "end": end,
            "isAllDay": all_day,
            "showAs": if self.tentative { "tentative" } else { "busy" },
            "isReminderOn": false,
        })
    }
}

/// OnHold's calendar in the account is gone (deleted by its owner): make it
/// again and write everything afresh.
pub fn calendar_gone(e: &ProviderError) -> bool {
    event_gone(e)
}

/// 404, or 410 (Google's "Resource has been deleted"), which arrives as
/// [`ProviderError::CursorExpired`].
fn event_gone(e: &ProviderError) -> bool {
    matches!(
        e,
        ProviderError::Refused { status: 404, .. } | ProviderError::CursorExpired
    )
}

fn conflict(e: &ProviderError) -> bool {
    matches!(e, ProviderError::Refused { status: 409, .. })
}

#[derive(Deserialize)]
struct Created {
    id: String,
}

#[derive(Deserialize)]
struct Calendars {
    value: Vec<Created>,
}

/// Writes events into a calendar OnHold made in someone's account.
pub enum CalendarWriter {
    Google {
        http: reqwest::Client,
        base: Url,
        token: String,
    },
    Microsoft {
        http: reqwest::Client,
        base: Url,
        token: String,
    },
}

impl CalendarWriter {
    pub fn google(http: reqwest::Client, base: Url, token: String) -> Self {
        Self::Google { http, base, token }
    }

    pub fn microsoft(http: reqwest::Client, base: Url, token: String) -> Self {
        Self::Microsoft { http, base, token }
    }

    /// Makes OnHold's calendar in the account; returns its id.
    pub async fn create_calendar(&self, name: &str, tz: &str) -> Result<String, ProviderError> {
        let made: Created = match self {
            Self::Google { http, base, token } => {
                provider::send_json(
                    http.post(endpoint(base, "/calendar/v3/calendars"))
                        .bearer_auth(token)
                        .json(&json!({ "summary": name, "timeZone": tz })),
                )
                .await?
            }
            Self::Microsoft { http, base, token } => {
                let made = provider::send_json(
                    http.post(endpoint(base, "/v1.0/me/calendars"))
                        .bearer_auth(token)
                        .json(&json!({ "name": name })),
                )
                .await;
                match made {
                    // Outlook refuses a second calendar of the same name: one
                    // left from before (a clean-up that did not finish) is
                    // taken over.
                    Err(e) if conflict(&e) => {
                        let mut url = endpoint(base, "/v1.0/me/calendars");
                        url.query_pairs_mut()
                            .append_pair(
                                "$filter",
                                &format!("name eq '{}'", name.replace('\'', "''")),
                            )
                            .append_pair("$select", "id");
                        let found: Calendars =
                            provider::send_json(http.get(url).bearer_auth(token)).await?;
                        found.value.into_iter().next().ok_or(e)?
                    }
                    other => other?,
                }
            }
        };
        Ok(made.id)
    }

    /// Removes OnHold's calendar and everything in it. Already gone is fine.
    pub async fn delete_calendar(&self, calendar: &str) -> Result<(), ProviderError> {
        let req = match self {
            Self::Google { http, base, token } => http
                .delete(endpoint(
                    base,
                    &format!("/calendar/v3/calendars/{}", segment(calendar)),
                ))
                .bearer_auth(token),
            Self::Microsoft { http, base, token } => http
                .delete(endpoint(
                    base,
                    &format!("/v1.0/me/calendars/{}", segment(calendar)),
                ))
                .bearer_auth(token),
        };
        match provider::send_raw(req).await {
            Ok(_) => Ok(()),
            Err(e) if event_gone(&e) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Writes an event: replaces `existing` when given and still there,
    /// otherwise adds one. `key` names the booking in this connection, the
    /// same every time, so adding again after an answer was lost does not
    /// make a second event. Returns the event's id. Fails with
    /// [`calendar_gone`] when the calendar itself is gone.
    pub async fn put(
        &self,
        calendar: &str,
        existing: Option<&str>,
        key: &str,
        event: &EventBody,
    ) -> Result<String, ProviderError> {
        if let Some(id) = existing {
            match self.replace(calendar, id, event).await {
                Ok(done) => return Ok(done),
                // Removed in the calendar: added again below.
                Err(e) if event_gone(&e) => {}
                Err(e) => return Err(e),
            }
        }
        match self {
            Self::Google { http, base, token } => {
                // Google takes the event's id from OnHold: an id already
                // used (an earlier add whose answer was lost, or an event
                // since deleted) is written over instead.
                let mut body = event.google();
                body["id"] = json!(key);
                let added = provider::send_json::<Created>(
                    http.post(endpoint(
                        base,
                        &format!("/calendar/v3/calendars/{}/events", segment(calendar)),
                    ))
                    .bearer_auth(token)
                    .json(&body),
                )
                .await;
                match added {
                    Ok(done) => Ok(done.id),
                    Err(e) if conflict(&e) => self.replace(calendar, key, event).await,
                    Err(e) => Err(e),
                }
            }
            Self::Microsoft { http, base, token } => {
                // Outlook returns the event already added under the same
                // transaction id rather than adding another.
                let mut body = event.graph();
                body["transactionId"] = json!(key);
                Ok(provider::send_json::<Created>(
                    http.post(endpoint(
                        base,
                        &format!("/v1.0/me/calendars/{}/events", segment(calendar)),
                    ))
                    .bearer_auth(token)
                    .json(&body),
                )
                .await?
                .id)
            }
        }
    }

    async fn replace(
        &self,
        calendar: &str,
        id: &str,
        event: &EventBody,
    ) -> Result<String, ProviderError> {
        let req = match self {
            Self::Google { http, base, token } => http
                .put(endpoint(
                    base,
                    &format!(
                        "/calendar/v3/calendars/{}/events/{}",
                        segment(calendar),
                        segment(id)
                    ),
                ))
                .bearer_auth(token)
                .json(&event.google()),
            Self::Microsoft { http, base, token } => http
                .patch(endpoint(base, &format!("/v1.0/me/events/{}", segment(id))))
                .bearer_auth(token)
                .json(&event.graph()),
        };
        Ok(provider::send_json::<Created>(req).await?.id)
    }

    /// Removes an event. Already gone is fine.
    pub async fn delete(&self, calendar: &str, id: &str) -> Result<(), ProviderError> {
        let req = match self {
            Self::Google { http, base, token } => http
                .delete(endpoint(
                    base,
                    &format!(
                        "/calendar/v3/calendars/{}/events/{}",
                        segment(calendar),
                        segment(id)
                    ),
                ))
                .bearer_auth(token),
            Self::Microsoft { http, base, token } => http
                .delete(endpoint(base, &format!("/v1.0/me/events/{}", segment(id))))
                .bearer_auth(token),
        };
        match provider::send_raw(req).await {
            Ok(_) => Ok(()),
            Err(e) if event_gone(&e) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .map(|d| d.to_utc())
            .expect("a test time")
    }

    fn body(all_day: Option<chrono_tz::Tz>) -> EventBody {
        EventBody {
            summary: "HOLD: Summer Ball".into(),
            description: "Status: hold".into(),
            location: Some("The Hall".into()),
            starts_at: at("2027-06-04T23:00:00Z"),
            ends_at: at("2027-06-06T23:00:00Z"),
            all_day,
            tentative: true,
            url: "https://app.test/bookings/1".into(),
        }
    }

    #[test]
    fn whole_days_are_dates_in_the_bookings_zone() {
        let b = body(Some(chrono_tz::Europe::London));
        let g = b.google();
        assert_eq!(g["start"], json!({ "date": "2027-06-05" }));
        assert_eq!(g["end"], json!({ "date": "2027-06-07" }));
        assert_eq!(g["status"], "tentative");
        let m = b.graph();
        assert_eq!(m["isAllDay"], true);
        assert_eq!(m["start"]["dateTime"], "2027-06-05T00:00:00");
        assert_eq!(m["start"]["timeZone"], "Europe/London");
        assert_eq!(m["showAs"], "tentative");
    }

    #[test]
    fn timed_events_are_in_utc() {
        let b = EventBody {
            tentative: false,
            ..body(None)
        };
        assert_eq!(
            b.google()["start"],
            json!({ "dateTime": "2027-06-04T23:00:00Z" })
        );
        assert_eq!(b.google()["status"], "confirmed");
        assert_eq!(
            b.graph()["end"],
            json!({ "dateTime": "2027-06-06T23:00:00", "timeZone": "UTC" })
        );
        assert_eq!(b.graph()["showAs"], "busy");
    }
}
