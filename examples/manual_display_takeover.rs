//! Manual real-display takeover smoke test (KasmVNC provider, human in the loop).
//!
//! Counterpart of `examples/manual_screencast_takeover.rs`, which exercises the
//! lightweight CDP takeover panel (`/takeover/screencast`). This one exercises the opt-in
//! **real-display takeover** mode (`--takeover display --display-provider
//! kasmvnc`, served at `/takeover/panel`): the browser runs *headful* on an in-container
//! KasmVNC X display, and the human drives it with real OS-level input
//! through a browser tab proxied by afhttp's authenticated listener.
//!
//! Unlike the CDP panel, this harness goes through the **real launch
//! path** — `AppState::launch` actually spawns `Xvnc`, waits for the X
//! display + web port, launches the browser headful on `DISPLAY=:NN`,
//! and wires the `/takeover/panel` reverse proxy. Nothing is faked, so the
//! whole point of the feature — input fidelity — is what you test.
//!
//! The automated counterpart is `tests/display_takeover.rs` (proxy/token
//! routing against a fake upstream, plus an `#[ignore]` real-`Xvnc`
//! launch smoke). This example puts a real human at the keyboard so the
//! *symptom that motivated the feature* can be felt directly: modifiers,
//! arrows, Backspace, and IME/CJK/Unicode composition all landing in a
//! real form field.
//!
//! Recommended invocation: `tests/manual-display-takeover.sh`. That
//! wrapper runs this example inside the test container with port 9222
//! forwarded to the host, and KasmVNC (`AFHTTP_KASMVNC_BIN` /
//! `AFHTTP_KASMVNC_WEB_ROOT`) already installed in the image.
//!
//! Usage (args are passed straight through by the wrapper):
//!   tests/manual-display-takeover.sh                 # chromium, ephemeral
//!   tests/manual-display-takeover.sh camoufox        # camoufox backend
//!   tests/manual-display-takeover.sh chromium work   # persistent profile "work"
//!
//! The second arg names a **persistent** profile — use it for the
//! warm-profile test: solve a login/challenge once via takeover, Ctrl-C,
//! re-run with the same profile name, confirm the session persisted.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::disallowed_macros,
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::time::Duration;

use agent_first_http::host::bootstrap::{
    install_rustls_provider, BrowserChoice, DisplayMode, HealthPublic, HostArgs, ProfileChoice,
    Takeover, TakeoverProviderKind,
};
use agent_first_http::host::listener::{build_router, AppState};
use agent_first_http::sdk::Client;
use agent_first_http::shared::ids::TabId;
use serde_json::json;
use tokio::net::TcpListener;

const LISTEN: &str = "0.0.0.0:9222";

/// Keypress-fidelity fixture. A real form field plus live logs of every
/// keydown (key/code + C·A·S·M modifiers + IME `isComposing`) and every
/// composition event. `window.__state` exposes the field value and both
/// logs so the poll loop can echo what actually landed — this is how you
/// see modifiers / arrows / Backspace / CJK reach the browser through
/// real OS input, the thing the CDP panel's hand-rolled keycode table
/// could not do.
const KEYS_HTML: &str = r##"<!doctype html><html><head><meta charset="utf-8"><title>display takeover · keypress test</title></head>
<body style="margin:0;width:1280px;height:720px;background:#0b1020;color:#e5e7eb;font-family:system-ui,sans-serif">
<div style="padding:24px">
  <div style="font-size:22px;font-weight:600;margin-bottom:4px">afhttp · real-display takeover keypress test</div>
  <div style="color:#9ca3af;margin-bottom:16px">Click the field, then type. Try modifiers (Ctrl/Alt/Shift), arrows, Backspace, and an IME / CJK string (e.g. 你好 / こんにちは). Everything should land.</div>
  <textarea id="f" rows="4" style="width:760px;font-size:20px;padding:10px;border-radius:8px;border:1px solid #334155;background:#111827;color:#f9fafb" placeholder="type here…"></textarea>
  <div style="display:flex;gap:24px;margin-top:16px">
    <div style="flex:1">
      <div style="color:#9ca3af;font-size:13px;text-transform:uppercase;letter-spacing:.05em;margin-bottom:6px">keydown log</div>
      <pre id="keys" style="height:300px;overflow:auto;margin:0;font-family:ui-monospace,monospace;font-size:14px;background:#111827;border-radius:8px;padding:10px"></pre>
    </div>
    <div style="flex:1">
      <div style="color:#9ca3af;font-size:13px;text-transform:uppercase;letter-spacing:.05em;margin-bottom:6px">IME composition log</div>
      <pre id="comp" style="height:300px;overflow:auto;margin:0;font-family:ui-monospace,monospace;font-size:14px;background:#111827;border-radius:8px;padding:10px"></pre>
    </div>
  </div>
</div>
<script>
  var f = document.getElementById("f");
  var keysEl = document.getElementById("keys");
  var compEl = document.getElementById("comp");
  window.__keys = [];
  window.__comps = [];
  function mods(e){ var m=""; m+=e.ctrlKey?"C":"·"; m+=e.altKey?"A":"·"; m+=e.shiftKey?"S":"·"; m+=e.metaKey?"M":"·"; return m; }
  function pushKey(s){ window.__keys.push(s); if(window.__keys.length>200)window.__keys.shift();
    keysEl.textContent = window.__keys.slice(-14).join("\n"); keysEl.scrollTop = keysEl.scrollHeight; }
  function pushComp(s){ window.__comps.push(s); if(window.__comps.length>200)window.__comps.shift();
    compEl.textContent = window.__comps.slice(-14).join("\n"); compEl.scrollTop = compEl.scrollHeight; }
  f.addEventListener("keydown", function(e){
    pushKey("[" + mods(e) + "] key=" + JSON.stringify(e.key) + " code=" + e.code + (e.isComposing?" (composing)":""));
  });
  f.addEventListener("compositionstart", function(e){ pushComp("start"); });
  f.addEventListener("compositionupdate", function(e){ pushComp("update " + JSON.stringify(e.data)); });
  f.addEventListener("compositionend",   function(e){ pushComp("end   " + JSON.stringify(e.data)); });
  window.__state = function(){ return JSON.stringify({ value: f.value, keys: window.__keys, comps: window.__comps }); };
  f.focus();
</script></body></html>"##;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    install_rustls_provider();

    // Args (all optional, order-flexible):
    //   <browser>     chromium | camoufox | fingerprint_chromium | auto
    //   <profile>     persistent profile name (default ephemeral)
    //   --url <URL> | http(s)://…   navigate a real page instead of the
    //                               built-in keypress fixture
    //   --quality <0-100>           display image quality (default 100)
    let mut browser_arg = "chromium".to_string();
    let mut profile_arg: Option<String> = None;
    let mut url_arg: Option<String> = None;
    let mut quality: u8 = 100;
    let mut positionals: Vec<String> = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if a == "--url" {
            url_arg = it.next();
        } else if a == "--quality" {
            quality = it
                .next()
                .and_then(|s| s.parse().ok())
                .filter(|q| *q <= 100)
                .unwrap_or(100);
        } else if a.starts_with("http://") || a.starts_with("https://") {
            url_arg = Some(a);
        } else {
            positionals.push(a);
        }
    }
    if let Some(b) = positionals.first() {
        browser_arg = b.clone();
    }
    if let Some(p) = positionals.get(1) {
        if p != "-" && !p.is_empty() {
            profile_arg = Some(p.clone());
        }
    }
    let browser = match browser_arg.as_str() {
        "auto" => BrowserChoice::Auto,
        "chromium" => BrowserChoice::Chromium,
        "chrome" => BrowserChoice::Chrome,
        "fingerprint_chromium" | "fingerprint-chromium" => BrowserChoice::FingerprintChromium,
        "camoufox" => BrowserChoice::Camoufox,
        other => {
            eprintln!("unknown browser {other:?}; use chromium|camoufox|fingerprint_chromium|auto");
            std::process::exit(2);
        }
    };
    let profile = match &profile_arg {
        Some(name) => {
            eprintln!("using persistent profile {name:?} (warm-profile test)");
            ProfileChoice::Persistent(name.clone())
        }
        None => ProfileChoice::Ephemeral,
    };

    // Real launch path: spawns Xvnc, waits for display + web port, then
    // launches the browser headful on DISPLAY=:NN and wires the proxy.
    let args = HostArgs {
        listen: format!("tcp:{LISTEN}"),
        profile,
        display: DisplayMode::Headful,
        takeover: Takeover::On {
            provider: TakeoverProviderKind::KasmVnc,
        },
        display_quality: quality,
        browser,
        browser_bin: None,
        token: None,
        takeover_enabled: true,
        health_enabled: true,
        health_public: HealthPublic::Off,
        engine_envs: Vec::new(),
        browser_args: Vec::new(),
        proxy: None,
        recent_requests_cap: 0,
    };
    eprintln!("launching KasmVNC + {browser_arg} headful (this can take a few seconds)…");
    let state = AppState::launch(&args).await?;
    let display = state
        .takeover
        .as_ref()
        .map(|d| d.display.clone())
        .unwrap_or_else(|| "(none)".into());
    eprintln!("display takeover ready on X display {display}");

    // Stand up the listener so the proxy + CDP are reachable on 9222.
    // `state` is kept in scope so its `Drop` (which reaps KasmVNC + the
    // browser) only fires on clean shutdown below, never mid-run.
    let app = build_router(state.clone());
    let listener = TcpListener::bind(LISTEN).await?;
    let serve_task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Open the target page in a real headful window on the display: the
    // built-in keypress fixture by default, or a real site via --url.
    let client = Client::connect("ws://127.0.0.1:9222")?;
    let real_url = url_arg.clone();
    let url = real_url.clone().unwrap_or_else(|| {
        format!(
            "data:text/html;base64,{}",
            base64_encode(KEYS_HTML.as_bytes())
        )
    });
    let created = client
        .cdp("Target.createTarget")
        .params(json!({"url": url}))
        .send()
        .await?;
    let target_id = created["targetId"]
        .as_str()
        .ok_or("Target.createTarget returned no targetId")?
        .to_string();

    println!();
    println!("================================================================");
    println!("  Manual real-display takeover test (KasmVNC provider)");
    println!("================================================================");
    println!();
    println!("  1. Open this URL in your browser:");
    println!("       http://127.0.0.1:9222/takeover/panel");
    println!();
    if let Some(u) = real_url.as_deref() {
        println!("  2. You're looking at a REAL headful browser on an in-container");
        println!("     X display, navigated to:");
        println!("       {u}");
        println!("     Drive it with real OS-level input — click, scroll, type,");
        println!("     solve a captcha / log in. This terminal reports title and");
        println!("     URL changes as you navigate.");
    } else {
        println!("  2. You're looking at a REAL headful browser on an in-container");
        println!("     X display. Click the text field and type:");
        println!("       · modifiers   — Ctrl/Alt/Shift combos");
        println!("       · navigation  — arrows, Home/End, Backspace, Delete");
        println!("       · IME / CJK   — switch to a CJK IME and type 你好 / こんにちは");
        println!("       · Unicode     — emoji, accented chars");
        println!("     This terminal echoes the field value + keydown/IME logs as");
        println!("     they land — all of it should match what you typed.");
    }
    println!();
    println!("  3. The poll loop below is the AGENT driving the same browser over");
    println!("     CDP *while you drive the display*. From another container shell:");
    println!("       afhttp fetch --endpoint-url http://127.0.0.1:9222 \\");
    println!("         --tab {target_id} --eval 'document.title'");
    println!();
    if profile_arg.is_some() {
        println!("  4. Warm-profile check: solve a login/challenge, Ctrl-C, then");
        println!("     re-run with the same profile name and confirm it persisted.");
        println!();
    }
    println!("  Ctrl-C to exit. On exit, KasmVNC + the browser are reaped");
    println!("  together (kill_on_drop) — verify with `ps` in the container:");
    println!("  no orphaned Xvnc / chromium / camoufox should remain.");
    println!("================================================================");
    println!();

    // Poll loop runs until Ctrl-C so shutdown is graceful: dropping `state`
    // reaps KasmVNC + the browser (matching `afhttp host`'s SIGTERM teardown).
    // For a real URL we tail title/location changes; for the fixture we echo
    // the field value + keydown/IME logs.
    let tab = TabId::new(target_id);
    tokio::select! {
        _ = async {
            if real_url.is_some() {
                url_poll_loop(&client, &tab).await
            } else {
                poll_loop(&client, &tab).await
            }
        } => {}
        _ = shutdown_signal() => {
            eprintln!("\nshutting down — reaping KasmVNC + browser…");
        }
    }

    // Tear down in order: close the CDP client (releases its connection's
    // AppState clone), stop the listener (releases the router's clone),
    // then drop our `state` — now the last clone, so `Drop` kills the
    // Xvnc and browser child processes.
    drop(client);
    serve_task.abort();
    drop(state);
    tokio::time::sleep(Duration::from_millis(300)).await;
    Ok(())
}

async fn poll_loop(client: &Client, tab: &TabId) {
    let mut last_value = String::new();
    let mut last_keys = 0usize;
    let mut last_comps = 0usize;
    loop {
        match client
            .cdp("Runtime.evaluate")
            .tab(tab.clone())
            .params(json!({
                "expression": "window.__state ? window.__state() : null",
                "returnByValue": true,
            }))
            .send()
            .await
        {
            Ok(v) => {
                if let Some(s) = v["result"]["value"].as_str() {
                    if let Ok(obj) = serde_json::from_str::<serde_json::Value>(s) {
                        let value = obj["value"].as_str().unwrap_or("").to_string();
                        let keys = obj["keys"].as_array().cloned().unwrap_or_default();
                        let comps = obj["comps"].as_array().cloned().unwrap_or_default();
                        for k in keys.iter().skip(last_keys) {
                            println!("[{}] keydown {}", now(), k.as_str().unwrap_or(""));
                        }
                        for c in comps.iter().skip(last_comps) {
                            println!("[{}] ime     {}", now(), c.as_str().unwrap_or(""));
                        }
                        if value != last_value {
                            println!("[{}] value = {value:?}", now());
                            last_value = value;
                        }
                        last_keys = keys.len();
                        last_comps = comps.len();
                    }
                }
            }
            Err(e) => eprintln!("(poll error: {e})"),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Real-URL mode: report whenever the page's title or location changes —
/// the signal that the operator navigated, logged in, or cleared a
/// challenge through the display.
async fn url_poll_loop(client: &Client, tab: &TabId) {
    let mut last = String::new();
    loop {
        match client
            .cdp("Runtime.evaluate")
            .tab(tab.clone())
            .params(json!({
                "expression":
                    "JSON.stringify({t:document.title,u:location.href,r:document.readyState})",
                "returnByValue": true,
            }))
            .send()
            .await
        {
            Ok(v) => {
                if let Some(s) = v["result"]["value"].as_str() {
                    if let Ok(obj) = serde_json::from_str::<serde_json::Value>(s) {
                        let line = format!(
                            "{:>11} {:?}  ←  {}",
                            obj["r"].as_str().unwrap_or(""),
                            obj["t"].as_str().unwrap_or(""),
                            obj["u"].as_str().unwrap_or("")
                        );
                        if line != last {
                            println!("[{}] {line}", now());
                            last = line;
                        }
                    }
                }
            }
            Err(e) => eprintln!("(poll error: {e})"),
        }
        tokio::time::sleep(Duration::from_millis(750)).await;
    }
}

/// Resolve on Ctrl-C (SIGINT) *or* SIGTERM, mirroring `afhttp host`'s
/// own `shutdown_signal`. Handling SIGTERM matters: a backgrounded
/// process has SIGINT ignored by job control, so `--takeover` teardown
/// (and `tests/manual-display-takeover.sh` under non-TTY shells) would
/// otherwise leak an Xvnc per run.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// HH:MM:SS in UTC. Inlined so the example doesn't pull in `chrono`.
fn now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "{:02}:{:02}:{:02}Z",
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60
    )
}
