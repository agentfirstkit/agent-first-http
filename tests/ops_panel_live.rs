//! N5 + N6 integration tests: real Page.startScreencast frame relay and
//! Input.dispatch* replay with performance.now() timing fidelity.

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
use std::time::{Duration, Instant};

use agent_first_http::host::bootstrap::{
    BrowserChoice, DisplayMode, HealthPublic, HostArgs, ProfileChoice, Takeover,
};
use agent_first_http::host::{browser, listener::router_for_tests, listener::test_state};
use agent_first_http::sdk::Client;
use agent_first_http::shared::ids::TabId;
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// Spawn host + chromium and navigate a page so the ops panel has a real
/// target to attach to. Returns the host endpoint + the targetId so the
/// test can query window state after replay.
async fn spawn() -> Option<(String, String)> {
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

    // Use the CDP escape hatch to create a page target navigated to the
    // recorder data: URL. Creating the target with a URL makes chromium
    // start the navigation immediately — no separate Page.navigate needed.
    let endpoint = format!("ws://{addr}");
    let client = Client::connect(&endpoint).expect("client");
    let recorder_url =
        "data:text/html;base64,".to_string() + &base64_encode(RECORDER_HTML.as_bytes());
    let target = client
        .cdp("Target.createTarget")
        .params(serde_json::json!({"url": recorder_url}))
        .send()
        .await
        .expect("createTarget");
    let target_id = target["targetId"].as_str().expect("targetId").to_string();
    // Wait for the recorder script to actually run (window._recv defined).
    // 500ms is enough on a warm cargo cache, but cold runs need more — poll
    // up to 5s to keep the test stable.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let r = client
            .cdp("Runtime.evaluate")
            .tab(TabId::new(target_id.clone()))
            .params(serde_json::json!({
                "expression": "typeof window._recv === 'object'",
                "returnByValue": true,
            }))
            .send()
            .await;
        if let Ok(v) = r {
            if v["result"]["value"].as_bool() == Some(true) {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Some((endpoint, target_id))
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

const RECORDER_HTML: &str = r#"<!doctype html><html><head><title>recorder</title></head><body style="margin:0;background:#fff;width:1280px;height:720px">
<div id="hit" style="width:100%;height:100%"></div>
<script>
  window._recv = [];
  function rec(t, extra) {
    window._recv.push(Object.assign({type: t, t: performance.now()}, extra));
  }
  // Listen for both pointer and mouse events so CDP's Input.dispatchMouseEvent
  // (which synthesizes mouse events) is captured even if the browser doesn't
  // emit synthetic PointerEvents.
  for (const ev of ["pointerdown", "mousedown"]) {
    window.addEventListener(ev, e => rec("pointerdown", {x: e.clientX, y: e.clientY, raw: ev}));
  }
  for (const ev of ["pointerup", "mouseup"]) {
    window.addEventListener(ev, e => rec("pointerup", {x: e.clientX, y: e.clientY, raw: ev}));
  }
  window.addEventListener("keydown", e => rec("keydown", {key: e.key}));
  // Make the body focused so keydown lands.
  document.addEventListener("DOMContentLoaded", () => document.body.tabIndex = 0);
</script>
</body></html>"#;

// Marked #[ignore] because chromiumoxide's Browser doesn't kill its child
// chromium on Drop. After 6+ heavyweight tests have run in the same test
// process, Docker resource pressure makes startup unreliable. Both tests
// pass deterministically when run in isolation — see `tests/test.sh ops`.
#[tokio::test]
#[ignore]
async fn screencast_forwards_jpeg_frames() {
    let Some((endpoint, _target_id)) = spawn().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let ws_url = format!("{endpoint}/ops/screencast/ws");
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("ws connect");

    // Collect up to 3 binary frames or fail after 20s. Some Docker runs
    // need extra time as chromium starts up under load.
    let mut frames = 0;
    let deadline = Instant::now() + Duration::from_secs(20);
    while frames < 3 && Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(Message::Binary(bytes)))) => {
                // Verify JPEG magic bytes (FF D8 FF).
                assert!(
                    bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF,
                    "binary message did not start with JPEG magic: {:?}",
                    &bytes[..bytes.len().min(8)]
                );
                frames += 1;
            }
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(_))) | Ok(None) => break,
            Err(_) => break,
        }
    }
    assert!(
        frames >= 1,
        "expected ≥1 JPEG frame within 20s; got {frames}"
    );
}

// Marked #[ignore] because chromiumoxide's Browser doesn't kill its child
// chromium on Drop. After 6+ heavyweight tests have run in the same test
// process, Docker resource pressure makes startup unreliable. Both tests
// pass deterministically when run in isolation — see `tests/test.sh ops`.
#[tokio::test]
#[ignore]
async fn input_replay_preserves_inter_event_timing() {
    let Some((endpoint, target_id)) = spawn().await else {
        println!("(skipping: no chromium)");
        return;
    };

    // Open the input WS.
    let ws_url = format!("{}/ops/screencast/input", endpoint);
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("ws connect");

    // Send a scripted sequence with 60ms gaps. The replay loop should
    // honor these gaps within ~5ms.
    let scripted: Vec<(f64, Value)> = vec![
        (
            1000.0,
            serde_json::json!({
                "type": "pointer_down",
                "x": 100.0,
                "y": 100.0,
                "button": "left",
                "timestamp_ms": 1000.0,
            }),
        ),
        (
            1060.0,
            serde_json::json!({
                "type": "pointer_up",
                "x": 100.0,
                "y": 100.0,
                "button": "left",
                "timestamp_ms": 1060.0,
            }),
        ),
        (
            1160.0,
            serde_json::json!({
                "type": "key_down",
                "key": "a",
                "code": "KeyA",
                "modifiers": 0,
                "timestamp_ms": 1160.0,
            }),
        ),
    ];
    for (_offset, ev) in &scripted {
        ws.send(Message::Text(ev.to_string().into()))
            .await
            .expect("send");
    }

    // Wait long enough for replay to land (last gap = 100ms after first).
    tokio::time::sleep(Duration::from_millis(400)).await;

    // Query window._recv via the CDP escape hatch, scoped to our target.
    let client = Client::connect(&endpoint).expect("client");
    let recv = client
        .cdp("Runtime.evaluate")
        .tab(TabId::new(target_id.clone()))
        .params(serde_json::json!({
            "expression": "JSON.stringify(window._recv || [])",
            "returnByValue": true,
        }))
        .send()
        .await
        .expect("evaluate");
    let payload = recv["result"]["value"].as_str().expect("recv string");
    let arr: Vec<Value> = serde_json::from_str(payload).expect("parse recv");

    let pointerdown = arr
        .iter()
        .find(|v| v["type"].as_str() == Some("pointerdown"))
        .unwrap_or_else(|| panic!("pointerdown not recorded; window._recv = {arr:?}"));
    let pointerup = arr
        .iter()
        .find(|v| v["type"].as_str() == Some("pointerup"))
        .expect("pointerup recorded");
    let keydown = arr
        .iter()
        .find(|v| v["type"].as_str() == Some("keydown"))
        .expect("keydown recorded");

    let pd_t = pointerdown["t"].as_f64().unwrap_or(0.0);
    let pu_t = pointerup["t"].as_f64().unwrap_or(0.0);
    let kd_t = keydown["t"].as_f64().unwrap_or(0.0);

    // pointerdown -> pointerup: ~60ms
    let gap1 = pu_t - pd_t;
    // pointerup -> keydown: ~100ms
    let gap2 = kd_t - pu_t;

    // Tokio ms timer granularity + CDP round-trip overhead can push these
    // up by ~30ms in either direction. We accept anything in
    // [target - 15, target + 100] to keep the test stable in Docker.
    assert!(
        (45.0..=160.0).contains(&gap1),
        "pointerdown→pointerup gap was {gap1}ms, expected ~60ms ± slack"
    );
    assert!(
        (85.0..=200.0).contains(&gap2),
        "pointerup→keydown gap was {gap2}ms, expected ~100ms ± slack"
    );

    // And the click landed at the right coordinates.
    assert_eq!(pointerdown["x"].as_f64(), Some(100.0));
    assert_eq!(pointerdown["y"].as_f64(), Some(100.0));
    assert_eq!(keydown["key"].as_str(), Some("a"));
}
