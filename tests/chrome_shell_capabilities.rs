//! Integration test: `--browser chrome_shell` launches chrome-headless-shell
//! and exposes the same capability matrix as the default chromium backend.
//! Gated on `AFHTTP_TEST_CHROME_SHELL_BIN` (or the standard install path);
//! Dockerfile.test pre-installs it on linux64.

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
use agent_first_http::sdk::fetch::RenderMode;
use agent_first_http::sdk::Client;
use agent_first_http::shared::artifacts::Artifact;
use tokio::net::TcpListener;

async fn spawn_host_with_chrome_shell() -> Option<(String, tempfile::TempDir)> {
    support::ensure_rustls_provider();
    let bin = support::env::discover_chrome_shell()?;
    let args = HostArgs {
        listen: "tcp:127.0.0.1:0".into(),
        profile: ProfileChoice::Ephemeral,
        display: DisplayMode::Headless,
        takeover: Takeover::Off,
        display_quality: 100,
        browser: BrowserChoice::ChromeShell,
        browser_bin: Some(bin),
        token: None,
        ops_enabled: true,
        health_enabled: true,
        health_public: HealthPublic::Off,
        engine_envs: Vec::new(),
        browser_args: Vec::new(),
        proxy: None,
        recent_requests_cap: 0,
    };
    let handle = browser::launch(&args).await.expect("chrome-shell launch");
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
async fn chrome_shell_exposes_full_capability_matrix() {
    let Some((endpoint, _tmp)) = spawn_host_with_chrome_shell().await else {
        println!("(skipping: no chrome-headless-shell binary; set AFHTTP_TEST_CHROME_SHELL_BIN)");
        return;
    };
    let client = Client::connect(&endpoint).expect("client");
    let caps = client.capabilities().await.expect("capabilities");
    // chrome-headless-shell is the same engine as chromium; full matrix expected.
    assert!(
        caps.artifacts["body"].supported,
        "body artifact must be supported on chrome-headless-shell",
    );
    assert!(
        caps.artifacts["rendered_html"].supported,
        "rendered_html must be supported on chrome-headless-shell",
    );
    assert!(
        caps.artifacts["screenshot"].supported,
        "screenshot must be supported on chrome-headless-shell",
    );
    assert!(
        caps.artifacts["observation"].supported,
        "observation must be supported on chrome-headless-shell",
    );
    assert!(
        caps.artifacts["network"].supported,
        "network must be supported on chrome-headless-shell",
    );
}

#[tokio::test]
async fn chrome_shell_browser_fetch_returns_rendered_html() {
    let Some((endpoint, tmp)) = spawn_host_with_chrome_shell().await else {
        println!("(skipping: no chrome-headless-shell binary; set AFHTTP_TEST_CHROME_SHELL_BIN)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let url = format!("{}/plain.html", fixture.base_url());
    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch(url)
        .render(RenderMode::Always)
        .timeout(Duration::from_secs(15))
        .want([Artifact::RenderedHtml])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("browser fetch via chrome-headless-shell");
    assert_eq!(result.status, 200);
    let path = result
        .rendered_html_file
        .as_ref()
        .expect("rendered_html_file");
    let html = std::fs::read_to_string(path).expect("read rendered");
    assert!(
        html.contains("Hello"),
        "rendered HTML should include fixture body, got: {html}"
    );
}
