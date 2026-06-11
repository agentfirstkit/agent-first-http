//! Integration test: `--browser fingerprint_chromium` launches the
//! upstream Ungoogled-Chromium fingerprint fork, derives a stable seed
//! from the resolved profile path, and the resulting `navigator.userAgent`
//! is consistent across two browser fetches in the same host (proving
//! the deterministic seed wired through).
//!
//! Gated on `AFHTTP_TEST_FINGERPRINT_CHROMIUM_BIN`; Dockerfile.test
//! installs the linux x86_64 release. arm64 self-skips because upstream
//! does not publish arm64 binaries.

#![cfg(feature = "host")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods,
    clippy::disallowed_macros,
    clippy::err_expect,
    clippy::print_stdout,
    clippy::useless_conversion
)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use agent_first_http::host::bootstrap::{
    BrowserChoice, DisplayMode, HealthPublic, HostArgs, ProfileChoice, Takeover,
};
use agent_first_http::host::{browser, listener::router_for_tests, listener::test_state};
use agent_first_http::sdk::fetch::{RenderMode, Wait};
use agent_first_http::sdk::Client;
use agent_first_http::shared::artifacts::Artifact;
use tokio::net::TcpListener;

async fn spawn_host_with_fingerprint_chromium() -> Option<(String, tempfile::TempDir)> {
    support::ensure_rustls_provider();
    let bin = support::env::discover_fingerprint_chromium()?;
    let args = HostArgs {
        listen: "tcp:127.0.0.1:0".into(),
        profile: ProfileChoice::Ephemeral,
        display: DisplayMode::Headless,
        takeover: Takeover::Off,
        display_quality: 100,
        browser: BrowserChoice::FingerprintChromium,
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
        .expect("fingerprint-chromium launch");
    assert_eq!(handle.family, "fingerprint-chromium");
    let state = test_state(None, HealthPublic::Off).with_default_browser(Arc::new(handle));
    let app = router_for_tests(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let tmp = tempfile::tempdir().expect("tmpdir");
    Some((format!("ws://{addr}"), tmp))
}

#[tokio::test]
async fn fingerprint_chromium_reports_family_in_capabilities() {
    let Some((endpoint, _tmp)) = spawn_host_with_fingerprint_chromium().await else {
        println!(
            "(skipping: no fingerprint-chromium binary; set AFHTTP_TEST_FINGERPRINT_CHROMIUM_BIN)"
        );
        return;
    };
    let client = Client::connect(&endpoint).expect("client");
    let caps = client.capabilities().await.expect("capabilities");
    // The fingerprint fork is the same engine as chromium; the full
    // capability matrix should be available.
    assert!(caps.artifacts["body"].supported);
    assert!(caps.artifacts["rendered_html"].supported);
    assert!(caps.artifacts["screenshot"].supported);
    assert!(caps.artifacts["observation"].supported);
    assert_eq!(caps.backend.family, "fingerprint-chromium");
}

#[tokio::test]
async fn fingerprint_chromium_user_agent_is_stable_within_a_host() {
    let Some((endpoint, tmp)) = spawn_host_with_fingerprint_chromium().await else {
        println!(
            "(skipping: no fingerprint-chromium binary; set AFHTTP_TEST_FINGERPRINT_CHROMIUM_BIN)"
        );
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let client = Client::connect(&endpoint).expect("client");

    let fetch_user_agent = || async {
        let r = client
            .fetch(format!("{}/identity.html", fixture.base_url()))
            .render(RenderMode::Always)
            .wait(Wait::Load)
            .timeout(Duration::from_secs(15))
            .want([Artifact::RenderedHtml])
            .out_dir(tmp.path().to_path_buf())
            .send()
            .await
            .expect("fetch");
        let path = r
            .rendered_html_file
            .as_ref()
            .expect("rendered_html_file")
            .clone();
        std::fs::read_to_string(path).expect("read rendered")
    };

    let html_a = fetch_user_agent().await;
    let html_b = fetch_user_agent().await;

    // The identity.html fixture writes navigator.userAgent into the DOM
    // via JS. Two fetches against the same fingerprint-chromium host
    // must observe identical UA strings — that's the contract the
    // deterministic seed buys.
    let ua_a = extract_ua_token(&html_a);
    let ua_b = extract_ua_token(&html_b);
    assert!(!ua_a.is_empty(), "UA missing in first fetch: {html_a}");
    assert_eq!(ua_a, ua_b, "UA drifted between fetches");
}

/// Pull a representative chunk of `navigator.userAgent` out of the
/// rendered identity.html. We don't care about the exact spoofed value,
/// only that two captures match.
fn extract_ua_token(html: &str) -> String {
    let needle = "\"ua\":\"";
    let Some(start) = html.find(needle) else {
        return String::new();
    };
    let rest = &html[start + needle.len()..];
    let end = rest.find('"').unwrap_or(rest.len());
    rest[..end].to_string()
}
