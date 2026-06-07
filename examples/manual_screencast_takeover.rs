//! Manual ops-panel takeover smoke test.
//!
//! Spawns an afhttp host with a chromium backend, navigates the target
//! chromium to a "stuck" fixture page, and prints the ops panel URL.
//! A human operator opens that URL in their own browser, clicks the
//! gray box, presses K, and watches this terminal walk `window.stage`
//! 0 → 1 → 2.
//!
//! The fully automated counterpart is `tests/ops_panel_two_browser.rs`,
//! which simulates the human with a second chromium. This example
//! exists so a real human can feel the panel's latency / UX over a
//! real Mac-browser-to-container network path; the two together cover
//! both reproducible CI gating and human-in-the-loop UX validation.
//!
//! Recommended invocation: `tests/manual-screencast-takeover.sh`. That wrapper
//! runs this example inside the test container with port 9222 forwarded
//! to the Mac host so `http://127.0.0.1:9222/ops/screencast` resolves both inside
//! the container (for chromium) and on the Mac (for the operator's
//! browser).

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::disallowed_macros,
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::sync::Arc;
use std::time::Duration;

use agent_first_http::host::bootstrap::{
    BrowserChoice, DisplayMode, HealthPublic, HostArgs, ProfileChoice, Takeover,
};
use agent_first_http::host::browser;
use agent_first_http::host::listener::{router_for_tests, test_state};
use agent_first_http::sdk::Client;
use agent_first_http::shared::ids::TabId;
use serde_json::json;
use tokio::net::TcpListener;

const LISTEN: &str = "0.0.0.0:9222";

/// Default URL when no arg is passed. nowsecure.nl is a long-standing
/// bot-detection test page that exercises Cloudflare-style fingerprint
/// checks; with plain headless chromium it usually serves the
/// "Verifying you are human" interstitial (or blocks outright).
const DEFAULT_URL: &str = "https://nowsecure.nl/";

/// Same `window.stage` machine as the automated test, plus visible
/// feedback so the human operator can SEE their click and keystroke
/// take effect through the screencast (gray box → green; success
/// banner on Shift+K).
/// Small precise hit target so the test exercises the coordinate
/// mapping path — clicks outside the gray box stay at stage 0. Visible
/// feedback (color flip + status text) lets the human operator see
/// whether the click landed.
const STUCK_HTML: &str = r##"<!doctype html><html><head><meta charset="utf-8"><title>stuck</title></head>
<body style="margin:0;width:1280px;height:720px;background:#fff;font-family:system-ui,sans-serif">
<div id="hit" style="position:absolute;left:100px;top:100px;width:80px;height:80px;background:#888;cursor:pointer;border-radius:6px;transition:background 0.15s"></div>
<div id="msg" style="position:absolute;left:100px;top:210px;font-size:28px;color:#374151">stage 0 · click the gray box</div>
<div id="banner" style="position:absolute;left:100px;top:260px;font-size:48px;color:#16a34a;font-weight:600"></div>
<div id="cursor" style="position:absolute;left:-99px;top:-99px;width:18px;height:18px;border:2px solid #ef4444;border-radius:50%;pointer-events:none;transform:translate(-50%,-50%);transition:none;background:rgba(239,68,68,0.15)"></div>
<div id="xy" style="position:absolute;right:16px;top:16px;font-family:ui-monospace,monospace;font-size:18px;color:#6b7280">mouse: —</div>
<svg id="trail" style="position:absolute;left:0;top:0;width:1280px;height:720px;pointer-events:none"></svg>
<script>
  var hit = document.getElementById("hit");
  var msg = document.getElementById("msg");
  var banner = document.getElementById("banner");
  var cursor = document.getElementById("cursor");
  var xy = document.getElementById("xy");
  var trail = document.getElementById("trail");
  var lastX = null, lastY = null;
  window.stage = 0;
  window.addEventListener("mousemove", function (e) {
    cursor.style.left = e.clientX + "px";
    cursor.style.top = e.clientY + "px";
    xy.textContent = "mouse: (" + Math.round(e.clientX) + ", " + Math.round(e.clientY) + ")";
    if (lastX !== null) {
      var line = document.createElementNS("http://www.w3.org/2000/svg", "line");
      line.setAttribute("x1", lastX); line.setAttribute("y1", lastY);
      line.setAttribute("x2", e.clientX); line.setAttribute("y2", e.clientY);
      line.setAttribute("stroke", "rgba(239,68,68,0.45)");
      line.setAttribute("stroke-width", "2");
      trail.appendChild(line);
      // Cap trail length so SVG doesn't grow forever.
      while (trail.childNodes.length > 400) trail.removeChild(trail.firstChild);
    }
    lastX = e.clientX; lastY = e.clientY;
  });
  hit.addEventListener("click", function () {
    window.stage = 1;
    hit.style.background = "#22c55e";
    msg.textContent = "stage 1 · now press Shift+K";
  });
  window.addEventListener("keydown", function (e) {
    if (window.stage === 1 && e.key === "K") {
      window.stage = 2;
      msg.textContent = "stage 2 · ✅ takeover succeeded";
      banner.textContent = "UNLOCKED";
      document.body.style.background = "#f0fdf4";
    }
  });
</script></body></html>"##;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Launch chromium (the "target" the operator will drive through the panel).
    let args = HostArgs {
        listen: format!("tcp:{LISTEN}"),
        profile: ProfileChoice::Ephemeral,
        display: DisplayMode::Headless,
        takeover: Takeover::Off,
        display_quality: 100,
        browser: BrowserChoice::Chromium,
        browser_bin: None,
        token: None,
        ops_enabled: true,
        health_enabled: true,
        health_public: HealthPublic::Off,
        engine_envs: Vec::new(),
        browser_args: Vec::new(),
        proxy: None,
        recent_requests_cap: 0,
    };
    eprintln!("launching chromium…");
    let handle = browser::launch(&args).await?;
    eprintln!("chromium up: {} ({})", handle.family, handle.version);

    // 2. Stand up the listener (axum router with /ops/screencast, /cdp, /health, ...).
    let state = test_state(None, HealthPublic::Off).with_default_browser(Arc::new(handle));
    let app = router_for_tests(state);
    let listener = TcpListener::bind(LISTEN).await?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 3. Decide what page to put in front of the operator. With no arg
    //    we use the built-in stuck fixture (deterministic smoke test);
    //    with an arg we navigate to that URL (real-world test against
    //    sites like Cloudflare-protected pages).
    let nav_url = std::env::args().nth(1);
    let mode_real_url = nav_url.is_some();
    let url = nav_url.unwrap_or_else(|| {
        format!(
            "data:text/html;base64,{}",
            base64_encode(STUCK_HTML.as_bytes())
        )
    });

    let endpoint = "ws://127.0.0.1:9222";
    let client = Client::connect(endpoint)?;
    let t = client
        .cdp("Target.createTarget")
        .params(json!({"url": url}))
        .send()
        .await?;
    let target_id = t["targetId"]
        .as_str()
        .ok_or("Target.createTarget returned no targetId")?
        .to_string();

    // 4. Operator instructions.
    println!();
    println!("================================================================");
    println!("  Manual ops panel takeover test");
    println!("================================================================");
    println!();
    println!("  1. Open this URL in your browser:");
    println!("       http://127.0.0.1:9222/ops/screencast");
    println!();
    if mode_real_url {
        println!("  2. The screencast shows whatever the headless chromium is");
        println!("     fetching. With Cloudflare / WAF-protected targets you'll");
        println!("     likely see an interstitial — click through it from the");
        println!("     panel; this terminal will print URL + title changes.");
    } else {
        println!("  2. You should see a JPEG screencast of a white page with a");
        println!("     gray square in the upper-left (canvas-logical coords ~100,100).");
        println!();
        println!("  3. Click the gray square.  (stage 0 → 1)");
        println!();
        println!("  4. Press Shift+K.         (stage 1 → 2)");
    }
    println!();
    println!("  Ctrl-C to exit.");
    println!("================================================================");
    println!();

    // 5. Poll loop. For the stuck-fixture mode we tail window.stage; for
    //    the real-URL mode we report whenever document.title or
    //    location.href changes — those are the signals an operator
    //    progressed past an interstitial.
    if mode_real_url {
        let mut last_title = String::new();
        let mut last_url = String::new();
        loop {
            match client
                .cdp("Runtime.evaluate")
                .tab(TabId::new(target_id.clone()))
                .params(json!({
                    "expression": "JSON.stringify({title: document.title, url: location.href, ready: document.readyState})",
                    "returnByValue": true,
                }))
                .send()
                .await
            {
                Ok(v) => {
                    if let Some(s) = v["result"]["value"].as_str() {
                        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(s) {
                            let title = obj["title"].as_str().unwrap_or("").to_string();
                            let url = obj["url"].as_str().unwrap_or("").to_string();
                            let ready = obj["ready"].as_str().unwrap_or("");
                            if title != last_title || url != last_url {
                                let now = chrono_local_time();
                                println!("[{now}] [{ready:>11}] {title:?}  ←  {url}");
                                last_title = title;
                                last_url = url;
                            }
                        }
                    }
                }
                Err(e) => eprintln!("(poll error: {e})"),
            }
            tokio::time::sleep(Duration::from_millis(750)).await;
        }
    } else {
        let mut last: i64 = i64::MIN;
        loop {
            match client
                .cdp("Runtime.evaluate")
                .tab(TabId::new(target_id.clone()))
                .params(json!({
                    "expression": "window.stage",
                    "returnByValue": true,
                }))
                .send()
                .await
            {
                Ok(v) => {
                    let stage = v["result"]["value"].as_i64().unwrap_or(-1);
                    if stage != last {
                        let now = chrono_local_time();
                        let label = match stage {
                            0 => "waiting for click on the gray box",
                            1 => "click landed — now press Shift+K",
                            2 => "✅ takeover succeeded",
                            -1 => "(stage not readable — target may still be loading)",
                            _ => "(stage value is out of expected range)",
                        };
                        println!("[{now}] stage={stage}  {label}");
                        last = stage;
                        if stage == 2 {
                            println!();
                            println!("Press Ctrl-C to exit (or re-click to keep playing).");
                        }
                    }
                }
                Err(e) => eprintln!("(poll error: {e})"),
            }
            tokio::time::sleep(Duration::from_millis(750)).await;
        }
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// HH:MM:SS in UTC. Pulled in tiny inline form so the example doesn't
/// drag the `chrono` crate in just for a timestamp.
fn chrono_local_time() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}Z")
}
