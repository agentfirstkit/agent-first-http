//! Deep two-browser E2E for the ops panel.
//!
//! `ops_panel_live.rs` opens `/ops/screencast` and `/ops/input` directly
//! and shoves bytes in. That validates the host's WebSocket → CDP relay,
//! but leaves the panel's own HTML+JS — the code that actually runs in
//! the operator's browser — out of the loop.
//!
//! This test wires a second Chromium ("operator") in front of the panel:
//!
//!   target chromium  ⇐  afhttp host  ⇐  panel JS (in operator chromium)
//!     stuck page                          captures pointer/keydown
//!                                         from the operator's canvas
//!
//! Driving the operator over CDP (`Input.dispatchMouseEvent` /
//! `dispatchKeyEvent`) makes chromium emit real `pointerdown` /
//! `pointerup` / `keydown` events to the panel JS. The panel forwards
//! them over WS to the host, the host replays via CDP on the target,
//! and the stuck page's `window.stage` walks from 0 → 1 → 2. Either
//! path breaking leaves the stage where it was, so the assertions
//! pinpoint which leg failed.
//!
//! Marked `#[ignore]` for the same reason `ops_panel_live.rs` is: two
//! Chromiums in one test process push Docker resource pressure past
//! what the default suite tolerates. Run via `tests/test.sh ops`.

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
use agent_first_http::host::browser::{self, BrowserHandle};
use agent_first_http::host::listener::{router_for_tests, test_state};
use agent_first_http::sdk::cdp::ws_client::Connection;
use agent_first_http::sdk::Client;
use agent_first_http::shared::ids::TabId;
use serde_json::{json, Value};
use tokio::net::TcpListener;

/// Page where `#hit` flips `window.stage` to 1 on click and the document
/// flips it to 2 only after a subsequent `K` keydown. Two distinct stages
/// give the test a clear signal for which event path landed.
const STUCK_HTML: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>stuck</title></head>
<body style="margin:0;width:1280px;height:720px;background:#fff">
<div id="hit" style="position:absolute;left:100px;top:100px;width:80px;height:80px;background:#888"></div>
<script>
  window.stage = 0;
  document.getElementById("hit").addEventListener("click", function () {
    window.stage = 1;
  });
  window.addEventListener("keydown", function (e) {
    if (window.stage === 1 && e.key === "K") {
      window.stage = 2;
    }
  });
</script></body></html>"#;

async fn spawn_chromium() -> Option<BrowserHandle> {
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
    browser::launch(&args).await.ok()
}

async fn cdp_send(conn: &Connection, method: &str, params: Value, sid: Option<&str>) -> Value {
    match conn.send(method, &params, sid).await {
        Ok(v) => v,
        Err(e) => panic!("CDP {method} failed: {e}"),
    }
}

async fn attach_target(conn: &Connection, target_id: &str) -> String {
    let v = cdp_send(
        conn,
        "Target.attachToTarget",
        json!({"targetId": target_id, "flatten": true}),
        None,
    )
    .await;
    v["sessionId"]
        .as_str()
        .expect("sessionId missing")
        .to_string()
}

async fn eval_value(conn: &Connection, sid: &str, expr: &str) -> Value {
    let v = cdp_send(
        conn,
        "Runtime.evaluate",
        json!({
            "expression": expr,
            "returnByValue": true,
        }),
        Some(sid),
    )
    .await;
    v["result"]["value"].clone()
}

// Poll a JS expression on the operator until it returns truthy. Used to
// wait for the panel canvas to mount before we start clicking it.
async fn wait_until_truthy(conn: &Connection, sid: &str, expr: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let v = eval_value(conn, sid, expr).await;
        match v {
            Value::Bool(true) => return true,
            Value::Number(n) if n.as_f64().unwrap_or(0.0) > 0.0 => return true,
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn wait_for_target_stage(
    client: &Client,
    target_id: &str,
    want: i64,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(v) = client
            .cdp("Runtime.evaluate")
            .tab(TabId::new(target_id.to_string()))
            .params(json!({
                "expression": "window.stage",
                "returnByValue": true,
            }))
            .send()
            .await
        {
            if v["result"]["value"].as_i64() == Some(want) {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    false
}

async fn wait_for_target_ready(client: &Client, target_id: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(v) = client
            .cdp("Runtime.evaluate")
            .tab(TabId::new(target_id.to_string()))
            .params(json!({
                "expression": "typeof window.stage === 'number'",
                "returnByValue": true,
            }))
            .send()
            .await
        {
            if v["result"]["value"].as_bool() == Some(true) {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[tokio::test]
#[ignore]
async fn operator_browser_drives_target_via_real_panel() {
    support::ensure_rustls_provider();

    // ---- 1. target browser + afhttp host serving /ops -------------------
    let Some(target_handle) = spawn_chromium().await else {
        println!("(skipping: no chromium for target)");
        return;
    };
    let state = test_state(None, HealthPublic::Off).with_default_browser(Arc::new(target_handle));
    let app = router_for_tests(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind host");
    let host_addr = listener.local_addr().expect("host local_addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Navigate the target to the stuck page via the host's CDP proxy. Using
    // the proxy (rather than the raw target WS) confirms multi-attach: the
    // panel's relay will later open its own session to the same target.
    let host_endpoint = format!("ws://{host_addr}");
    let host_client = Client::connect(&host_endpoint).expect("host client");
    let stuck_url = format!(
        "data:text/html;base64,{}",
        base64_encode(STUCK_HTML.as_bytes())
    );
    let target = host_client
        .cdp("Target.createTarget")
        .params(json!({"url": stuck_url}))
        .send()
        .await
        .expect("createTarget(stuck)");
    let target_page_id = target["targetId"]
        .as_str()
        .expect("targetId(stuck)")
        .to_string();
    // Pin the TARGET viewport to exactly the canvas-logical size the test
    // expects. Headless chromium otherwise defaults to ~800×600, which the
    // panel JS now adapts to (since the screencast frame size, not a
    // hardcoded 1280×720, drives the canvas backing store). Without this
    // override the test's click coords below would drift with chromium's
    // default viewport rather than the fixture's body dimensions.
    let _ = host_client
        .cdp("Emulation.setDeviceMetricsOverride")
        .tab(TabId::new(target_page_id.clone()))
        .params(json!({
            "width": 1280,
            "height": 720,
            "deviceScaleFactor": 1,
            "mobile": false,
        }))
        .send()
        .await
        .expect("setDeviceMetricsOverride(target)");
    assert!(
        wait_for_target_ready(&host_client, &target_page_id).await,
        "stuck page never set window.stage"
    );

    // ---- 2. operator browser (the "human's Mac") ------------------------
    let Some(op_handle) = spawn_chromium().await else {
        println!("(skipping: operator chromium launch failed)");
        return;
    };
    let op_ws_url = op_handle.ws_url.clone();
    let op_conn = Connection::connect(&op_ws_url, None)
        .await
        .expect("operator CDP");

    // Open the panel page directly via the operator's CDP — same URL a
    // human would paste from `afhttp ui`.
    let panel_url = format!("http://{host_addr}/ops");
    let v = cdp_send(
        &op_conn,
        "Target.createTarget",
        json!({"url": panel_url}),
        None,
    )
    .await;
    let op_page_id = v["targetId"].as_str().expect("targetId(panel)").to_string();
    let op_sid = attach_target(&op_conn, &op_page_id).await;

    // Force a known viewport so the canvas is fully laid out at (0, ~h1+p)
    // with width 1280. Without this, headless defaults vary by version.
    cdp_send(
        &op_conn,
        "Emulation.setDeviceMetricsOverride",
        json!({
            "width": 1280,
            "height": 900,
            "deviceScaleFactor": 1,
            "mobile": false,
        }),
        Some(&op_sid),
    )
    .await;

    // The panel script is `type="module"`, so its `canvas` / `screencast`
    // / `input` bindings are module-scoped, not on `window`. We rely on
    // the explicit `window.__opsScreencastOpen` / `__opsInputOpen` hooks
    // set in app.js so the test never races against the WS handshake.
    assert!(
        wait_until_truthy(
            &op_conn,
            &op_sid,
            "document.getElementById('screen') !== null",
            Duration::from_secs(10),
        )
        .await,
        "panel canvas element never mounted"
    );
    assert!(
        wait_until_truthy(
            &op_conn,
            &op_sid,
            "window.__opsScreencastOpen === true && window.__opsInputOpen === true",
            Duration::from_secs(15),
        )
        .await,
        "ops panel WebSockets never reached open state"
    );

    // Resolve canvas viewport rect so we can compute the operator-side
    // clientX/Y that maps to canvas-logical (140, 140) — inside #hit.
    let rect_v = eval_value(
        &op_conn,
        &op_sid,
        "JSON.stringify((function () { \
           var r = document.getElementById('screen').getBoundingClientRect(); \
           return {l: r.left, t: r.top, w: r.width, h: r.height}; \
         })())",
    )
    .await;
    let rect: Value =
        serde_json::from_str(rect_v.as_str().expect("rect string")).expect("rect json parse");
    let l = rect["l"].as_f64().expect("rect.l");
    let t = rect["t"].as_f64().expect("rect.t");
    let w = rect["w"].as_f64().expect("rect.w");
    let h = rect["h"].as_f64().expect("rect.h");
    assert!(w > 0.0 && h > 0.0, "canvas has zero size: {rect}");
    let click_x = l + 140.0 * w / 1280.0;
    let click_y = t + 140.0 * h / 720.0;

    // Operator-side counter for pointerdowns on the panel canvas. Lets a
    // failure assertion below distinguish "click missed the canvas in the
    // operator" from "panel JS captured it but the relay dropped it".
    cdp_send(
        &op_conn,
        "Runtime.evaluate",
        json!({
            "expression": "\
                window.__opsCanvasPointers = 0; \
                document.getElementById('screen').addEventListener('pointerdown', function () { \
                  window.__opsCanvasPointers++; \
                }); \
                true",
            "returnByValue": true,
        }),
        Some(&op_sid),
    )
    .await;

    // ---- 3. click on operator canvas → panel WS → target #hit click ----
    //
    // One click is normally enough — the WS-ready wait above means the
    // panel's `send()` is no longer in its drop-events-silently window.
    // We still retry a small number of times to absorb the inevitable
    // single-digit-millisecond races between WS open and listener attach.
    let mut clicked_to_stage_1 = false;
    for _ in 0..5 {
        cdp_send(
            &op_conn,
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseMoved",
                "x": click_x,
                "y": click_y,
                "button": "none",
            }),
            Some(&op_sid),
        )
        .await;
        cdp_send(
            &op_conn,
            "Input.dispatchMouseEvent",
            json!({
                "type": "mousePressed",
                "x": click_x,
                "y": click_y,
                "button": "left",
                "clickCount": 1,
            }),
            Some(&op_sid),
        )
        .await;
        cdp_send(
            &op_conn,
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseReleased",
                "x": click_x,
                "y": click_y,
                "button": "left",
                "clickCount": 1,
            }),
            Some(&op_sid),
        )
        .await;
        if wait_for_target_stage(&host_client, &target_page_id, 1, Duration::from_secs(2)).await {
            clicked_to_stage_1 = true;
            break;
        }
    }
    let op_pointers = eval_value(&op_conn, &op_sid, "window.__opsCanvasPointers").await;
    assert!(
        clicked_to_stage_1,
        "operator click never reached target #hit (stage stayed 0); \
         operator panel canvas saw {op_pointers} pointerdown events"
    );

    // ---- 4. keydown K on operator → panel WS → target keydown ----------
    let mut keyed_to_stage_2 = false;
    for _ in 0..5 {
        cdp_send(
            &op_conn,
            "Input.dispatchKeyEvent",
            json!({
                "type": "keyDown",
                "key": "K",
                "code": "KeyK",
                "text": "K",
                "modifiers": 0,
            }),
            Some(&op_sid),
        )
        .await;
        cdp_send(
            &op_conn,
            "Input.dispatchKeyEvent",
            json!({
                "type": "keyUp",
                "key": "K",
                "code": "KeyK",
                "modifiers": 0,
            }),
            Some(&op_sid),
        )
        .await;
        if wait_for_target_stage(&host_client, &target_page_id, 2, Duration::from_secs(2)).await {
            keyed_to_stage_2 = true;
            break;
        }
    }
    assert!(
        keyed_to_stage_2,
        "operator keydown K never reached target window (stage stayed at 1)"
    );

    op_conn.close();
    drop(op_handle);
}
