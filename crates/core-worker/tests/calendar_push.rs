//! M6 bookings in members' own calendars, against stand-ins for Google
//! (OAuth, Calendar API) and Microsoft (OAuth, Graph): connecting, OnHold
//! making its own calendar and filling it, bookings changing and going,
//! one talent's bookings only, the calendar deleted by its owner, a grant
//! withdrawn, a member leaving and disconnecting.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../core-api/tests/common/mod.rs"]
mod common;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
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

/// A calendar OnHold made in an account.
#[derive(Default, Clone)]
struct Calendar {
    name: String,
    events: BTreeMap<String, Value>,
}

/// What the stand-ins hold and were asked.
#[derive(Default)]
struct World {
    /// Calendars by id, across both providers.
    calendars: HashMap<String, Calendar>,
    /// Which calendar each event is in.
    event_calendar: HashMap<String, String>,
    made: usize,
    /// Event writes (adds and changes) asked for.
    writes: usize,
    /// The Google grant was withdrawn.
    withdrawn: bool,
    revoked: Vec<String>,
    /// The next event added is kept but its answer is lost.
    lose_next_answer: bool,
    /// Outlook events by transaction id.
    transactions: HashMap<String, String>,
    /// Grants also carry mailbox access given to the same app before.
    wide: bool,
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
        ("refresh_token", _) if !(w.withdrawn && !microsoft) => {}
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
        "Calendars.ReadWrite User.Read profile openid email"
    } else {
        "openid https://www.googleapis.com/auth/calendar.app.created https://www.googleapis.com/auth/userinfo.email"
    }
    .to_owned();
    if w.wide {
        scope.push_str(if microsoft {
            " Mail.ReadWrite"
        } else {
            " https://www.googleapis.com/auth/gmail.readonly"
        });
    }
    axum::Json(json!({
        "access_token": if microsoft { "ms-access" } else { "google-access" },
        "refresh_token": if microsoft { "ms-refresh" } else { "google-refresh" },
        // Expired at once, so every write refreshes first.
        "expires_in": if f["grant_type"] == "refresh_token" { 3599 } else { 0 },
        "scope": scope,
        "token_type": "Bearer",
    }))
    .into_response()
}

fn gone() -> Response {
    (
        StatusCode::NOT_FOUND,
        axum::Json(json!({ "error": { "code": 404, "message": "Not Found" } })),
    )
        .into_response()
}

fn refused(w: &World, headers: &HeaderMap) -> Option<Response> {
    let bearer = headers["authorization"].to_str().unwrap();
    assert!(
        bearer == "Bearer google-access" || bearer == "Bearer ms-access",
        "{bearer}"
    );
    (w.withdrawn && bearer == "Bearer google-access").then(|| {
        (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({ "error": { "code": 401, "message": "Invalid Credentials" } })),
        )
            .into_response()
    })
}

fn make_calendar(w: &mut World, name: &str) -> String {
    w.made += 1;
    let id = format!("cal-{}", w.made);
    w.calendars.insert(
        id.clone(),
        Calendar {
            name: name.to_owned(),
            events: BTreeMap::new(),
        },
    );
    id
}

fn answer(w: &mut World, id: &str) -> Response {
    if std::mem::take(&mut w.lose_next_answer) {
        return (StatusCode::BAD_GATEWAY, "lost").into_response();
    }
    axum::Json(json!({ "id": id })).into_response()
}

/// Adds an event. Google takes the id OnHold gives, and refuses one used
/// before in that calendar, even if since deleted, as Google does.
fn add_event(w: &mut World, cal: &str, mut body: Value) -> Response {
    if !w.calendars.contains_key(cal) {
        return gone();
    }
    let id = match body.get("id").and_then(Value::as_str) {
        Some(given) => {
            if w.event_calendar.get(given).is_some_and(|c| c == cal) {
                return (
                    StatusCode::CONFLICT,
                    axum::Json(json!({ "error": { "code": 409, "message": "The requested identifier already exists." } })),
                )
                    .into_response();
            }
            given.to_owned()
        }
        None => format!("ev-{}", w.event_calendar.len() + 1),
    };
    // Outlook answers a repeated transaction with the event already added.
    if let Some(txn) = body.get("transactionId").and_then(Value::as_str) {
        if let Some(known) = w.transactions.get(txn).cloned()
            && w.calendars[cal].events.contains_key(&known)
        {
            return answer(w, &known);
        }
        w.transactions.insert(txn.to_owned(), id.clone());
    }
    body.as_object_mut().unwrap().remove("transactionId");
    w.calendars
        .get_mut(cal)
        .unwrap()
        .events
        .insert(id.clone(), body);
    w.event_calendar.insert(id.clone(), cal.to_owned());
    w.writes += 1;
    answer(w, &id)
}

/// Changes an event; Google also brings back one deleted (`revive`).
fn change_event(w: &mut World, id: &str, body: Value, revive: bool) -> Response {
    let Some(cal) = w.event_calendar.get(id).cloned() else {
        return gone();
    };
    let Some(c) = w.calendars.get_mut(&cal) else {
        return gone();
    };
    if !revive && !c.events.contains_key(id) {
        return gone();
    }
    c.events.insert(id.to_owned(), body);
    w.writes += 1;
    axum::Json(json!({ "id": id })).into_response()
}

/// Removes an event; one already deleted is 404 for Outlook and 410 for
/// Google.
fn remove_event(w: &mut World, id: &str, google: bool) -> StatusCode {
    let removed = w
        .event_calendar
        .get(id)
        .and_then(|cal| w.calendars.get_mut(cal))
        .and_then(|c| c.events.remove(id));
    match (removed, google && w.event_calendar.contains_key(id)) {
        (Some(_), _) => StatusCode::NO_CONTENT,
        (None, true) => StatusCode::GONE,
        (None, false) => StatusCode::NOT_FOUND,
    }
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
            get(|| async {
                axum::Json(json!({ "sub": "1", "email": "Owner@Gmail.test", "email_verified": true }))
            }),
        )
        .route(
            "/v1.0/me",
            get(|| async {
                axum::Json(json!({ "mail": "Ana@Contoso.test", "userPrincipalName": "ana@contoso.test" }))
            }),
        )
        // Google.
        .route(
            "/calendar/v3/calendars",
            post(
                |State(w): State<Shared>, headers: HeaderMap, body: axum::Json<Value>| async move {
                    let mut w = w.lock().unwrap();
                    if let Some(r) = refused(&w, &headers) {
                        return r;
                    }
                    assert!(body["timeZone"].is_string());
                    let id = make_calendar(&mut w, body["summary"].as_str().unwrap());
                    axum::Json(json!({ "id": id })).into_response()
                },
            ),
        )
        .route(
            "/calendar/v3/calendars/{cal}",
            delete(
                |State(w): State<Shared>, Path(cal): Path<String>| async move {
                    match w.lock().unwrap().calendars.remove(&cal) {
                        Some(_) => StatusCode::NO_CONTENT,
                        None => StatusCode::NOT_FOUND,
                    }
                },
            ),
        )
        .route(
            "/calendar/v3/calendars/{cal}/events",
            post(
                |State(w): State<Shared>,
                 headers: HeaderMap,
                 Path(cal): Path<String>,
                 body: axum::Json<Value>| async move {
                    let mut w = w.lock().unwrap();
                    if let Some(r) = refused(&w, &headers) {
                        return r;
                    }
                    add_event(&mut w, &cal, body.0)
                },
            ),
        )
        .route(
            "/calendar/v3/calendars/{cal}/events/{id}",
            put(
                |State(w): State<Shared>,
                 Path((cal, id)): Path<(String, String)>,
                 body: axum::Json<Value>| async move {
                    let mut w = w.lock().unwrap();
                    if !w.calendars.contains_key(&cal) {
                        return gone();
                    }
                    change_event(&mut w, &id, body.0, true)
                },
            )
            .delete(
                |State(w): State<Shared>, Path((_cal, id)): Path<(String, String)>| async move {
                    remove_event(&mut w.lock().unwrap(), &id, true)
                },
            ),
        )
        // Microsoft.
        .route(
            "/v1.0/me/calendars",
            post(
                |State(w): State<Shared>, headers: HeaderMap, body: axum::Json<Value>| async move {
                    let mut w = w.lock().unwrap();
                    if let Some(r) = refused(&w, &headers) {
                        return r;
                    }
                    let name = body["name"].as_str().unwrap();
                    if w.calendars.values().any(|c| c.name == name) {
                        return (
                            StatusCode::CONFLICT,
                            axum::Json(json!({ "error": { "code": "ErrorFolderExists", "message": "A folder with the specified name already exists." } })),
                        )
                            .into_response();
                    }
                    let id = make_calendar(&mut w, name);
                    axum::Json(json!({ "id": id })).into_response()
                },
            )
            .get(
                |State(w): State<Shared>, Query(q): Query<HashMap<String, String>>| async move {
                    let w = w.lock().unwrap();
                    let filter = &q["$filter"];
                    let name = filter
                        .strip_prefix("name eq '")
                        .and_then(|f| f.strip_suffix('\''))
                        .unwrap()
                        .replace("''", "'");
                    let value: Vec<Value> = w
                        .calendars
                        .iter()
                        .filter(|(_, c)| c.name == name)
                        .map(|(id, _)| json!({ "id": id }))
                        .collect();
                    axum::Json(json!({ "value": value }))
                },
            ),
        )
        .route(
            "/v1.0/me/calendars/{cal}",
            delete(
                |State(w): State<Shared>, Path(cal): Path<String>| async move {
                    match w.lock().unwrap().calendars.remove(&cal) {
                        Some(_) => StatusCode::NO_CONTENT,
                        None => StatusCode::NOT_FOUND,
                    }
                },
            ),
        )
        .route(
            "/v1.0/me/calendars/{cal}/events",
            post(
                |State(w): State<Shared>, Path(cal): Path<String>, body: axum::Json<Value>| async move {
                    add_event(&mut w.lock().unwrap(), &cal, body.0)
                },
            ),
        )
        .route(
            "/v1.0/me/events/{id}",
            patch(
                |State(w): State<Shared>, Path(id): Path<String>, body: axum::Json<Value>| async move {
                    change_event(&mut w.lock().unwrap(), &id, body.0, false)
                },
            )
            .delete(
                |State(w): State<Shared>, Path(id): Path<String>| async move {
                    remove_event(&mut w.lock().unwrap(), &id, false)
                },
            ),
        )
        .with_state(world.clone());
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
    Harness { app, worker, world }
}

impl Harness {
    async fn drain(&self) {
        for _ in 0..50 {
            // Writes that met another one running wait for the sweep.
            let swept = self.worker.schedule_once().await.unwrap();
            let published = self.worker.publish_once().await.unwrap();
            let ran = self.worker.run_jobs_once().await.unwrap();
            if swept.unwrap_or(0) == 0 && published == 0 && ran == 0 {
                return;
            }
        }
        panic!("the queue did not settle");
    }

    async fn pushes(&self, who: &Client) -> Value {
        self.app
            .get("/api/v1/calendar/pushes")
            .signed_in(who)
            .send()
            .await
            .ok()
            .body
    }

    /// Connects through Google or Microsoft as a member would.
    async fn connect(&self, who: &Client, provider: &str, talent: Option<&str>) -> Value {
        self.try_connect(who, provider, talent).await.ok().body
    }

    async fn try_connect(
        &self,
        who: &Client,
        provider: &str,
        talent: Option<&str>,
    ) -> common::Resp {
        let start = self
            .app
            .post("/api/v1/calendar/pushes/start")
            .signed_in(who)
            .json(json!({ "provider": provider, "talent_id": talent }))
            .send()
            .await
            .ok()
            .body;
        let url = Url::parse(start["url"].as_str().unwrap()).unwrap();
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert!(q["redirect_uri"].ends_with("/settings/calendar"), "{url}");
        assert!(!q.contains_key("include_granted_scopes"), "{url}");
        if provider == "google" {
            assert!(q["scope"].contains("calendar.app.created"), "{url}");
            assert!(!q["scope"].contains("auth/calendar "), "{url}");
        }
        self.app
            .post("/api/v1/calendar/pushes/finish")
            .signed_in(who)
            .json(json!({ "code": "good-code", "state": q["state"] }))
            .send()
            .await
    }

    async fn sync(&self, who: &Client, id: &str) {
        self.app
            .post(&format!("/api/v1/calendar/pushes/{id}/sync"))
            .signed_in(who)
            .send()
            .await
            .ok();
        self.drain().await;
    }

    async fn booking(&self, who: &Client, title: &str, talent: &str, days: i64) -> String {
        self.app
            .post("/api/v1/bookings")
            .signed_in(who)
            .json(json!({ "title": title, "talent_ids": [talent], "fee": { "minor": 987_654, "currency": "GBP" },
                "starts_at": stamp(ahead(days, 19)), "ends_at": stamp(ahead(days, 23)) }))
            .send()
            .await
            .ok()
            .id("/id")
            .to_string()
    }

    async fn set_status(&self, who: &Client, booking: &str, status: &str) {
        self.app
            .post(&format!("/api/v1/bookings/{booking}/status"))
            .signed_in(who)
            .json(json!({ "status": status }))
            .send()
            .await
            .ok();
    }

    /// The events in a calendar, by their title: (status or showAs, body).
    fn events(&self, cal: &str) -> BTreeMap<String, Value> {
        let w = self.world.lock().unwrap();
        w.calendars[cal]
            .events
            .values()
            .map(|e| {
                let title = e
                    .get("summary")
                    .or_else(|| e.get("subject"))
                    .and_then(Value::as_str)
                    .unwrap()
                    .to_owned();
                (title, e.clone())
            })
            .collect()
    }

    fn calendar_named(&self, part: &str) -> Option<String> {
        let w = self.world.lock().unwrap();
        w.calendars
            .iter()
            .find(|(_, c)| c.name.contains(part))
            .map(|(id, _)| id.clone())
    }
}

fn ahead(days: i64, hour: i64) -> DateTime<Utc> {
    Utc::now().duration_trunc(Span::days(1)).unwrap() + Span::days(days) + Span::hours(hour)
}

fn title_with(events: &BTreeMap<String, Value>, part: &str) -> Value {
    events
        .iter()
        .find(|(t, _)| t.contains(part))
        .map(|(_, e)| e.clone())
        .unwrap_or_else(|| panic!("no event for {part}: {events:?}"))
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn bookings_are_kept_in_members_own_calendars(pool: PgPool) {
    let h = harness(pool).await;
    let (_ws, owner) = h.app.workspace_with_owner("owls").await;
    let talent = |name: &'static str| {
        let h = &h;
        let owner = &owner;
        async move {
            h.app
                .post("/api/v1/talent")
                .signed_in(owner)
                .json(json!({ "name": name, "kind": "act" }))
                .send()
                .await
                .ok()
                .id("/id")
                .to_string()
        }
    };
    let owls = talent("The Night Owls").await;
    let larks = talent("The Larks").await;
    let ball = h.booking(&owner, "Summer Ball", &owls, 3).await;
    let gala = h.booking(&owner, "Charity Gala", &owls, 5).await;
    h.set_status(&owner, &gala, "confirmed").await;
    let fete = h.booking(&owner, "Village Fete", &larks, 7).await;
    let old = h.booking(&owner, "Long Gone", &owls, -90).await;
    h.drain().await;

    let empty = h.pushes(&owner).await;
    assert_eq!(empty["calendars"], json!([]));
    assert_eq!(empty["google"], true);

    // A grant that also carries the mailbox is refused.
    h.world.lock().unwrap().wide = true;
    h.try_connect(&owner, "google", None)
        .await
        .expect(StatusCode::BAD_REQUEST, "scopes_too_wide");
    h.world.lock().unwrap().wide = false;

    // The owner connects Google: OnHold makes its calendar and fills it.
    let g = h.connect(&owner, "google", None).await;
    assert_eq!(g["account"], "owner@gmail.test");
    assert_eq!(g["provider"], "google");
    let gid = g["id"].as_str().unwrap().to_owned();
    h.drain().await;
    let gcal = h.calendar_named("OnHold").expect("OnHold's calendar");
    let events = h.events(&gcal);
    assert_eq!(events.len(), 3, "{events:?}");
    let ball_event = title_with(&events, "Summer Ball");
    assert_eq!(ball_event["status"], "tentative");
    assert_eq!(title_with(&events, "Charity Gala")["status"], "confirmed");
    assert!(title_with(&events, "Village Fete").is_object());
    assert!(!events.keys().any(|t| t.contains("Long Gone")));
    // Never the fee.
    let all = serde_json::to_string(&events).unwrap();
    assert!(!all.contains("9876") && !all.contains("987"), "{all}");
    assert!(
        ball_event["description"]
            .as_str()
            .unwrap()
            .contains(&format!("/bookings/{ball}"))
    );
    let listed = h.pushes(&owner).await;
    assert_eq!(listed["calendars"][0]["status"], "active");
    assert!(listed["calendars"][0]["last_synced_at"].is_string());

    // A booking confirmed: written straight away.
    let writes = h.world.lock().unwrap().writes;
    h.set_status(&owner, &ball, "confirmed").await;
    h.drain().await;
    assert_eq!(
        title_with(&h.events(&gcal), "Summer Ball")["status"],
        "confirmed"
    );
    assert_eq!(h.world.lock().unwrap().writes, writes + 1);

    // Nothing changed: a full comparison writes nothing.
    h.sync(&owner, &gid).await;
    assert_eq!(h.world.lock().unwrap().writes, writes + 1);

    // An answer lost after Google added the event: trying again writes
    // over that event rather than adding a second.
    h.world.lock().unwrap().lose_next_answer = true;
    let lost = h.booking(&owner, "Lost Answer", &owls, 6).await;
    h.drain().await;
    h.sync(&owner, &gid).await;
    let events = h.events(&gcal);
    assert_eq!(events.len(), 4, "{events:?}");
    assert!(title_with(&events, "Lost Answer").is_object());
    // Counted by id, not title: no second copy.
    assert_eq!(h.world.lock().unwrap().calendars[&gcal].events.len(), 4);
    h.set_status(&owner, &lost, "cancelled").await;

    // A booking cancelled or removed: its event goes, even one already
    // deleted in Google (which answers 410).
    let gala_event = {
        let w = h.world.lock().unwrap();
        w.calendars[&gcal]
            .events
            .iter()
            .find(|(_, e)| e["summary"].as_str().unwrap().contains("Charity Gala"))
            .map(|(id, _)| id.clone())
            .unwrap()
    };
    h.world
        .lock()
        .unwrap()
        .calendars
        .get_mut(&gcal)
        .unwrap()
        .events
        .remove(&gala_event);
    h.set_status(&owner, &gala, "cancelled").await;
    h.app
        .delete(&format!("/api/v1/bookings/{fete}"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    h.drain().await;
    let events = h.events(&gcal);
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(events.keys().next().unwrap().contains("Summer Ball"));
    let tracked: i64 = sqlx::query_scalar("SELECT count(*) FROM calendar_push_events")
        .fetch_one(&h.app.owner)
        .await
        .unwrap();
    assert_eq!(tracked, 1, "nothing left failing to be removed");
    assert_eq!(h.pushes(&owner).await["calendars"][0]["status"], "active");
    let _ = old;

    // The owner deletes OnHold's calendar in Google: it is made again.
    h.world.lock().unwrap().calendars.remove(&gcal);
    let fresh = h.booking(&owner, "New Year", &owls, 9).await;
    h.drain().await;
    let again = h.calendar_named("OnHold").expect("made again");
    assert_ne!(again, gcal);
    let events = h.events(&again);
    assert_eq!(events.len(), 2, "{events:?}");
    let _ = fresh;

    // An agent connects Outlook for one talent only.
    let ana = h.app.add_member(&owner, "ana@owls.test", "agent").await;
    let m = h.connect(&ana, "microsoft", Some(&larks)).await;
    assert_eq!(m["account"], "ana@contoso.test");
    assert_eq!(m["talent_name"], "The Larks");
    let mid = m["id"].as_str().unwrap().to_owned();
    h.booking(&owner, "Larks at the Pier", &larks, 11).await;
    h.drain().await;
    let mcal = h.calendar_named("The Larks").expect("Ana's calendar");
    let events = h.events(&mcal);
    assert_eq!(events.len(), 1, "{events:?}");
    let pier = title_with(&events, "Larks at the Pier");
    assert_eq!(pier["showAs"], "tentative");
    assert_eq!(pier["start"]["timeZone"], "UTC");
    // Each member sees only their own.
    assert_eq!(
        h.pushes(&ana).await["calendars"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        h.pushes(&owner).await["calendars"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    h.app
        .post(&format!("/api/v1/calendar/pushes/{mid}/sync"))
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::NOT_FOUND, "not_found");

    // OnHold loses track of its Outlook calendar (as after a clean-up that
    // did not finish): Outlook refuses a second of the same name, so the
    // one there is taken over, and its event is not added twice.
    sqlx::query("UPDATE calendar_pushes SET remote_calendar = NULL WHERE id = $1::uuid")
        .bind(&mid)
        .execute(&h.app.owner)
        .await
        .unwrap();
    sqlx::query("DELETE FROM calendar_push_events WHERE push_id = $1::uuid")
        .bind(&mid)
        .execute(&h.app.owner)
        .await
        .unwrap();
    h.sync(&ana, &mid).await;
    assert_eq!(h.calendar_named("The Larks"), Some(mcal.clone()));
    assert_eq!(h.world.lock().unwrap().calendars[&mcal].events.len(), 1);
    assert_eq!(h.pushes(&ana).await["calendars"][0]["status"], "active");

    // The Google grant is withdrawn: the calendar needs connecting again.
    h.world.lock().unwrap().withdrawn = true;
    h.booking(&owner, "Spring Fair", &owls, 13).await;
    h.drain().await;
    let listed = h.pushes(&owner).await;
    assert_eq!(listed["calendars"][0]["status"], "reconnect", "{listed}");
    h.app
        .post(&format!("/api/v1/calendar/pushes/{gid}/sync"))
        .signed_in(&owner)
        .send()
        .await
        .expect(StatusCode::CONFLICT, "reconnect_required");
    // Connecting again revives the same connection.
    h.world.lock().unwrap().withdrawn = false;
    let back = h.connect(&owner, "google", None).await;
    assert_eq!(back["id"], gid.as_str());
    assert_eq!(back["status"], "active");
    h.drain().await;
    let events = h.events(&again);
    // Summer Ball, New Year, Larks at the Pier and Spring Fair.
    assert_eq!(events.len(), 4, "{events:?}");
    assert_eq!(title_with(&events, "Spring Fair")["status"], "tentative");

    // Ana leaves: her calendar is removed and the grant forgotten.
    let ana_id = h.app.user_id("ana@owls.test").await;
    h.app
        .delete(&format!("/api/v1/members/{}", ana_id.0))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    h.worker.schedule_once().await.unwrap();
    h.drain().await;
    assert!(!h.world.lock().unwrap().calendars.contains_key(&mcal));
    let left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM calendar_pushes WHERE length(secret) > 0 OR revoke_at IS NOT NULL",
    )
    .fetch_one(&h.app.owner)
    .await
    .unwrap();
    assert_eq!(left, 1, "only the owner's connection keeps its grant");

    // The owner disconnects: OnHold's calendar goes and Google is asked to
    // forget the grant.
    h.app
        .delete(&format!("/api/v1/calendar/pushes/{gid}"))
        .signed_in(&owner)
        .send()
        .await
        .ok();
    h.drain().await;
    assert!(!h.world.lock().unwrap().calendars.contains_key(&again));
    assert!(
        h.world
            .lock()
            .unwrap()
            .revoked
            .contains(&"google-refresh".to_owned())
    );
    assert_eq!(h.pushes(&owner).await["calendars"], json!([]));
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM calendar_push_events")
        .fetch_one(&h.app.owner)
        .await
        .unwrap();
    assert_eq!(events, 0);
    // Bookings changing afterwards write nothing.
    let writes = h.world.lock().unwrap().writes;
    h.booking(&owner, "Too Late", &owls, 15).await;
    h.drain().await;
    assert_eq!(h.world.lock().unwrap().writes, writes);
}
