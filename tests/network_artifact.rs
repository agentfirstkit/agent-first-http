//! N1 + N3 integration tests: real Network.* event aggregation and
//! `--network-bodies xhr|all` response body capture.

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
use agent_first_http::sdk::fetch::{NetworkBodies, RenderMode, Wait};
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

fn read_log(path: &std::path::Path) -> Value {
    let s = std::fs::read_to_string(path).expect("read log");
    serde_json::from_str(&s).expect("parse log json")
}

#[tokio::test]
async fn network_artifact_captures_multiple_entries() {
    let Some((endpoint, tmp)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch(format!("{}/xhr.html", fixture.base_url()))
        .render(RenderMode::Always)
        .wait(Wait::Idle)
        .timeout(Duration::from_secs(10))
        .want([Artifact::Network])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    let net_path = result.network_file.as_ref().expect("network_file");
    let log = read_log(net_path);
    let entries = log["entries"].as_array().expect("entries array");
    assert!(
        entries.len() >= 2,
        "expected document + xhr entries, got {}: {entries:?}",
        entries.len()
    );

    // Document entry
    let doc = entries
        .iter()
        .find(|e| e["url"].as_str().unwrap_or("").ends_with("/xhr.html"))
        .expect("document entry present");
    assert_eq!(doc["status"], 200);
    let doc_mime = doc["mime_type"].as_str().unwrap_or("");
    assert!(doc_mime.starts_with("text/html"), "mime = {doc_mime}");

    // XHR entry
    let xhr = entries
        .iter()
        .find(|e| e["url"].as_str().unwrap_or("").ends_with("/data.json"))
        .expect("data.json entry present");
    let xhr_type = xhr["resource_type"].as_str().unwrap_or("");
    assert!(
        matches!(xhr_type, "XHR" | "Fetch"),
        "resource_type = {xhr_type}"
    );
    assert_eq!(xhr["status"], 200);
}

#[tokio::test]
async fn network_artifact_redacts_credential_headers_by_default() {
    let Some((endpoint, tmp)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let client = Client::connect(&endpoint).expect("client");
    // Plain page is enough — browser sends Cookie if any, headers reach the
    // aggregator either way and we can grep the JSON output.
    let result = client
        .fetch(format!("{}/plain.html", fixture.base_url()))
        .render(RenderMode::Always)
        .wait(Wait::Load)
        .timeout(Duration::from_secs(10))
        .want([Artifact::Network])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    let net_path = result.network_file.as_ref().expect("network_file");
    let raw = std::fs::read_to_string(net_path).expect("read");
    // None of the credential-bearing header names should appear with
    // their original values; if present, the value must be "[redacted]".
    for needle in [
        "\"Cookie\"",
        "\"cookie\"",
        "\"Authorization\"",
        "\"authorization\"",
    ] {
        if let Some(idx) = raw.find(needle) {
            // Inspect the value following the colon.
            let tail = &raw[idx..];
            assert!(
                tail.contains("[redacted]"),
                "found {needle} but value not redacted near: {}...",
                &tail[..tail.len().min(120)]
            );
        }
    }
}

#[tokio::test]
async fn network_bodies_xhr_captures_xhr_payload_only() {
    let Some((endpoint, tmp)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch(format!("{}/xhr.html", fixture.base_url()))
        .render(RenderMode::Always)
        .wait(Wait::Idle)
        .timeout(Duration::from_secs(10))
        .want([Artifact::Network])
        .network_bodies(NetworkBodies::Xhr)
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    let net_path = result.network_file.as_ref().expect("network_file");
    let log = read_log(net_path);
    let entries = log["entries"].as_array().expect("entries array");

    // Exactly one body file should exist: the data.json XHR.
    let xhr = entries
        .iter()
        .find(|e| e["url"].as_str().unwrap_or("").ends_with("/data.json"))
        .expect("xhr entry");
    let body_file = xhr["body_file"]
        .as_str()
        .expect("XHR entry should have body_file");
    let content = std::fs::read_to_string(body_file).expect("read body");
    assert!(
        content.contains("hello") && content.contains("world"),
        "body content = {content}"
    );

    // Document should NOT have a body_file under xhr mode.
    let doc = entries
        .iter()
        .find(|e| e["url"].as_str().unwrap_or("").ends_with("/xhr.html"))
        .expect("doc entry");
    assert!(
        doc["body_file"].is_null() || doc.get("body_file").is_none(),
        "doc entry should not have body_file under --network-bodies xhr: {doc}"
    );
}

#[tokio::test]
async fn network_bodies_all_captures_document_too() {
    let Some((endpoint, tmp)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch(format!("{}/xhr.html", fixture.base_url()))
        .render(RenderMode::Always)
        .wait(Wait::Idle)
        .timeout(Duration::from_secs(10))
        .want([Artifact::Network])
        .network_bodies(NetworkBodies::All)
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    let net_path = result.network_file.as_ref().expect("network_file");
    let log = read_log(net_path);
    let entries = log["entries"].as_array().expect("entries array");
    let doc = entries
        .iter()
        .find(|e| e["url"].as_str().unwrap_or("").ends_with("/xhr.html"))
        .expect("doc entry");
    let body_file = doc["body_file"]
        .as_str()
        .expect("doc entry should have body_file under --network-bodies all");
    let content = std::fs::read_to_string(body_file).expect("read");
    assert!(
        content.contains("fetch('/data.json')"),
        "body should be the HTML document, got {content}"
    );
}

#[tokio::test]
async fn network_bodies_off_writes_no_files() {
    let Some((endpoint, tmp)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let client = Client::connect(&endpoint).expect("client");
    let _ = client
        .fetch(format!("{}/xhr.html", fixture.base_url()))
        .render(RenderMode::Always)
        .wait(Wait::Idle)
        .timeout(Duration::from_secs(10))
        .want([Artifact::Network])
        .network_bodies(NetworkBodies::Off)
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    // No network-bodies directory created when mode = off.
    let candidate = std::fs::read_dir(tmp.path())
        .expect("dir")
        .flatten()
        .find(|e| e.path().is_dir());
    if let Some(req_dir) = candidate {
        let nb = req_dir.path().join("network-bodies");
        assert!(
            !nb.exists(),
            "network-bodies/ should not exist under --network-bodies off"
        );
    }
}

#[tokio::test]
async fn console_artifact_captures_log_and_exception() {
    let Some((endpoint, tmp)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch(format!("{}/console.html", fixture.base_url()))
        .render(RenderMode::Always)
        .wait(Wait::Ms(200))
        .timeout(Duration::from_secs(10))
        .want([Artifact::Console])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    let console_file = result.console_file.as_ref().expect("console_file");
    let log = read_log(console_file);
    assert_eq!(log["schema_version"], 1);
    let events = log["events"].as_array().expect("events");
    let mut saw_log = false;
    let mut saw_exception = false;
    for ev in events {
        let level = ev["level"].as_str().unwrap_or("");
        let text = ev["text"].as_str().unwrap_or("");
        if level == "log" && text.contains("hello from fixture") {
            saw_log = true;
        }
        if level == "exception" && text.contains("boom from fixture") {
            saw_exception = true;
        }
    }
    assert!(
        saw_log,
        "expected a log event with 'hello from fixture'; events={events:?}",
    );
    assert!(
        saw_exception,
        "expected an exception event with 'boom from fixture'; events={events:?}",
    );
}
