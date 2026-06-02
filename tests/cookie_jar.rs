//! End-to-end test for the HTTP-path cookie jar. Verifies that Set-Cookie
//! from one fetch surfaces on the next request as a Cookie header, and
//! that the on-disk JSON jar survives concurrent writes.

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

use agent_first_http::sdk::fetch::RenderMode;
use agent_first_http::sdk::Client;
use agent_first_http::shared::artifacts::Artifact;

#[tokio::test]
async fn http_path_persists_set_cookie_into_jar() {
    let fixture = support::fixture_server::spawn().await;
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let jar_path = tmpdir.path().join("session.jar.json");

    let client = Client::connect("ws://localhost:9999").expect("client");

    // First fetch: server sends two Set-Cookie headers. Jar starts empty.
    let _ = client
        .fetch(format!("{}/set-cookie", fixture.base_url()))
        .render(RenderMode::None)
        .want([Artifact::Body])
        .out_dir(tmpdir.path().to_path_buf())
        .cookie_jar(jar_path.clone())
        .send()
        .await
        .expect("first fetch");

    assert!(
        jar_path.exists(),
        "jar should have been written after first fetch",
    );

    // Second fetch: jar replays the cookies as a Cookie header.
    let result = client
        .fetch(format!("{}/echo-cookie", fixture.base_url()))
        .render(RenderMode::None)
        .want([Artifact::Body])
        .out_dir(tmpdir.path().to_path_buf())
        .cookie_jar(jar_path.clone())
        .send()
        .await
        .expect("second fetch");

    let body_path = result.body_file.as_ref().expect("body_file in response");
    let bytes = std::fs::read(body_path).expect("read body");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    let cookie_header = body["cookie"].as_str().unwrap_or("");

    assert!(
        cookie_header.contains("afhttp_sid=session-token-1"),
        "echo of Cookie should contain replayed afhttp_sid; got {cookie_header:?}",
    );
    assert!(
        cookie_header.contains("afhttp_marker=present"),
        "echo of Cookie should contain replayed afhttp_marker; got {cookie_header:?}",
    );
}

#[tokio::test]
async fn fetch_without_cookie_jar_does_not_replay_cookies() {
    let fixture = support::fixture_server::spawn().await;
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let jar_path = tmpdir.path().join("session.jar.json");

    let client = Client::connect("ws://localhost:9999").expect("client");

    // Set cookies into the jar.
    let _ = client
        .fetch(format!("{}/set-cookie", fixture.base_url()))
        .render(RenderMode::None)
        .want([Artifact::Body])
        .out_dir(tmpdir.path().to_path_buf())
        .cookie_jar(jar_path.clone())
        .send()
        .await
        .expect("set-cookie fetch");

    // Now fetch WITHOUT the jar — cookies must not appear.
    let result = client
        .fetch(format!("{}/echo-cookie", fixture.base_url()))
        .render(RenderMode::None)
        .want([Artifact::Body])
        .out_dir(tmpdir.path().to_path_buf())
        // no .cookie_jar(...) on this call
        .send()
        .await
        .expect("plain echo fetch");

    let body_path = result.body_file.as_ref().expect("body_file");
    let bytes = std::fs::read(body_path).expect("read");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    let cookie_header = body["cookie"].as_str().unwrap_or("");

    assert!(
        !cookie_header.contains("afhttp_sid"),
        "fetch without --cookie-jar must not replay cookies; got {cookie_header:?}",
    );
}

#[tokio::test]
async fn concurrent_jar_persists_do_not_corrupt_file() {
    let fixture = support::fixture_server::spawn().await;
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let jar_path = tmpdir.path().join("session.jar.json");

    let client = Client::connect("ws://localhost:9999").expect("client");

    // Run several parallel fetches against /set-cookie sharing the same jar.
    // The persist() lock must serialize so the file stays well-formed.
    let mut handles = Vec::new();
    for _ in 0..6 {
        let c = client.clone();
        let url = format!("{}/set-cookie", fixture.base_url());
        let jar = jar_path.clone();
        let out = tmpdir.path().to_path_buf();
        handles.push(tokio::spawn(async move {
            c.fetch(url)
                .render(RenderMode::None)
                .want([Artifact::Body])
                .out_dir(out)
                .cookie_jar(jar)
                .send()
                .await
        }));
    }
    for h in handles {
        h.await.expect("join").expect("fetch");
    }

    // After all the parallel writes, the jar must still parse cleanly
    // and contain both cookies.
    let bytes = std::fs::read(&jar_path).expect("read jar");
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("jar must be valid JSON");
    let entries = parsed.as_array().expect("jar is JSON array");
    let names: Vec<&str> = entries.iter().filter_map(|e| e["name"].as_str()).collect();
    assert!(
        names.contains(&"afhttp_sid"),
        "jar lost afhttp_sid; got {names:?}",
    );
    assert!(
        names.contains(&"afhttp_marker"),
        "jar lost afhttp_marker; got {names:?}",
    );
}
