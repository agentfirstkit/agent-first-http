//! Integration tests for the cdp escape hatch + multi-attach, the ops
//! panel routes, and inline_ephemeral.

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
use agent_first_http::sdk::{Client, InlineConfig};
use serde_json::json;
use tokio::net::TcpListener;

async fn spawn() -> Option<String> {
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
    Some(format!("ws://{addr}"))
}

#[tokio::test]
async fn cdp_escape_hatch_runs_browser_method() {
    let Some(endpoint) = spawn().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let client = Client::connect(&endpoint).expect("client");
    let resp = client.cdp("Browser.getVersion").send().await.expect("cdp");
    assert!(resp["product"].is_string(), "expected product, got {resp}");
}

#[tokio::test]
async fn client_close_drops_cached_cdp_connection_and_reconnects() {
    let Some(endpoint) = spawn().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let client = Client::connect(&endpoint).expect("client");
    let first = client
        .cdp("Browser.getVersion")
        .send()
        .await
        .expect("first");
    assert!(
        first["product"].is_string(),
        "expected product, got {first}"
    );

    client.close().await;

    let second = client
        .cdp("Browser.getVersion")
        .send()
        .await
        .expect("second");
    assert!(
        second["product"].is_string(),
        "expected product after reconnect, got {second}"
    );
}

#[tokio::test]
async fn multi_attach_two_clients_see_same_target() {
    let Some(endpoint) = spawn().await else {
        println!("(skipping: no chromium)");
        return;
    };
    // Open a fresh target via client A.
    let client_a = Client::connect(&endpoint).expect("client a");
    let target = client_a
        .cdp("Target.createTarget")
        .params(json!({"url": "about:blank"}))
        .send()
        .await
        .expect("createTarget");
    let target_id = target["targetId"].as_str().expect("targetId").to_string();

    // Client B should see the same target in Target.getTargets.
    let client_b = Client::connect(&endpoint).expect("client b");
    let targets = client_b
        .cdp("Target.getTargets")
        .send()
        .await
        .expect("getTargets");
    let arr = targets["targetInfos"]
        .as_array()
        .expect("targetInfos array");
    let found = arr
        .iter()
        .any(|t| t["targetId"].as_str() == Some(&target_id));
    assert!(
        found,
        "client B did not see target {target_id}; targets={arr:?}"
    );
}

#[tokio::test]
async fn ops_panel_serves_static_html() {
    let Some(endpoint) = spawn().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let base = endpoint.replacen("ws://", "http://", 1);
    let r = reqwest::Client::new()
        .get(format!("{base}/ops"))
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), reqwest::StatusCode::OK);
    let ct = r
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("text/html"), "content-type was {ct:?}");
    let body = r.text().await.expect("text");
    assert!(body.contains("afhttp"), "body missing brand: {body}");
}

#[tokio::test]
async fn ops_panel_assets_have_right_mime_types() {
    let Some(endpoint) = spawn().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let base = endpoint.replacen("ws://", "http://", 1);
    let js = reqwest::Client::new()
        .get(format!("{base}/ops/assets/app.js"))
        .send()
        .await
        .expect("js");
    assert_eq!(js.status(), reqwest::StatusCode::OK);
    let ct = js
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("javascript"), "js content-type was {ct:?}");

    let css = reqwest::Client::new()
        .get(format!("{base}/ops/assets/app.css"))
        .send()
        .await
        .expect("css");
    let ct = css
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("text/css"), "css content-type was {ct:?}");
}

#[tokio::test]
async fn inline_ephemeral_yields_a_usable_client() {
    let Some(bin) = support::env::discover_browser() else {
        println!("(skipping: no chromium)");
        return;
    };
    support::ensure_rustls_provider();
    // Pass the discovered binary explicitly so the test is deterministic across
    // platforms (CI runners install Chrome in non-standard, sometimes
    // non-auto-discoverable, locations); the auto-discovery path itself is
    // covered by the Docker integration job.
    let client = Client::inline_ephemeral_with(InlineConfig {
        browser_bin: Some(bin),
        ..Default::default()
    })
    .await
    .expect("inline_ephemeral_with");
    let h = client.health().await.expect("health");
    assert_eq!(h.status, "ok");
    let backend = h.backend.expect("backend");
    assert_eq!(backend.family, "chromium");
}
