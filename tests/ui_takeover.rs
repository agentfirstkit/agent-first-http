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
    assert_eq!(ready["progress"]["delivery"], "window");
    // Announced while it runs, so a person can be pointed at it by name and an
    // agent never has to handle the credential to do it.
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
    // This run minted the credential, so it reports the deadline it was given.
    assert!(ready["progress"]["takeover_url_ttl_s"].as_u64().unwrap() > 0);
    assert!(
        ready["progress"]["takeover_url_expires_at_rfc3339"]
            .as_str()
            .is_some_and(|value| value.ends_with('Z')),
        "{ready}"
    );

    // The stub exited, so the session ended.
    let done = result(&events);
    assert_eq!(done["result"]["code"], "ui_takeover");
    assert_eq!(done["result"]["outcome"], "closed");
    assert_eq!(done["result"]["session"], "watch");
    assert!(done["result"]["open_s"].is_number(), "{done}");

    // The credential reached the window and nothing else.
    let secret = support::takeover_host::handoff_secret_of(&opened);
    let emitted = serde_json::to_string(&events).unwrap();
    assert!(!emitted.contains(&secret), "credential leaked: {emitted}");
    assert!(
        !emitted.contains("handoff_secret"),
        "credential leaked: {emitted}"
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

    assert_eq!(opened, panel_url);
    assert_eq!(status, "200");
    // No host call was made, so there is no fresh deadline to report — the one
    // that mattered was reported when the credential was minted.
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let event: Value = format!("{stdout}{stderr}")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .find_map(|line| serde_json::from_str(line).ok())
        .expect("a protocol error event");
    assert_eq!(event["kind"], "error");
    assert_eq!(event["error"]["code"], "invalid_endpoint");
}

/// The delivery for a machine with no screen: no window, a listing instead.
///
/// This is the shape the remote shell needs — `afui session list` cannot offer
/// what nothing announced — and it is also the only shape a headless container
/// has, where the window delivery fails outright. What it must prove is that
/// the announcement is real while the command runs and gone when it is not,
/// because a listing that outlives its owner is exactly the ownerless zombie
/// AFUI's session criteria forbid.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_windowless_takeover_announces_the_panel_and_withdraws_it_on_the_way_out() {
    let upstream = spawn_fake_provider().await;
    let base = spawn_host(Some(HOST_TOKEN), upstream).await;
    let panel_url = mint_panel_url(&base, HOST_TOKEN).await;
    let registry = tempfile::tempdir().expect("tmp");
    let sessions = registry.path().join("sessions");

    let child = Command::new(env!("CARGO_BIN_EXE_afhttp"))
        .args([
            "ui",
            "takeover",
            "--takeover-url-secret",
            &panel_url,
            "--takeover-no-window",
        ])
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
    // The announced address is the one that actually works, credential and all
    // — an entry without it names a URL that answers 401.
    assert_eq!(entry["access_url_secret"], Value::String(panel_url.clone()));
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
    assert_eq!(progress(&events)["progress"]["delivery"], "listed");
    let done = result(&events);
    assert_eq!(done["result"]["delivery"], "listed");
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
}

/// A registry that cannot be written must not take the window with it.
///
/// The window is the delivery here; the listing is a bonus. Losing the bonus
/// silently would be the other failure — an agent believing its panel is
/// listed when it is not — so the silence is made observable instead: no
/// `session_id` on the ready event means nothing else can see this panel.
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
    assert!(
        progress(&events)["progress"].get("session_id").is_none(),
        "an unannounced panel must not claim a session id: {events:?}"
    );
    assert_eq!(result(&events)["result"]["outcome"], "closed");
    // And the window really opened on the panel.
    let opened = std::fs::read_to_string(stub_dir.path().join("url")).unwrap_or_default();
    assert_eq!(opened, panel_url);
}
