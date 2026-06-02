//! End-to-end test for `afhttp tabs list` and `afhttp tabs close`. The
//! CLI subcommand surface is thin — just `Client.cdp("Target.getTargets")`
//! / `Target.closeTarget` — so we test through the SDK directly.

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
use tokio::net::TcpListener;

async fn spawn_chromium_host() -> Option<(String, tempfile::TempDir)> {
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
        ops_enabled: true,
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
    let tmp = tempfile::tempdir().expect("tmpdir");
    Some((format!("ws://{addr}"), tmp))
}

#[tokio::test]
async fn target_get_close_round_trip() {
    let Some((endpoint, _tmp)) = spawn_chromium_host().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let client = Client::connect(&endpoint).expect("client");

    // Open a fresh blank target via raw CDP so we keep a stable id without
    // depending on fetch lifecycle. Chromium returns {targetId} on create.
    let create = client
        .cdp("Target.createTarget")
        .params(serde_json::json!({ "url": "about:blank" }))
        .send()
        .await
        .expect("Target.createTarget");
    // Connection::send unwraps the JSON-RPC envelope's `result` field,
    // so `create` is the inner method result, not the full message.
    let target_id = create["targetId"]
        .as_str()
        .expect("targetId in createTarget response")
        .to_string();

    // The new target must appear in Target.getTargets — this is what
    // `afhttp tabs list` surfaces.
    let listed = client
        .cdp("Target.getTargets")
        .send()
        .await
        .expect("Target.getTargets");
    let targets = listed["targetInfos"].as_array().expect("targetInfos array");
    assert!(
        targets
            .iter()
            .any(|t| t["targetId"].as_str() == Some(target_id.as_str())),
        "target {target_id} missing from getTargets; got {targets:?}",
    );

    // Close it via Target.closeTarget — this is what `afhttp tabs close`
    // wraps. CDP returns {success: bool}.
    let close = client
        .cdp("Target.closeTarget")
        .params(serde_json::json!({ "targetId": target_id.clone() }))
        .send()
        .await
        .expect("Target.closeTarget");
    assert_eq!(close["success"], serde_json::Value::Bool(true));

    // Subsequent list must not contain the closed target. Allow a small
    // wait so chromium can process the close before we re-list.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let after = client
        .cdp("Target.getTargets")
        .send()
        .await
        .expect("Target.getTargets after close");
    let after_targets = after["targetInfos"]
        .as_array()
        .expect("after targetInfos array");
    assert!(
        !after_targets
            .iter()
            .any(|t| t["targetId"].as_str() == Some(target_id.as_str())),
        "target {target_id} still listed after close",
    );
}
