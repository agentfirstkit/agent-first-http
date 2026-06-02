//! Isolation invariant tests for the cookie jar.
//!
//! These tests stand up a real host listener (with the new `/profile`
//! endpoint), spawn a Client against it, and verify:
//! - The default cookie-jar path is `<profile-dir>/cookies.jar.json`.
//! - An explicit `--cookie-jar` that does NOT match the host's profile
//!   is rejected with `invalid_argument` before any network I/O.
//! - Two hosts with different profiles share no cookie state.

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

use std::time::Duration;

use agent_first_http::host::bootstrap::HealthPublic;
use agent_first_http::host::listener::{router_for_tests, test_state};
use agent_first_http::sdk::fetch::RenderMode;
use agent_first_http::sdk::Client;
use agent_first_http::shared::artifacts::Artifact;
use agent_first_http::shared::error::ErrorCode;
use tokio::net::TcpListener;

/// Spin up a minimal test host whose AppState reports `profile_path` =
/// `profile_dir`. No real chromium is launched — these tests exercise
/// only the HTTP path against the fixture server, which goes through the
/// SDK and never needs a backend.
async fn spawn_test_host(profile_dir: std::path::PathBuf) -> String {
    support::ensure_rustls_provider();
    let state =
        test_state(None, HealthPublic::Off).with_persistent_profile("isolation-test", profile_dir);
    let app = router_for_tests(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    format!("http://{addr}")
}

#[tokio::test]
async fn default_cookie_jar_lives_inside_profile_dir() {
    let fixture = support::fixture_server::spawn().await;
    let profile_tmp = tempfile::tempdir().expect("profile tmp");
    let endpoint = spawn_test_host(profile_tmp.path().to_path_buf()).await;
    let out_tmp = tempfile::tempdir().expect("out tmp");

    let client = Client::connect(&endpoint).expect("client");

    // First fetch with no explicit jar — should default to <profile>/cookies.jar.json.
    let _ = client
        .fetch(format!("{}/set-cookie", fixture.base_url()))
        .render(RenderMode::None)
        .want([Artifact::Body])
        .out_dir(out_tmp.path().to_path_buf())
        .send()
        .await
        .expect("first fetch");

    let expected_jar = profile_tmp.path().join("cookies.jar.json");
    assert!(
        expected_jar.exists(),
        "default jar must land inside the profile dir at {}",
        expected_jar.display()
    );

    // Second fetch through the same client must replay the cookies.
    let r = client
        .fetch(format!("{}/echo-cookie", fixture.base_url()))
        .render(RenderMode::None)
        .want([Artifact::Body])
        .out_dir(out_tmp.path().to_path_buf())
        .send()
        .await
        .expect("second fetch");
    let body_path = r.body_file.as_ref().expect("body_file");
    let bytes = std::fs::read(body_path).expect("read body");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    let cookie_header = body["cookie"].as_str().unwrap_or("");
    assert!(
        cookie_header.contains("afhttp_sid"),
        "default jar should replay session cookies; got {cookie_header:?}",
    );
}

#[tokio::test]
async fn explicit_cookie_jar_outside_profile_is_rejected() {
    let fixture = support::fixture_server::spawn().await;
    let profile_tmp = tempfile::tempdir().expect("profile tmp");
    let foreign_tmp = tempfile::tempdir().expect("foreign tmp");
    let endpoint = spawn_test_host(profile_tmp.path().to_path_buf()).await;
    let out_tmp = tempfile::tempdir().expect("out tmp");

    let client = Client::connect(&endpoint).expect("client");
    let stranger_jar = foreign_tmp.path().join("their-cookies.jar.json");

    let err = client
        .fetch(format!("{}/set-cookie", fixture.base_url()))
        .render(RenderMode::None)
        .want([Artifact::Body])
        .out_dir(out_tmp.path().to_path_buf())
        .cookie_jar(stranger_jar.clone())
        .send()
        .await
        .err()
        .expect("expected invalid_argument");
    assert_eq!(err.error_code, ErrorCode::InvalidArgument);
    assert!(
        err.detail.contains("does not match"),
        "rejection detail should explain the mismatch: {}",
        err.detail
    );

    // The stranger jar must NOT have been written.
    assert!(
        !stranger_jar.exists(),
        "the rejection must prevent any write to the foreign path: {}",
        stranger_jar.display()
    );
}

#[tokio::test]
async fn explicit_cookie_jar_matching_profile_is_accepted() {
    let fixture = support::fixture_server::spawn().await;
    let profile_tmp = tempfile::tempdir().expect("profile tmp");
    let endpoint = spawn_test_host(profile_tmp.path().to_path_buf()).await;
    let out_tmp = tempfile::tempdir().expect("out tmp");

    let client = Client::connect(&endpoint).expect("client");
    let matching = profile_tmp.path().join("cookies.jar.json");

    let _ = client
        .fetch(format!("{}/set-cookie", fixture.base_url()))
        .render(RenderMode::None)
        .want([Artifact::Body])
        .out_dir(out_tmp.path().to_path_buf())
        .cookie_jar(matching.clone())
        .send()
        .await
        .expect("explicit-match jar should be accepted");
    assert!(matching.exists());
}

#[tokio::test]
async fn no_cookie_jar_flag_disables_persistence_even_with_profile() {
    let fixture = support::fixture_server::spawn().await;
    let profile_tmp = tempfile::tempdir().expect("profile tmp");
    let endpoint = spawn_test_host(profile_tmp.path().to_path_buf()).await;
    let out_tmp = tempfile::tempdir().expect("out tmp");

    let client = Client::connect(&endpoint).expect("client");

    // Fetch that would normally write the jar — but we opt out.
    let _ = client
        .fetch(format!("{}/set-cookie", fixture.base_url()))
        .render(RenderMode::None)
        .want([Artifact::Body])
        .out_dir(out_tmp.path().to_path_buf())
        .no_cookie_jar()
        .send()
        .await
        .expect("no-jar fetch");

    let jar_path = profile_tmp.path().join("cookies.jar.json");
    assert!(
        !jar_path.exists(),
        "no_cookie_jar must prevent jar creation; found {}",
        jar_path.display()
    );
}

#[tokio::test]
async fn cookies_do_not_bleed_across_profiles() {
    let fixture = support::fixture_server::spawn().await;
    let profile_a = tempfile::tempdir().expect("profile A");
    let profile_b = tempfile::tempdir().expect("profile B");
    let endpoint_a = spawn_test_host(profile_a.path().to_path_buf()).await;
    let endpoint_b = spawn_test_host(profile_b.path().to_path_buf()).await;
    let out_tmp = tempfile::tempdir().expect("out tmp");

    // Drop a session into profile A.
    let client_a = Client::connect(&endpoint_a).expect("client A");
    let _ = client_a
        .fetch(format!("{}/set-cookie", fixture.base_url()))
        .render(RenderMode::None)
        .want([Artifact::Body])
        .out_dir(out_tmp.path().to_path_buf())
        .send()
        .await
        .expect("A set-cookie");

    // Read from profile B — should see no cookies.
    let client_b = Client::connect(&endpoint_b).expect("client B");
    let r = client_b
        .fetch(format!("{}/echo-cookie", fixture.base_url()))
        .render(RenderMode::None)
        .want([Artifact::Body])
        .out_dir(out_tmp.path().to_path_buf())
        .send()
        .await
        .expect("B echo");
    let body_path = r.body_file.as_ref().expect("body_file");
    let bytes = std::fs::read(body_path).expect("read");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    let cookie_header = body["cookie"].as_str().unwrap_or("");
    assert!(
        !cookie_header.contains("afhttp_sid"),
        "profile B must not see profile A's session; got {cookie_header:?}",
    );

    // Sanity: profile A's jar still has the cookie; profile B has no jar.
    assert!(profile_a.path().join("cookies.jar.json").exists());
    assert!(!profile_b.path().join("cookies.jar.json").exists());
}
