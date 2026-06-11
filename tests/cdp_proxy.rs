//! Integration test for the `/cdp` WebSocket proxy and the chromiumoxide
//! browser launch. Requires `AFHTTP_TEST_BROWSER_BIN` (the Dockerfile sets
//! it to `/usr/bin/chromium`).

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
use agent_first_http::host::browser;
use agent_first_http::host::listener::{router_for_tests, test_state};
use agent_first_http::sdk::Client;
use futures::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

fn discover_browser() -> Option<std::path::PathBuf> {
    support::env::discover_browser()
}

fn make_args(browser_bin: std::path::PathBuf) -> HostArgs {
    HostArgs {
        listen: "tcp:127.0.0.1:0".into(),
        profile: ProfileChoice::Ephemeral,
        display: DisplayMode::Headless,
        takeover: Takeover::Off,
        display_quality: 100,
        browser: BrowserChoice::Chromium,
        browser_bin: Some(browser_bin),
        token: None,
        takeover_enabled: true,
        health_enabled: true,
        health_public: HealthPublic::Off,
        engine_envs: Vec::new(),
        browser_args: Vec::new(),
        proxy: None,
        recent_requests_cap: 0,
    }
}

#[tokio::test]
async fn launches_browser_and_health_flips_to_ok() {
    let Some(bin) = discover_browser() else {
        println!("(skipping: no browser binary)");
        return;
    };
    support::ensure_rustls_provider();

    let args = make_args(bin);
    let handle = browser::launch(&args).await.expect("browser launch");
    let state = test_state(None, HealthPublic::Off).with_default_browser(Arc::new(handle));

    let app = router_for_tests(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = Client::connect(&format!("http://{addr}")).expect("client");
    let health = client.health().await.expect("health");
    assert_eq!(health.status, "ok", "{health:?}");
    let backend = health.backend.expect("backend present");
    assert_eq!(backend.family, "chromium");
    assert!(backend.connected);
    assert!(
        !backend.version.is_empty(),
        "backend.version should not be empty",
    );

    client
        .cdp("Target.createTarget")
        .params(serde_json::json!({"url": "about:blank"}))
        .send()
        .await
        .expect("create target");
    let with_target = client.health().await.expect("health after target");
    assert!(
        with_target.tabs_active >= 1,
        "tabs_active should reflect Target.getTargets page count: {with_target:?}"
    );
}

#[tokio::test]
async fn cdp_proxy_forwards_browser_getversion() {
    let Some(bin) = discover_browser() else {
        println!("(skipping: no browser binary)");
        return;
    };
    support::ensure_rustls_provider();

    let args = make_args(bin);
    let handle = browser::launch(&args).await.expect("browser launch");
    let state = test_state(None, HealthPublic::Off).with_default_browser(Arc::new(handle));

    let app = router_for_tests(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let url = format!("ws://{addr}/cdp");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("ws connect");
    let req = serde_json::json!({
        "id": 1,
        "method": "Browser.getVersion",
        "params": {}
    });
    ws.send(Message::Text(req.to_string().into()))
        .await
        .expect("send");

    let resp = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("recv timeout")
        .expect("recv")
        .expect("recv ok");
    let text = match resp {
        Message::Text(t) => t.to_string(),
        other => panic!("expected text, got {other:?}"),
    };
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(parsed["id"], 1);
    assert!(
        parsed.get("result").is_some(),
        "expected result object: {parsed}"
    );
    let result = &parsed["result"];
    assert!(
        result["product"].is_string(),
        "expected product field: {result}"
    );
    let product = result["product"].as_str().unwrap_or_default();
    assert!(
        product.to_lowercase().contains("chrom"),
        "expected chromium product string, got {product:?}"
    );
}

#[tokio::test]
async fn cdp_proxy_returns_503_when_no_backend() {
    support::ensure_rustls_provider();

    // No browser launched → browser_ws_url stays None.
    let state = test_state(None, HealthPublic::Off);
    let app = router_for_tests(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Use plain HTTP — without a WebSocket upgrade, axum's WebSocketUpgrade
    // extractor returns 400. Instead, do an actual WS upgrade attempt; the
    // handler short-circuits with 503 before upgrading.
    let raw = reqwest::Client::new()
        .get(format!("http://{addr}/cdp"))
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .expect("send");
    // axum reaches the handler; the handler responds 503 because no backend.
    assert_eq!(
        raw.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "expected 503 when no backend; got {}",
        raw.status()
    );
}
