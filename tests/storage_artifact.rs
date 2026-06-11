//! Integration tests for the opt-in `storage` artifact: localStorage,
//! sessionStorage, and IndexedDB database names captured via CDP, plus the
//! 256 KiB size-cap truncation branch.

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
use serde_json::Value;
use tokio::net::TcpListener;

async fn spawn_host_with_browser() -> Option<(String, tempfile::TempDir)> {
    support::ensure_rustls_provider();
    let bin = support::env::discover_browser()?;
    let args = HostArgs {
        listen: "tcp:127.0.0.1:0".into(),
        profile: ProfileChoice::Ephemeral,
        display: DisplayMode::Headless,
        takeover: Takeover::Off,
        display_quality: 100,
        browser: BrowserChoice::Chromium,
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
    let handle = browser::launch(&args).await.expect("browser launch");
    let state = test_state(None, HealthPublic::Off).with_default_browser(Arc::new(handle));
    let app = router_for_tests(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    Some((format!("ws://{addr}"), tempfile::tempdir().expect("tmp")))
}

fn read_json(path: &std::path::Path) -> Value {
    let s = std::fs::read_to_string(path).expect("read storage artifact");
    serde_json::from_str(&s).expect("parse storage json")
}

#[tokio::test]
async fn storage_artifact_captures_local_session_and_indexeddb() {
    let Some((endpoint, tmp)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch(format!("{}/storage.html", fixture.base_url()))
        .render(RenderMode::Always)
        .wait(Wait::Ms(200))
        .timeout(Duration::from_secs(10))
        .want([Artifact::Storage])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    let storage_path = result.storage_file.as_ref().expect("storage_file present");
    let snap = read_json(storage_path);

    assert_eq!(snap["schema_version"], 1);
    assert_eq!(
        snap["local_storage"]["ls_key"].as_str(),
        Some("ls_value"),
        "localStorage not captured: {snap}"
    );
    assert_eq!(
        snap["session_storage"]["ss_key"].as_str(),
        Some("ss_value"),
        "sessionStorage not captured: {snap}"
    );
    // IndexedDB names are best-effort (async open) — assert the field is the
    // expected array shape, which exercises the eval_str_array parse path.
    assert!(
        snap["indexed_db_names"].is_array(),
        "indexed_db_names should be an array: {snap}"
    );
    assert!(snap["truncated"].is_null());
}

#[tokio::test]
async fn storage_artifact_truncates_over_cap() {
    let Some((endpoint, tmp)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch(format!("{}/storage-large.html", fixture.base_url()))
        .render(RenderMode::Always)
        .wait(Wait::Ms(200))
        .timeout(Duration::from_secs(10))
        .want([Artifact::Storage])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    let storage_path = result.storage_file.as_ref().expect("storage_file present");
    let snap = read_json(storage_path);

    let note = &snap["truncated"];
    assert!(
        note.is_object(),
        "expected a truncation note for an over-cap payload: {snap}"
    );
    assert_eq!(note["cap_bytes"], 256 * 1024);
    assert!(
        note["reason"]
            .as_str()
            .unwrap_or("")
            .contains("exceeded cap"),
        "reason = {}",
        note["reason"]
    );
    // Values are dropped when truncated.
    assert!(snap["local_storage"].is_null());
    assert!(snap["session_storage"].is_null());
}
