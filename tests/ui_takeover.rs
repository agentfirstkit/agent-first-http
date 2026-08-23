//! `afhttp ui takeover` end to end, driven as a real process.
//!
//! The command's whole point is that it blocks on a person, so nothing about it
//! can be checked by calling a function: the assertions are that a window
//! process was launched on the panel URL, that the URL it was handed actually
//! authorizes, that the run ended when that process exited, and that both
//! emitted events are protocol-valid and carry no credential.
//!
//! `AFUI_BROWSER_BINARY` stands in for the browser. The stub records the
//! `--app=<url>` it was launched with, curls it, records the status, and exits
//! — which is a person closing the window.

#![cfg(feature = "cli")]
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

use std::path::Path;
use std::process::Command;

use serde_json::Value;

use support::takeover_host::{mint_panel_url, spawn_fake_provider, spawn_host};

const HOST_TOKEN: &str = "secret";

/// Stands in for the browser AFUI would launch. Records what it was asked to
/// open, proves the URL is reachable with the credential baked into it, then
/// exits — the stand-in for the person closing the window.
///
/// `--app=<url>` is how `UiWindow` opens a window rather than a browser, so
/// reading that argument is also an assertion that a *window* was requested.
const STUB_BROWSER: &str = r#"#!/bin/sh
set -eu
url=""
for arg in "$@"; do
  case "$arg" in
    --app=*) url="${arg#--app=}" ;;
  esac
done
printf '%s' "$url" > "$AFHTTP_STUB_DIR/url"
# Not -L: the credential is spent on this hop. What comes back is the panel's
# own landing page, which is where the display client's public prefix is worked
# out — in the browser, because nothing on the wire can say where a proxy put
# it.
curl -s -o /dev/null -w '%{http_code}' "$url" > "$AFHTTP_STUB_DIR/status"
"#;

fn write_stub(dir: &Path) -> std::path::PathBuf {
    let stub = dir.join("stub-browser.sh");
    std::fs::write(&stub, STUB_BROWSER).expect("write stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");
    }
    stub
}

/// Run `afhttp ui takeover` with the stub in place of a browser, and return the
/// events it emitted plus what the stub saw.
fn drive(args: &[&str], stub_dir: &Path) -> (Vec<Value>, String, String) {
    let stub = write_stub(stub_dir);
    let output = Command::new(env!("CARGO_BIN_EXE_afhttp"))
        .args(["ui", "takeover"])
        .args(args)
        .env("AFUI_BROWSER_BINARY", &stub)
        .env("AFHTTP_STUB_DIR", stub_dir)
        // The panel is announced to AFUI's cross-process session registry for
        // as long as the command runs, so give the test its own registry root
        // rather than writing into whoever runs it.
        .env("AFUI_CONFIG_DIR", stub_dir)
        // The command must not fall back to an endpoint nobody named.
        .env_remove("AFHTTP_ENDPOINT_URL")
        .env_remove("AFHTTP_TOKEN_SECRET")
        .output()
        .expect("run afhttp ui takeover");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "afhttp ui takeover failed: {stdout}{stderr}"
    );
    let events: Vec<Value> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{line:?}: {e}")))
        .collect();
    agent_first_data::validate_protocol_stream(&events, true)
        .expect("a blocking command still emits one well-formed stream");
    let opened = std::fs::read_to_string(stub_dir.join("url")).unwrap_or_default();
    let status = std::fs::read_to_string(stub_dir.join("status")).unwrap_or_default();
    (events, opened, status)
}

fn progress(events: &[Value]) -> &Value {
    events
        .iter()
        .find(|event| event["kind"] == "progress")
        .expect("a ui_ready progress event precedes the blocking window")
}

fn result(events: &[Value]) -> &Value {
    events
        .iter()
        .find(|event| event["kind"] == "result")
        .expect("a terminal result when the window closes")
}

#[tokio::test(flavor = "multi_thread")]
async fn minting_a_credential_opens_a_window_on_a_reachable_panel_and_waits_for_it() {
    let upstream = spawn_fake_provider().await;
    let base = spawn_host(Some(HOST_TOKEN), upstream).await;
    let stub_dir = tempfile::tempdir().expect("tmp");

    let (events, opened, status) = tokio::task::spawn_blocking({
        let base = base.clone();
        let dir = stub_dir.path().to_path_buf();
        move || {
            drive(
                &["--endpoint-url", &base, "--token-secret", HOST_TOKEN],
                &dir,
            )
        }
    })
    .await
    .expect("driver thread");

    // A window was opened on the panel this host minted, credential included.
    assert!(
        opened.starts_with(&format!("{base}/takeover/panel")),
        "window opened on {opened:?}"
    );
    assert!(opened.contains("handoff_secret="), "{opened}");
    // 200, not 401: the credential in that URL authorized, and what came back
    // is the panel's own landing page.
    assert_eq!(status, "200", "panel was not reachable with the credential");

    // ui_ready comes first, because opening the window blocks.
    assert_eq!(events.len(), 2, "{events:?}");
    let ready = progress(&events);
    assert_eq!(ready["progress"]["code"], "ui_takeover");
    assert_eq!(ready["progress"]["session"], "watch");
    assert_eq!(ready["progress"]["mode"], "window");
    // Present in every mode, because every delivery is announced: a window is
    // a registry entry too, best effort, and this is that entry's id. afhttp
    // used to assemble this event by hand and left the field out here; the
    // facts AFUI hands back do not have a hole in them for one delivery.
    assert!(
        ready["progress"]["session_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "{ready}"
    );
    assert_eq!(
        ready["progress"]["panel_url"],
        Value::String(format!("{base}/takeover/panel"))
    );
    assert!(ready["progress"].get("takeover_url_ttl_s").is_none());
    assert!(
        ready["progress"]
            .get("takeover_url_expires_at_rfc3339")
            .is_none()
    );

    // The stub exited, so the session ended.
    let done = result(&events);
    assert_eq!(done["result"]["code"], "ui_takeover");
    assert_eq!(done["result"]["outcome"], "closed");
    assert_eq!(done["result"]["session"], "watch");
    assert_eq!(done["result"]["mode"], "window");
    assert!(done["result"]["open_s"].is_number(), "{done}");

    // The credential reached the window and nothing else.
    let secret = support::takeover_host::handoff_secret_of(&opened);
    let emitted = serde_json::to_string(&events).unwrap();
    assert!(!emitted.contains(&secret), "credential leaked: {emitted}");
    assert!(
        !emitted.contains("handoff_secret"),
        "credential leaked: {emitted}"
    );

    let revoked = reqwest::get(&opened)
        .await
        .expect("request panel after UI delivery ended");
    assert_eq!(
        revoked.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "the private upstream lease outlived the AFUI delivery"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_already_minted_panel_url_needs_no_host_of_its_own() {
    let upstream = spawn_fake_provider().await;
    let base = spawn_host(Some(HOST_TOKEN), upstream).await;
    // What an earlier `afhttp panel` or `fetch --takeover` handed back.
    let panel_url = mint_panel_url(&base, HOST_TOKEN).await;
    let stub_dir = tempfile::tempdir().expect("tmp");

    let (events, opened, status) = tokio::task::spawn_blocking({
        let panel_url = panel_url.clone();
        let dir = stub_dir.path().to_path_buf();
        move || drive(&["--takeover-url-secret", &panel_url], &dir)
    })
    .await
    .expect("driver thread");

    assert!(
        opened.starts_with(&format!("{base}/takeover/panel?handoff_secret=")),
        "{opened}"
    );
    assert_ne!(
        opened, panel_url,
        "the fixed handoff must be exchanged for a UI lease"
    );
    assert_eq!(status, "200");
    let spent = reqwest::get(&panel_url)
        .await
        .expect("request exchanged fixed handoff");
    assert_eq!(
        spent.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "exchange left the fixed handoff usable alongside the UI lease"
    );
    // The internal source lease never becomes the delivery's public lifetime.
    let ready = progress(&events);
    assert!(
        ready["progress"].get("takeover_url_ttl_s").is_none(),
        "{ready}"
    );
    assert!(
        ready["progress"]
            .get("takeover_url_expires_at_rfc3339")
            .is_none(),
        "{ready}"
    );
    assert_eq!(result(&events)["result"]["outcome"], "closed");
}

#[test]
fn a_panel_url_that_is_not_a_url_fails_before_a_window_opens() {
    let stub_dir = tempfile::tempdir().expect("tmp");
    let stub = write_stub(stub_dir.path());
    let output = Command::new(env!("CARGO_BIN_EXE_afhttp"))
        .args(["ui", "takeover", "--takeover-url-secret", "not a url"])
        .env("AFUI_BROWSER_BINARY", &stub)
        .env("AFHTTP_STUB_DIR", stub_dir.path())
        .output()
        .expect("run afhttp ui takeover");
    assert!(!output.status.success());
    assert!(
        !stub_dir.path().join("url").exists(),
        "a bad URL must be rejected before anything is launched"
    );
    assert_eq!(error_event(&output)["error"]["code"], "invalid_endpoint");
}

/// The delivery for a machine with no screen: no window, a listing instead.
///
/// This is the shape the remote shell needs — `afui session list` cannot offer
/// what nothing announced — and it is also the only shape a headless container
/// has, where the window delivery fails outright. What it must prove is that
/// the announcement is real while the command runs and gone when it is not,
/// because a listing that outlives its owner is exactly the ownerless zombie
/// AFUI's session criteria forbid.
///
/// Driven through `AFUI_DELIVERY` rather than the flag, because that is the
/// shape a machine with no screen actually runs in: the variable is set once,
/// and every takeover under it stops reaching for a window. A `--mode` with a
/// default of its own would swallow this, and nothing else in this suite would
/// notice.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_windowless_takeover_announces_the_panel_and_withdraws_it_on_the_way_out() {
    let upstream = spawn_fake_provider().await;
    let base = spawn_host(Some(HOST_TOKEN), upstream).await;
    let panel_url = mint_panel_url(&base, HOST_TOKEN).await;
    let registry = tempfile::tempdir().expect("tmp");
    let sessions = registry.path().join("sessions");

    let child = Command::new(env!("CARGO_BIN_EXE_afhttp"))
        .args(["ui", "takeover", "--takeover-url-secret", &panel_url])
        .env("AFUI_DELIVERY", "session")
        .env("AFUI_CONFIG_DIR", registry.path())
        // Nothing may be launched here: the point of this delivery is that
        // there is nowhere to launch it.
        .env("AFUI_BROWSER_BINARY", "/nonexistent/browser")
        .env_remove("AFHTTP_ENDPOINT_URL")
        .env_remove("AFHTTP_TOKEN_SECRET")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn afhttp ui takeover");

    let entry = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            if let Ok(read) = std::fs::read_dir(&sessions) {
                for item in read.flatten() {
                    if let Ok(body) = std::fs::read(item.path())
                        && let Ok(value) = serde_json::from_slice::<Value>(&body)
                    {
                        return value;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the panel is announced while the command runs");

    assert_eq!(entry["provider_id"], "afhttp");
    assert_eq!(entry["ui_kind"], "takeover");
    // The fixed handoff was exchanged for a process-bound upstream lease. The
    // registry holds that private source URL; neither one is emitted.
    let registered_url = entry["access_url_secret"]
        .as_str()
        .expect("registered upstream URL");
    assert!(
        registered_url.starts_with(&format!("{base}/takeover/panel?handoff_secret=")),
        "{registered_url}"
    );
    assert_ne!(registered_url, panel_url);
    // The announcing process, which is what bounds this session: its upstream
    // is a host that will still be there afterwards.
    assert_eq!(entry["owner_pid"].as_u64(), Some(u64::from(child.id())));

    // The other ending this delivery has: the agent stops waiting.
    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("send SIGTERM");
    assert!(killed.success());
    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success(), "{:?}", output.status);

    let events: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{line:?}: {e}")))
        .collect();
    agent_first_data::validate_protocol_stream(&events, true).expect("one well-formed stream");
    assert_eq!(progress(&events)["progress"]["mode"], "session");
    let done = result(&events);
    assert_eq!(done["result"]["mode"], "session");
    assert_eq!(done["result"]["outcome"], "stopped");

    let left_behind: Vec<_> = std::fs::read_dir(&sessions)
        .map(|read| read.flatten().map(|item| item.path()).collect())
        .unwrap_or_default();
    assert!(
        left_behind.is_empty(),
        "the announcement must be withdrawn when the command ends: {left_behind:?}"
    );

    let emitted = serde_json::to_string(&events).unwrap();
    assert!(!emitted.contains("handoff_secret"), "leaked: {emitted}");

    let revoked = reqwest::get(registered_url)
        .await
        .expect("request panel after Session delivery ended");
    assert_eq!(
        revoked.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "the private upstream lease outlived the Session delivery"
    );
}

/// A registry that cannot be written must not take the window with it.
///
/// The window is the delivery here; the listing is a best-effort bonus, just
/// as it is for a Router AFUI serves itself.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_window_still_opens_when_the_panel_cannot_be_announced() {
    let upstream = spawn_fake_provider().await;
    let base = spawn_host(Some(HOST_TOKEN), upstream).await;
    let panel_url = mint_panel_url(&base, HOST_TOKEN).await;
    let stub_dir = tempfile::tempdir().expect("tmp");
    let stub = write_stub(stub_dir.path());
    // A registry root that is a file, so creating the sessions directory under
    // it cannot succeed.
    let blocked = stub_dir.path().join("not-a-directory");
    std::fs::write(&blocked, b"").expect("write blocker");

    let output = Command::new(env!("CARGO_BIN_EXE_afhttp"))
        .args(["ui", "takeover", "--takeover-url-secret", &panel_url])
        .env("AFUI_BROWSER_BINARY", &stub)
        .env("AFHTTP_STUB_DIR", stub_dir.path())
        .env("AFUI_CONFIG_DIR", &blocked)
        .env_remove("AFHTTP_ENDPOINT_URL")
        .env_remove("AFHTTP_TOKEN_SECRET")
        .output()
        .expect("run afhttp ui takeover");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(output.status.success(), "{stdout}");

    let events: Vec<Value> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{line:?}: {e}")))
        .collect();
    agent_first_data::validate_protocol_stream(&events, true).expect("one well-formed stream");
    assert_eq!(progress(&events)["progress"]["mode"], "window");
    assert!(
        progress(&events)["progress"]["session_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    assert_eq!(result(&events)["result"]["outcome"], "closed");
    // And the window really opened on the panel.
    let opened = std::fs::read_to_string(stub_dir.path().join("url")).unwrap_or_default();
    assert!(opened.starts_with(&format!("{base}/takeover/panel?handoff_secret=")));
    assert_ne!(opened, panel_url);
}

/// A Link is AFUI's one-session page, never the upstream takeover URL.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_link_hands_over_an_afui_owned_page_before_it_blocks() {
    let upstream = spawn_fake_provider().await;
    // Loopback is enough for the upstream now: AFUI is the LAN listener and
    // proxies to this private source itself.
    let base = spawn_host(Some(HOST_TOKEN), upstream).await;
    let registry = tempfile::tempdir().expect("tmp");
    let events_path = registry.path().join("events.jsonl");
    let events_file = std::fs::File::create(&events_path).expect("event file");

    let mut child = Command::new(env!("CARGO_BIN_EXE_afhttp"))
        .args([
            "ui",
            "takeover",
            "--endpoint-url",
            &base,
            "--token-secret",
            HOST_TOKEN,
            "--mode",
            "link",
        ])
        .env("AFUI_CONFIG_DIR", registry.path())
        // A link opens nothing here; the device that opens it is elsewhere.
        .env("AFUI_BROWSER_BINARY", "/nonexistent/browser")
        .env_remove("AFHTTP_ENDPOINT_URL")
        .env_remove("AFHTTP_TOKEN_SECRET")
        .stdout(std::process::Stdio::from(events_file))
        .spawn()
        .expect("spawn afhttp ui takeover");

    let ready = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let body = std::fs::read_to_string(&events_path).unwrap_or_default();
            if let Some(event) = body
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .find(|event| event["kind"] == "progress")
            {
                return event;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the AFUI Link is emitted before the command waits");

    assert_eq!(ready["progress"]["mode"], "link");
    let handed_over = ready["progress"][agent_first_ui::cli::LINK_URL_FIELD]
        .as_str()
        .unwrap_or_else(|| panic!("a Link must hand back AFUI's URL: {ready}"));
    assert!(!handed_over.contains("handoff_secret"), "{handed_over}");
    assert!(!handed_over.starts_with(&base), "{handed_over}");
    assert_eq!(ready["progress"]["idle_timeout_s"], 900);
    assert_eq!(ready["progress"]["grace_period_s"], 300);
    assert_eq!(
        ready["progress"]["panel_url"],
        Value::String(format!("{base}/takeover/panel"))
    );

    // Connect over loopback while retaining the AFUI capability path. The
    // advertised LAN host may differ, but the listener itself binds every
    // interface and the shell must already be answering.
    let mut local_link = url::Url::parse(handed_over).expect("AFUI Link URL");
    local_link
        .set_host(Some("127.0.0.1"))
        .expect("replace advertised host for local test");
    let status = reqwest::get(local_link)
        .await
        .expect("open AFUI Link page")
        .status();
    assert_eq!(status, 200, "the AFUI Link page was not reachable");

    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("send SIGTERM");
    assert!(killed.success());
    let status = child.wait().expect("wait");
    assert!(status.success(), "{status:?}");

    let events: Vec<Value> = std::fs::read_to_string(&events_path)
        .expect("read events")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{line:?}: {e}")))
        .collect();
    agent_first_data::validate_protocol_stream(&events, true).expect("one well-formed stream");

    let done = result(&events);
    assert_eq!(done["result"]["mode"], "link");
    assert_eq!(done["result"]["outcome"], "stopped");
    assert!(
        !serde_json::to_string(done)
            .unwrap()
            .contains("handoff_secret"),
        "{done}"
    );
}

/// A delivery word that names no delivery is an argument error, whether it was
/// typed or left in the environment — never a silent fall back to a window,
/// which is the one failure `AFUI_DELIVERY` exists to prevent.
#[test]
fn a_delivery_word_that_names_no_delivery_is_refused() {
    let output = Command::new(env!("CARGO_BIN_EXE_afhttp"))
        .args([
            "ui",
            "takeover",
            "--takeover-url-secret",
            "http://127.0.0.1:9222/takeover/panel?handoff_secret=deadbeef",
        ])
        .env("AFUI_DELIVERY", "lan")
        .env_remove("AFHTTP_ENDPOINT_URL")
        .env_remove("AFHTTP_TOKEN_SECRET")
        .output()
        .expect("run afhttp ui takeover");

    assert!(!output.status.success());
    let event = error_event(&output);
    assert_eq!(event["error"]["code"], "invalid_argument");
    let detail = event["error"]["message"].as_str().unwrap_or_default();
    for word in ["window", "link", "session"] {
        assert!(detail.contains(word), "{detail}");
    }
}

/// The one protocol error event a failed run emits, from whichever stream it
/// landed on.
fn error_event(output: &std::process::Output) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("{stdout}{stderr}")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .find_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event["kind"] == "error")
        .expect("a protocol error event")
}
