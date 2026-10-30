//! What reaches the logs. Its own test binary, because it installs a log
//! subscriber that tests running beside it would disturb.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use core_api::{AppState, Config, router};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

/// Collects formatted log lines.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn public_link_tokens_stay_out_of_the_logs() {
    let logs = Captured::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    // The database is down, so the requests fail and are logged as errors.
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(200))
        .connect_lazy("postgres://nobody:nothing@127.0.0.1:1/none")
        .expect("lazy pool");
    let app = router(AppState::new(pool, Config::for_development()).expect("state"));
    // Every documented path that carries a token, too.
    let tokened: Vec<String> = core_api::openapi()
        .paths
        .paths
        .keys()
        .filter(|p| p.contains("{token}"))
        .map(|p| {
            let p = p.replace("{token}", "link-secret-789");
            if p.starts_with("/api/v1") {
                p
            } else {
                format!("/api/v1{p}")
            }
        })
        .collect();
    assert!(
        tokened.iter().any(|p| p.contains("/public/sign/")),
        "{tokened:?}"
    );
    let fixed = [
        ("GET", "/api/v1/public/quotes/quote-secret-123".to_owned()),
        (
            "POST",
            "/api/v1/public/quotes/quote-secret-123/accept".to_owned(),
        ),
        (
            "GET",
            "/api/v1/public/calendar/feed-secret-456.ics".to_owned(),
        ),
    ];
    for (method, path) in fixed
        .into_iter()
        .chain(tokened.into_iter().map(|p| ("GET", p)))
    {
        let req = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(r#"{"name":"Sam"}"#))
            .expect("request");
        app.clone().oneshot(req).await.expect("response");
    }

    let logs = String::from_utf8(logs.0.lock().expect("lock").clone()).expect("utf-8");
    assert!(logs.contains("/public/quotes/<redacted>/accept"), "{logs}");
    assert!(logs.contains("/public/calendar/<redacted>"), "{logs}");
    assert!(!logs.contains("quote-secret-123"), "{logs}");
    assert!(!logs.contains("feed-secret-456"), "{logs}");
    assert!(logs.contains("/public/sign/<redacted>"), "{logs}");
    assert!(logs.contains("/public/invoices/<redacted>/pay"), "{logs}");
    assert!(!logs.contains("link-secret-789"), "{logs}");
}
