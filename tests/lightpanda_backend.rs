//! Integration test: `--browser lightpanda` spawns the real lightpanda
//! subprocess, discovers its CDP WS address, and the host reports the
//! lightpanda-family capability matrix (no screenshot, no screencast).
//! Gated on `AFHTTP_TEST_LIGHTPANDA_BIN`.

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
use agent_first_http::sdk::Client;
use agent_first_http::sdk::fetch::RenderMode;
use agent_first_http::shared::error::ErrorCode;
use tokio::net::TcpListener;

async fn spawn_host_with_lightpanda() -> Option<(String, tempfile::TempDir)> {
    support::ensure_rustls_provider();
    let bin = support::env::discover_lightpanda()?;
    let args = HostArgs {
        listen: "tcp:127.0.0.1:0".into(),
        profile: ProfileChoice::Ephemeral,
        display: DisplayMode::Headless,
        takeover: Takeover::Off,
        display_quality: 100,
        browser: BrowserChoice::Lightpanda,
        browser_bin: Some(bin),
        token: None,
        takeover_enabled: true,
        health_enabled: true,
        health_public: HealthPublic::Off,
        engine_envs: Vec::new(),
        browser_args: Vec::new(),
        proxy: None,
        recent_requests_cap: 0,
        tab_idle_timeout_s: 0,
    };
    let handle = browser::launch(&args).await.expect("lightpanda launch");
    assert_eq!(handle.family, "lightpanda");
    assert!(handle.ws_url.starts_with("ws://"));
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
async fn lightpanda_launches_and_exposes_subset_capability_matrix() {
    let Some((endpoint, _tmp)) = spawn_host_with_lightpanda().await else {
        support::env::require_backend_if_missing("lightpanda");
        println!("(skipping: no lightpanda binary; set AFHTTP_TEST_LIGHTPANDA_BIN)");
        return;
    };
    let client = Client::connect(&endpoint).expect("client");
    let caps = client.capabilities().await.expect("capabilities");
    // Lightpanda has no rendering pipeline: screenshot + screencast must be
    // marked unsupported. Body/text/network metadata should still be on.
    assert!(
        !caps.artifacts["screenshot"].supported,
        "screenshot must be unsupported on lightpanda",
    );
    assert!(
        caps.artifacts.contains_key("body"),
        "body capability entry must exist",
    );
}

#[tokio::test]
async fn lightpanda_serves_consecutive_browser_fetches() {
    let Some((endpoint, _tmp)) = spawn_host_with_lightpanda().await else {
        support::env::require_backend_if_missing("lightpanda");
        println!("(skipping: no lightpanda binary; set AFHTTP_TEST_LIGHTPANDA_BIN)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let client = Client::connect(&endpoint).expect("client");
    // Lightpanda holds one page at a time, so each fetch must close its page
    // before the next one can open.
    for attempt in 0..3 {
        let result = client
            .fetch(format!("{}/plain.html", fixture.base_url()))
            .render(RenderMode::Always)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .unwrap_or_else(|error| panic!("fetch {attempt}: {error:?}"));
        assert_eq!(result.status, 200, "fetch {attempt}");
    }
}

#[tokio::test]
async fn lightpanda_refuses_persistent_profile() {
    let Some(bin) = support::env::discover_lightpanda() else {
        support::env::require_backend_if_missing("lightpanda");
        println!("(skipping: no lightpanda binary; set AFHTTP_TEST_LIGHTPANDA_BIN)");
        return;
    };
    support::ensure_rustls_provider();
    let args = HostArgs {
        listen: "tcp:127.0.0.1:0".into(),
        profile: ProfileChoice::Persistent("e2e-lightpanda".into()),
        display: DisplayMode::Headless,
        takeover: Takeover::Off,
        display_quality: 100,
        browser: BrowserChoice::Lightpanda,
        browser_bin: Some(bin),
        token: None,
        takeover_enabled: true,
        health_enabled: true,
        health_public: HealthPublic::Off,
        engine_envs: Vec::new(),
        browser_args: Vec::new(),
        proxy: None,
        recent_requests_cap: 0,
        tab_idle_timeout_s: 0,
    };
    let err = browser::launch(&args).await.err().expect("expected error");
    assert_eq!(
        err.error_code,
        ErrorCode::BackendUnsupported,
        "persistent profile on lightpanda must surface backend_unsupported, got {err:?}",
    );
}
