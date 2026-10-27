//! Printing with a real browser. Runs wherever Chrome, Chromium or Edge is
//! installed (or `CHROMIUM_PATH` names one); CI sets
//! `ONHOLD_REQUIRE_CHROMIUM=1` so a missing browser fails instead of
//! passing quietly.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use core_pdf::write::{Chromium, Renderer};

fn browser() -> Option<Chromium> {
    let found = match std::env::var_os("CHROMIUM_PATH") {
        Some(path) => Some(Chromium::new(path)),
        None => Chromium::find(),
    };
    if found.is_none() && std::env::var("ONHOLD_REQUIRE_CHROMIUM").as_deref() == Ok("1") {
        panic!("ONHOLD_REQUIRE_CHROMIUM is set but no browser was found");
    }
    found.map(|mut c| {
        // Containers run tests as root, where Chromium's sandbox cannot start.
        c.no_sandbox = true;
        c
    })
}

#[test]
fn prints_html_and_reaches_nothing_outside() {
    let Some(chromium) = browser() else {
        eprintln!("no browser installed; skipping");
        return;
    };
    let html = r#"<!doctype html><html><head><meta charset="utf-8">
        <style>h1 { color: #b00 }</style></head>
        <body><h1>Advance sheet</h1><p>Doors 19:00 · Fee £2,000.00</p>
        <img src="http://169.254.169.254/latest/meta-data/">
        <iframe src="file:///etc/passwd"></iframe>
        <script>document.body.innerHTML = 'script ran'</script></body></html>"#;
    let pdf = chromium.render(html, "Advance sheet").expect("prints");
    let read = core_pdf::extract(&pdf, &core_pdf::Limits::default()).expect("reads");
    let text = read.text();
    assert!(text.contains("Advance sheet"), "{text}");
    assert!(text.contains("£2,000.00"), "{text}");
    assert!(!text.contains("script ran"), "{text}");
    assert!(!text.contains("root:"), "{text}");
}
