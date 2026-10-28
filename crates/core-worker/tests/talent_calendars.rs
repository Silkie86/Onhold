//! M6 talent's own calendars, busy times only, against stand-ins for
//! Google (OAuth, free/busy), Microsoft (OAuth, calendar view) and a
//! published iCal file: the link the agency sends, connecting through it
//! without an account, the worker reading busy times, clash checks
//! counting them, a calendar changing, a grant withdrawn, and calendars
//! and links removed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../core-api/tests/common/mod.rs"]
mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::{DateTime, Duration as Span, DurationRound, Utc};
use common::{Client, TestApp};
use core_api::Config;
use core_api::config::MailboxConfig;
use core_integrations::http::WebhookClient;
use core_integrations::mail::MemoryMailer;
use core_integrations::oauth::OAuthApp;
use core_worker::{Settings, Worker};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use url::Url;

/// What the stand-ins hold and were asked.
#[derive(Default)]
struct World {
    /// Busy stretches on the Google calendar.
    google: Vec<(DateTime<Utc>, DateTime<Utc>)>,
    /// Events on the Microsoft calendar: (start, end, show as).
    microsoft: Vec<(DateTime<Utc>, DateTime<Utc>, &'static str)>,
    ical: String,
    /// The Google grant was withdrawn.
    withdrawn: bool,
    /// Grants also carry mailbox access given to the same app before.
    wide: bool,
    freebusy_calls: usize,
    /// What Microsoft was asked to return.
    selects: Vec<String>,
    revoked: Vec<String>,
}

type Shared = Arc<Mutex<World>>;

fn form(body: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

async fn token(State(w): State<Shared>, body: String) -> Response {
    let f = form(&body);
    let w = w.lock().unwrap();
    let microsoft = f.get("client_id").map(String::as_str) == Some("ms-client");
    match (f["grant_type"].as_str(), f.get("code").map(String::as_str)) {
        ("authorization_code", Some("good-code")) => {}
        ("refresh_token", _) if !w.withdrawn => {}
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({ "error": "invalid_grant",
                    "error_description": "Token has been expired or revoked." })),
            )
                .into_response();
        }
    }
    let mut scope = if microsoft {
        "Calendars.ReadBasic User.Read profile openid email"
    } else {
        "openid https://www.googleapis.com/auth/calendar.freebusy https://www.googleapis.com/auth/userinfo.email"
    }
    .to_owned();
    if w.wide {
        scope.push_str(" https://www.googleapis.com/auth/gmail.readonly");
    }
    axum::Json(json!({
        "access_token": if microsoft { "ms-access" } else { "google-access" },
        "refresh_token": if microsoft { "ms-refresh" } else { "google-refresh" },
        "expires_in": 3599,
        "scope": scope,
        "token_type": "Bearer",
    }))
    .into_response()
}

async fn stand_ins() -> (Url, Shared) {
    let world: Shared = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/token", post(token))
        .route("/common/oauth2/v2.0/token", post(token))
        .route(
            "/revoke",
            post(|State(w): State<Shared>, body: String| async move {
                w.lock().unwrap().revoked.push(form(&body)["token"].clone());
                StatusCode::OK
            }),
        )
        .route(
            "/oauth2/v3/userinfo",
            get(|headers: HeaderMap| async move {
                assert_eq!(headers["authorization"], "Bearer google-access");
                axum::Json(json!({ "sub": "1", "email": "Sam@Gmail.test", "email_verified": true }))
            }),
        )
        .route(
            "/calendar/v3/freeBusy",
            post(
                |State(w): State<Shared>, headers: HeaderMap, body: axum::Json<Value>| async move {
                    let mut w = w.lock().unwrap();
                    if w.withdrawn {
                        return (
                            StatusCode::UNAUTHORIZED,
                            axum::Json(json!({ "error": { "code": 401, "message": "Invalid Credentials" } })),
                        );
                    }
                    assert_eq!(headers["authorization"], "Bearer google-access");
                    assert_eq!(body["items"], json!([{ "id": "primary" }]));
                    w.freebusy_calls += 1;
                    let from: DateTime<Utc> = body["timeMin"].as_str().unwrap().parse().unwrap();
                    let to: DateTime<Utc> = body["timeMax"].as_str().unwrap().parse().unwrap();
                    assert!(to - from <= Span::days(60), "one window at a time");
                    // Google clips stretches to the window asked about.
                    let busy: Vec<Value> = w
                        .google
                        .iter()
                        .filter(|(s, e)| *s < to && *e > from)
                        .map(|(s, e)| json!({ "start": stamp((*s).max(from)), "end": stamp((*e).min(to)) }))
                        .collect();
                    (
                        StatusCode::OK,
                        axum::Json(json!({
                            "kind": "calendar#freeBusy",
                            "timeMin": body["timeMin"], "timeMax": body["timeMax"],
                            "calendars": { "primary": { "busy": busy } }
                        })),
                    )
                },
            ),
        )
        .route(
            "/v1.0/me",
            get(|| async {
                axum::Json(json!({ "mail": "Alex@Contoso.test", "userPrincipalName": "alex@contoso.test" }))
            }),
        )
        .route(
            "/v1.0/me/calendarView",
            get(
                |State(w): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>| async move {
                    assert_eq!(headers["authorization"], "Bearer ms-access");
                    assert_eq!(headers["prefer"], "outlook.timezone=\"UTC\"");
                    let mut w = w.lock().unwrap();
                    w.selects.push(q.get("$select").cloned().unwrap_or_default());
                    // Two events a page, as Graph pages them.
                    let page: usize = q.get("page").map_or(0, |p| p.parse().unwrap());
                    let events: Vec<Value> = w
                        .microsoft
                        .iter()
                        .skip(page * 2)
                        .take(2)
                        .map(|(s, e, show)| {
                            json!({
                                "start": { "dateTime": s.format("%Y-%m-%dT%H:%M:%S.0000000").to_string(), "timeZone": "UTC" },
                                "end": { "dateTime": e.format("%Y-%m-%dT%H:%M:%S.0000000").to_string(), "timeZone": "UTC" },
                                "showAs": show,
                                "isCancelled": false,
                            })
                        })
                        .collect();
                    let mut body = json!({ "value": events });
                    if (page + 1) * 2 < w.microsoft.len() {
                        body["@odata.nextLink"] = json!(format!(
                            "https://graph.microsoft.com/v1.0/me/calendarView?startDateTime={}&endDateTime={}&$select={}&page={}",
                            q["startDateTime"], q["endDateTime"], q["$select"], page + 1
                        ));
                    }
                    axum::Json(body)
                },
            ),
        )
        .route(
            "/cal.ics",
            get(|State(w): State<Shared>| async move {
                (
                    [("content-type", "text/calendar; charset=utf-8")],
                    w.lock().unwrap().ical.clone(),
                )
            }),
        )
        .route("/not-a-calendar", get(|| async { "<html>hello</html>" }))
        .with_state(world.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (Url::parse(&format!("http://{addr}")).unwrap(), world)
}

struct Harness {
    app: TestApp,
    worker: Worker,
    world: Shared,
    base: Url,
}

async fn harness(pool: PgPool) -> Harness {
    let (base, world) = stand_ins().await;
    let config = Config {
        mailboxes: MailboxConfig {
            google: Some(
                OAuthApp::google("google-client".into(), "google-secret".into()).at(&base),
            ),
            microsoft: Some(OAuthApp::microsoft_at(
                "ms-client".into(),
                "ms-secret".into(),
                "common",
                base.as_str(),
            )),
            gmail_api: base.clone(),
            graph_api: base.clone(),
            google_api: base.clone(),
            ..MailboxConfig::default()
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
    Harness {
        app,
        worker,
        world,
        base,
    }
}

impl Harness {
    async fn drain(&self) {
        for _ in 0..50 {
            let published = self.worker.publish_once().await.unwrap();
            let ran = self.worker.run_jobs_once().await.unwrap();
            if published == 0 && ran == 0 {
                return;
            }
        }
        panic!("the queue did not settle");
    }

    /// Makes every calendar due a read now and lets the worker read them.
    async fn read_all(&self) {
        sqlx::query(
            "UPDATE talent_calendars SET next_poll_at = now() - interval '1 second' WHERE next_poll_at IS NOT NULL",
        )
        .execute(&self.app.owner)
        .await
        .unwrap();
        self.worker.schedule_once().await.unwrap();
        self.drain().await;
    }

    async fn calendars(&self, who: &Client, talent: &str) -> Value {
        self.app
            .get(&format!("/api/v1/talent/{talent}/calendars"))
            .signed_in(who)
            .send()
            .await
            .ok()
            .body
    }

    /// The talent's availability over the next year: (source, kind, note, start, end).
    async fn availability(
        &self,
        who: &Client,
        talent: &str,
    ) -> Vec<(String, String, String, String, String)> {
        let to = stamp(Utc::now() + Span::days(390));
        let from = stamp(Utc::now() - Span::days(2));
        let body = self
            .app
            .get(&format!(
                "/api/v1/talent/{talent}/availability?from={from}&to={to}"
            ))
            .signed_in(who)
            .send()
            .await
            .ok()
            .body;
        body.as_array()
            .unwrap()
            .iter()
            .map(|a| {
                let s = |k: &str| a[k].as_str().unwrap_or_default().to_owned();
                (
                    s("source"),
                    s("kind"),
                    s("note"),
                    s("starts_at"),
                    s("ends_at"),
                )
            })
            .collect()
    }

    /// Opens a link's connect page through its public API: the token.
    async fn new_link(&self, who: &Client, talent: &str) -> String {
        let made = self
            .app
            .post(&format!("/api/v1/talent/{talent}/calendar-links"))
            .signed_in(who)
            .send()
            .await
            .ok()
            .body;
        let url = Url::parse(made["url"].as_str().unwrap()).unwrap();
        let path = url.path().to_owned();
        path.strip_prefix("/connect-calendar/").unwrap().to_owned()
    }

    /// Connects through Google or Microsoft as the talent would.
    async fn connect(&self, token: &str, provider: &str, person: &str) -> Value {
        let start = self
            .app
            .post(&format!("/api/v1/public/calendar-connect/{token}/start"))
            .json(json!({ "provider": provider, "person": person }))
            .send()
            .await
            .ok()
            .body;
        let url = Url::parse(start["url"].as_str().unwrap()).unwrap();
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert!(q["redirect_uri"].ends_with("/connect-calendar"), "{url}");
        let state = q["state"].clone();
        self.app
            .post("/api/v1/public/calendar-connect/finish")
            .json(json!({ "code": "good-code", "state": state }))
            .send()
            .await
            .ok()
            .body
    }
}

/// A whole hour some days ahead, so times compare exactly.
fn ahead(days: i64, hour: i64) -> DateTime<Utc> {
    Utc::now().duration_trunc(Span::days(1)).unwrap() + Span::days(days) + Span::hours(hour)
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn talent_connect_their_calendars_and_busy_times_count_as_clashes(pool: PgPool) {
    let h = harness(pool).await;
    let (ws, owner) = h.app.workspace_with_owner("owls").await;
    let talent = h
        .app
        .post("/api/v1/talent")
        .signed_in(&owner)
        .json(json!({ "name": "The Night Owls", "kind": "act" }))
        .send()
        .await
        .ok()
        .id("/id")
        .to_string();
    let empty = h.calendars(&owner, &talent).await;
    assert_eq!(empty["calendars"], json!([]));
    assert_eq!(empty["google"], true);
    assert_eq!(empty["microsoft"], true);

    {
        let mut w = h.world.lock().unwrap();
        w.google = vec![
            (ahead(3, 18), ahead(3, 23)),
            // Crosses a 60-day window: read whole, as one stretch.
            (ahead(59, 20), ahead(61, 2)),
        ];
        w.microsoft = vec![
            (ahead(5, 9), ahead(5, 17), "busy"),
            (ahead(6, 9), ahead(6, 10), "free"),
            (ahead(7, 9), ahead(7, 12), "tentative"),
            (ahead(8, 0), ahead(9, 0), "oof"),
        ];
        let local = |d: DateTime<Utc>| d.format("%Y%m%dT%H%M%SZ").to_string();
        w.ical = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:a@test\r\nSUMMARY:Private\r\nDTSTART:{}\r\nDTEND:{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            local(ahead(10, 12)),
            local(ahead(10, 14))
        );
    }

    // The agency makes a link; the talent opens it without an account.
    let token = h.new_link(&owner, &talent).await;
    let page = h
        .app
        .get(&format!("/api/v1/public/calendar-connect/{token}"))
        .send()
        .await
        .ok()
        .body;
    assert_eq!(page["talent_name"], "The Night Owls");
    assert_eq!(page["workspace_name"], "owls agency");
    assert_eq!(page["connected"], json!([]));

    // Google asks only for free/busy.
    let start = h
        .app
        .post(&format!("/api/v1/public/calendar-connect/{token}/start"))
        .json(json!({ "provider": "google" }))
        .send()
        .await
        .ok()
        .body;
    let scope: String = Url::parse(start["url"].as_str().unwrap())
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "scope")
        .unwrap()
        .1
        .into_owned();
    assert_eq!(
        scope,
        "openid email https://www.googleapis.com/auth/calendar.freebusy"
    );
    // ...and never for what the same app was given before.
    assert!(
        !start["url"]
            .as_str()
            .unwrap()
            .contains("include_granted_scopes")
    );

    let google = h.connect(&token, "google", " Sam ").await;
    assert_eq!(google["account"], "sam@gmail.test");
    assert_eq!(google["talent_name"], "The Night Owls");
    let microsoft = h.connect(&token, "microsoft", "Alex").await;
    assert_eq!(microsoft["account"], "alex@contoso.test");
    let ical_url = format!("{}cal.ics", h.base);
    // A page that is not a calendar is refused while they are there.
    h.app
        .post(&format!("/api/v1/public/calendar-connect/{token}/ical"))
        .json(json!({ "url": format!("{}not-a-calendar", h.base) }))
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "calendar_unreadable");
    let ical = h
        .app
        .post(&format!("/api/v1/public/calendar-connect/{token}/ical"))
        .json(json!({ "url": ical_url, "person": "Jo" }))
        .send()
        .await
        .ok()
        .body;
    assert_eq!(ical["account"], "127.0.0.1");
    h.drain().await;

    let cals = h.calendars(&owner, &talent).await;
    let list = cals["calendars"].as_array().unwrap();
    assert_eq!(list.len(), 3, "{cals}");
    for c in list {
        assert_eq!(c["status"], "active", "{c}");
        assert!(c["last_synced_at"].is_string(), "{c}");
        assert!(c.get("secret").is_none());
    }
    assert_eq!(list[0]["person"], "Sam");
    assert_eq!(cals["links"].as_array().unwrap().len(), 1);
    // A year ahead in 60-day windows; Microsoft asked for times only.
    assert_eq!(h.world.lock().unwrap().freebusy_calls, 7);
    assert!(
        h.world
            .lock()
            .unwrap()
            .selects
            .iter()
            .all(|s| s == "start,end,showAs,isCancelled,isAllDay,originalStartTimeZone")
    );
    // Pasting the same iCal link again renews that calendar, not a second.
    h.app
        .post(&format!("/api/v1/public/calendar-connect/{token}/ical"))
        .json(json!({ "url": ical_url }))
        .send()
        .await
        .ok();
    h.drain().await;
    let cals = h.calendars(&owner, &talent).await;
    assert_eq!(cals["calendars"].as_array().unwrap().len(), 3, "{cals}");

    let busy = h.availability(&owner, &talent).await;
    let row = |src: &str, kind: &str, who: &str, s: DateTime<Utc>, e: DateTime<Utc>| {
        (
            src.to_owned(),
            kind.to_owned(),
            format!("Busy in their own calendar ({who})"),
            s.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
            e.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
        )
    };
    assert_eq!(
        busy,
        vec![
            row("external", "unavailable", "Sam", ahead(3, 18), ahead(3, 23)),
            row("external", "unavailable", "Alex", ahead(5, 9), ahead(5, 17)),
            row("external", "tentative", "Alex", ahead(7, 9), ahead(7, 12)),
            row("external", "unavailable", "Alex", ahead(8, 0), ahead(9, 0)),
            row(
                "external",
                "unavailable",
                "Jo",
                ahead(10, 12),
                ahead(10, 14)
            ),
            row(
                "external",
                "unavailable",
                "Sam",
                ahead(59, 20),
                ahead(61, 2)
            ),
        ]
    );

    // A booking then clashes, and confirming it is blocked.
    let clash = h
        .app
        .post("/api/v1/bookings/clash-check")
        .signed_in(&owner)
        .json(json!({ "talent_ids": [talent], "starts_at": stamp(ahead(5, 12)), "ends_at": stamp(ahead(5, 14)) }))
        .send()
        .await
        .ok()
        .body;
    assert_eq!(clash["blocking"], true, "{clash}");
    assert_eq!(
        clash["clashes"][0]["title"],
        "Busy in their own calendar (Alex)"
    );
    // Busy time within a booking OnHold knows is most likely that booking,
    // written into their calendar: it only warns.
    let booking = h
        .app
        .post("/api/v1/bookings")
        .signed_in(&owner)
        .json(json!({ "title": "Day show", "talent_ids": [talent],
            "starts_at": stamp(ahead(5, 8)), "ends_at": stamp(ahead(5, 18)) }))
        .send()
        .await
        .ok()
        .id("/id")
        .to_string();
    let own = h
        .app
        .post("/api/v1/bookings/clash-check")
        .signed_in(&owner)
        .json(
            json!({ "talent_ids": [talent], "starts_at": stamp(ahead(5, 8)),
            "ends_at": stamp(ahead(5, 18)), "except_id": booking }),
        )
        .send()
        .await
        .ok()
        .body;
    assert_eq!(own["blocking"], false, "{own}");
    assert_eq!(
        own["clashes"][0]["title"],
        "Busy in their own calendar (Alex)"
    );
    h.app
        .delete(&format!("/api/v1/bookings/{booking}"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    // Busy time from a calendar changes there, not here.
    let entry = clash["clashes"][0]["availability_id"].as_str().unwrap();
    h.app
        .patch(&format!("/api/v1/availability/{entry}"))
        .signed_in(&owner)
        .json(json!({ "note": "mine now" }))
        .send()
        .await
        .expect(StatusCode::CONFLICT, "from_their_calendar");
    h.app
        .delete(&format!("/api/v1/availability/{entry}"))
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::CONFLICT, "from_their_calendar");

    // Their calendar changes: the next read follows it, and nothing else is rewritten.
    let kept: (String,) = sqlx::query_as("SELECT id::text FROM calendar_busy WHERE starts_at = $1")
        .bind(ahead(5, 9))
        .fetch_one(&h.app.owner)
        .await
        .unwrap();
    h.world.lock().unwrap().google = vec![(ahead(4, 18), ahead(4, 23))];
    h.read_all().await;
    let busy = h.availability(&owner, &talent).await;
    assert_eq!(busy.len(), 5, "{busy:?}");
    assert_eq!(
        busy[0],
        row("external", "unavailable", "Sam", ahead(4, 18), ahead(4, 23))
    );
    let still: (String,) =
        sqlx::query_as("SELECT id::text FROM calendar_busy WHERE starts_at = $1")
            .bind(ahead(5, 9))
            .fetch_one(&h.app.owner)
            .await
            .unwrap();
    assert_eq!(kept, still, "an unchanged stretch is left alone");

    // Sam withdraws OnHold's access: the calendar asks to be connected
    // again, and its busy times stop counting meanwhile.
    h.world.lock().unwrap().withdrawn = true;
    h.read_all().await;
    let cals = h.calendars(&owner, &talent).await;
    let sam = cals["calendars"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["person"] == "Sam")
        .unwrap()
        .clone();
    assert_eq!(sam["status"], "reconnect", "{sam}");
    assert!(
        sam["last_error"].as_str().unwrap().contains("connect"),
        "{sam}"
    );
    assert_eq!(h.availability(&owner, &talent).await.len(), 4);
    let sam_id = sam["id"].as_str().unwrap();
    h.app
        .post(&format!("/api/v1/talent/{talent}/calendars/{sam_id}/sync"))
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::CONFLICT, "reconnect_required");
    // Connecting again through the link brings it back, the same calendar.
    h.world.lock().unwrap().withdrawn = false;
    let again = h.connect(&token, "google", "").await;
    assert_eq!(again["account"], "sam@gmail.test");
    h.drain().await;
    let cals = h.calendars(&owner, &talent).await;
    assert_eq!(cals["calendars"].as_array().unwrap().len(), 3);
    assert_eq!(cals["calendars"][0]["status"], "active");
    assert_eq!(cals["calendars"][0]["person"], "Sam");

    // Others in the workspace may look; only those who manage talent change.
    let viewer = h.app.add_member(&owner, "viewer@owls.test", "viewer").await;
    assert_eq!(
        h.calendars(&viewer, &talent).await["calendars"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    h.app
        .post(&format!("/api/v1/talent/{talent}/calendar-links"))
        .signed_in(&viewer)
        .send()
        .await
        .expect(StatusCode::FORBIDDEN, "permission_denied");
    // Another workspace sees nothing of it.
    let (_, other) = h.app.workspace_with_owner("hawks").await;
    h.app
        .get(&format!("/api/v1/talent/{talent}/calendars"))
        .signed_in(&other)
        .send()
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");

    // A grant carrying more than busy times is refused, and left alone.
    h.world.lock().unwrap().wide = true;
    let start = h
        .app
        .post(&format!("/api/v1/public/calendar-connect/{token}/start"))
        .json(json!({ "provider": "google" }))
        .send()
        .await
        .ok()
        .body;
    let state = Url::parse(start["url"].as_str().unwrap())
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    h.app
        .post("/api/v1/public/calendar-connect/finish")
        .json(json!({ "code": "good-code", "state": state }))
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "scopes_too_wide");
    h.world.lock().unwrap().wide = false;

    // Sam's calendar also connected for another talent.
    let solo = h
        .app
        .post("/api/v1/talent")
        .signed_in(&owner)
        .json(json!({ "name": "Sam Solo" }))
        .send()
        .await
        .ok()
        .id("/id")
        .to_string();
    let solo_token = h.new_link(&owner, &solo).await;
    h.connect(&solo_token, "google", "").await;
    h.drain().await;

    // Disconnecting forgets the grant and the busy times; Google keeps the
    // grant while the other connection uses the account.
    h.app
        .delete(&format!("/api/v1/talent/{talent}/calendars/{sam_id}"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    assert!(
        h.availability(&owner, &talent)
            .await
            .iter()
            .all(|r| !r.2.contains("Sam"))
    );
    assert!(h.world.lock().unwrap().revoked.is_empty());
    let kept: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM talent_calendars WHERE disconnected_at IS NOT NULL AND length(secret) > 0",
    )
    .fetch_one(&h.app.owner)
    .await
    .unwrap();
    assert_eq!(kept, 0, "a disconnected calendar keeps no grant");
    // Removing the other talent disconnects the last use: the worker asks
    // Google to forget it.
    h.app
        .delete(&format!("/api/v1/talent/{solo}"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    h.worker.schedule_once().await.unwrap();
    h.drain().await;
    assert_eq!(h.world.lock().unwrap().revoked, vec!["google-refresh"]);
    let kept: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM talent_calendars WHERE disconnected_at IS NOT NULL AND length(secret) > 0",
    )
    .fetch_one(&h.app.owner)
    .await
    .unwrap();
    assert_eq!(kept, 0);

    // Revoking the link stops it working, even part way through connecting.
    let link_id = cals["links"][0]["id"].as_str().unwrap();
    let start = h
        .app
        .post(&format!("/api/v1/public/calendar-connect/{token}/start"))
        .json(json!({ "provider": "google" }))
        .send()
        .await
        .ok()
        .body;
    h.app
        .delete(&format!("/api/v1/talent/{talent}/calendar-links/{link_id}"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    h.app
        .get(&format!("/api/v1/public/calendar-connect/{token}"))
        .send()
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");
    let state = Url::parse(start["url"].as_str().unwrap())
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    h.app
        .post("/api/v1/public/calendar-connect/finish")
        .json(json!({ "code": "good-code", "state": state }))
        .send()
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");

    let actions = h.app.audit_actions(ws).await;
    for a in [
        "calendar_connect_link.created",
        "talent_calendar.connected",
        "talent_calendar.health_changed",
        "talent_calendar.reconnected",
        "talent_calendar.disconnected",
        "calendar_connect_link.revoked",
    ] {
        assert!(actions.iter().any(|x| x == a), "{a} in {actions:?}");
    }
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn links_expire_and_removed_talent_take_their_calendars(pool: PgPool) {
    let h = harness(pool).await;
    let (_, owner) = h.app.workspace_with_owner("owls").await;
    let talent = h
        .app
        .post("/api/v1/talent")
        .signed_in(&owner)
        .json(json!({ "name": "Sam Solo" }))
        .send()
        .await
        .ok()
        .id("/id")
        .to_string();
    h.world.lock().unwrap().ical = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n".into();
    let token = h.new_link(&owner, &talent).await;
    // A link that is not a web address is refused.
    h.app
        .post(&format!("/api/v1/public/calendar-connect/{token}/ical"))
        .json(json!({ "url": "my calendar" }))
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST, "invalid_input");
    h.app
        .post(&format!("/api/v1/public/calendar-connect/{token}/ical"))
        .json(json!({ "url": format!("{}cal.ics", h.base) }))
        .send()
        .await
        .ok();
    h.drain().await;
    assert_eq!(
        h.calendars(&owner, &talent).await["calendars"][0]["status"],
        "active"
    );

    // Two weeks on, the link no longer works.
    sqlx::query("UPDATE calendar_connect_links SET expires_at = now() - interval '1 second'")
        .execute(&h.app.owner)
        .await
        .unwrap();
    h.app
        .get(&format!("/api/v1/public/calendar-connect/{token}"))
        .send()
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");
    assert_eq!(
        h.calendars(&owner, &talent).await["links"],
        json!([]),
        "expired links are not listed"
    );

    // Removing the talent disconnects their calendars.
    h.app
        .delete(&format!("/api/v1/talent/{talent}"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    let live: i64 =
        sqlx::query_scalar("SELECT count(*) FROM talent_calendars WHERE disconnected_at IS NULL")
            .fetch_one(&h.app.owner)
            .await
            .unwrap();
    assert_eq!(live, 0);
    let kept: i64 =
        sqlx::query_scalar("SELECT count(*) FROM talent_calendars WHERE length(secret) > 0")
            .fetch_one(&h.app.owner)
            .await
            .unwrap();
    assert_eq!(kept, 0, "the iCal link is forgotten");
}
