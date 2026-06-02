//! Integration test: `--browser camoufox` spawns the foxbridge CDP→Juggler
//! proxy against the camoufox binary, exposes a CDP WebSocket on a
//! pre-reserved ephemeral port, and the host reports the subset-backend
//! capability matrix (no screenshot, no ops-panel screencast).
//!
//! Gated on BOTH `AFHTTP_TEST_FOXBRIDGE_BIN` and `AFHTTP_TEST_CAMOUFOX_BIN`.
//! Dockerfile.test best-effort installs foxbridge (via `go install`) and
//! the camoufox release tarball; this test self-skips if either binary
//! ends up missing — foxbridge upstream has no release binaries and the
//! go install can fail in restricted networks.

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
use agent_first_http::shared::error::ErrorCode;
use tokio::net::TcpListener;

fn skip_if_no_camoufox_stack() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let foxbridge = support::env::discover_foxbridge()?;
    let camoufox = support::env::discover_camoufox()?;
    Some((foxbridge, camoufox))
}

async fn spawn_host_with_camoufox() -> Option<(String, tempfile::TempDir)> {
    let (foxbridge, _camoufox) = skip_if_no_camoufox_stack()?;
    support::ensure_rustls_provider();
    let args = HostArgs {
        listen: "tcp:127.0.0.1:0".into(),
        profile: ProfileChoice::Ephemeral,
        display: DisplayMode::Headless,
        takeover: Takeover::Off,
        display_quality: 100,
        browser: BrowserChoice::Camoufox,
        // browser_bin pins the foxbridge binary; camoufox is auto-discovered
        // by resolve_named_bin inside launch_camoufox.
        browser_bin: Some(foxbridge),
        token: None,
        ops_enabled: true,
        health_enabled: true,
        health_public: HealthPublic::Off,
        engine_envs: Vec::new(),
        browser_args: Vec::new(),
        proxy: None,
        recent_requests_cap: 0,
    };
    let handle = browser::launch(&args).await.expect("camoufox launch");
    assert_eq!(handle.family, "camoufox");
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
async fn camoufox_launches_and_exposes_subset_capability_matrix() {
    let Some((endpoint, _tmp)) = spawn_host_with_camoufox().await else {
        println!(
            "(skipping: foxbridge or camoufox binary missing; set \
             AFHTTP_TEST_FOXBRIDGE_BIN and AFHTTP_TEST_CAMOUFOX_BIN)"
        );
        return;
    };
    let client = Client::connect(&endpoint).expect("client");
    let caps = client.capabilities().await.expect("capabilities");
    // Subset backend: no chromium-only screenshot / screencast.
    assert!(
        !caps.artifacts["screenshot"].supported,
        "screenshot must be unsupported on camoufox",
    );
    assert!(
        caps.artifacts.contains_key("body"),
        "body entry must exist on the capability matrix",
    );
    assert_eq!(caps.backend.family, "camoufox");
}

#[tokio::test]
async fn camoufox_refuses_persistent_profile() {
    let Some((foxbridge, _camoufox)) = skip_if_no_camoufox_stack() else {
        println!(
            "(skipping: foxbridge or camoufox binary missing; set \
             AFHTTP_TEST_FOXBRIDGE_BIN and AFHTTP_TEST_CAMOUFOX_BIN)"
        );
        return;
    };
    support::ensure_rustls_provider();
    let args = HostArgs {
        listen: "tcp:127.0.0.1:0".into(),
        profile: ProfileChoice::Persistent("e2e-camoufox".into()),
        display: DisplayMode::Headless,
        takeover: Takeover::Off,
        display_quality: 100,
        browser: BrowserChoice::Camoufox,
        browser_bin: Some(foxbridge),
        token: None,
        ops_enabled: true,
        health_enabled: true,
        health_public: HealthPublic::Off,
        engine_envs: Vec::new(),
        browser_args: Vec::new(),
        proxy: None,
        recent_requests_cap: 0,
    };
    let err = browser::launch(&args).await.err().expect("expected error");
    assert_eq!(
        err.error_code,
        ErrorCode::BackendUnsupported,
        "persistent profile on camoufox must surface backend_unsupported, got {err:?}",
    );
}
