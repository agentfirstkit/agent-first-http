//! Integration test: the two takeover backends — `brave` (default) and
//! `chrome` — launch and report the backend family that takeover discovery
//! keys on.
//!
//! `validate_hard_site_capabilities` decides whether a running host is
//! takeover-ready by looking up `backend.family` in the takeover table. If a
//! backend's family string ever drifts from its `--browser` name, that lookup
//! fails and `fetch --takeover` refuses a host that is in fact fine — with an
//! error blaming the host rather than the mismatch.
//!
//! WebGL coverage for these backends lives in `display_takeover.rs` instead,
//! because the defect it guards only reproduces headful: under `--headless=new`
//! Chromium serves WebGL whether or not `--disable-gpu` is passed, so a
//! headless assertion here passes with the fix reverted and guards nothing.
//!
//! Neither backend is arch-limited — Brave and Chrome both publish amd64 and
//! arm64 Debian packages and `Dockerfile.test` installs both — so these tests
//! run everywhere the suite runs instead of self-skipping into a false pass.

#![cfg(feature = "host")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods,
    clippy::disallowed_macros,
    clippy::print_stdout
)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agent_first_http::host::bootstrap::{
    BrowserChoice, DisplayMode, HealthPublic, HostArgs, ProfileChoice, Takeover,
};
use agent_first_http::host::{browser, listener::router_for_tests, listener::test_state};
use agent_first_http::sdk::Client;
use tokio::net::TcpListener;

async fn spawn_takeover_host(
    browser_choice: BrowserChoice,
    bin: PathBuf,
) -> (String, tempfile::TempDir) {
    support::ensure_rustls_provider();
    let args = HostArgs {
        listen: "tcp:127.0.0.1:0".into(),
        profile: ProfileChoice::Ephemeral,
        display: DisplayMode::Headless,
        takeover: Takeover::Off,
        display_quality: 100,
        browser: browser_choice,
        browser_bin: Some(bin),
        token: None,
        takeover_enabled: true,
        health_enabled: true,
        health_public: HealthPublic::Off,
        engine_envs: Vec::new(),
        browser_args: Vec::new(),
        proxy: None,
        recent_requests_cap: 0,
    };
    let handle = browser::launch(&args)
        .await
        .expect("takeover browser launch");
    let state = test_state(None, HealthPublic::Off).with_default_browser(Arc::new(handle));
    let app = router_for_tests(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    (format!("ws://{addr}"), tempfile::tempdir().expect("tmpdir"))
}

/// Every takeover backend must report the family that the takeover table looks
/// its `--browser` name up by, or takeover discovery rejects a healthy host.
async fn assert_family_matches_browser_name(browser_choice: BrowserChoice, expected: &str) {
    let bin = match expected {
        "brave" => support::env::discover_brave(),
        "chrome" => support::env::discover_chrome(),
        other => panic!("unknown takeover backend {other}"),
    }
    .unwrap_or_else(|| {
        panic!(
            "{expected} is a takeover backend and must be present in the test image; \
             set AFHTTP_TEST_{}_BIN if it lives elsewhere",
            expected.to_uppercase()
        )
    });

    let (endpoint, _tmp) = spawn_takeover_host(browser_choice, bin).await;
    let client = Client::connect(&endpoint).expect("client");
    let caps = client.capabilities().await.expect("capabilities");
    assert_eq!(
        caps.backend.family, expected,
        "takeover discovery keys on backend.family; drift here silently breaks `fetch --takeover`"
    );
    assert!(
        caps.artifacts["screenshot"].supported,
        "a takeover backend must render: it drives a display a human looks at"
    );
}

#[tokio::test]
async fn brave_reports_its_family_for_takeover_discovery() {
    assert_family_matches_browser_name(BrowserChoice::Brave, "brave").await;
}

#[tokio::test]
async fn chrome_reports_its_family_for_takeover_discovery() {
    assert_family_matches_browser_name(BrowserChoice::Chrome, "chrome").await;
}
