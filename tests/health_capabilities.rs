//! Integration tests for the host's `/health` and `/capabilities` HTTP
//! endpoints plus bearer-token middleware.

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
use agent_first_http::host::listener::{router_for_tests, test_state, AppState};
use agent_first_http::sdk::Client;
use tokio::net::TcpListener;

/// Spawn the host listener on a random TCP port and return its base URL.
/// The task is left to run until the test exits (axum::serve is dropped
/// when the listener is closed).
async fn spawn_host(token: Option<&str>, health_public: HealthPublic) -> String {
    support::ensure_rustls_provider();
    let state = test_state(token, health_public);
    serve_state(state).await
}

async fn serve_state(state: AppState) -> String {
    let app = router_for_tests(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    // Tiny grace period so the bound socket is ready for connect().
    tokio::time::sleep(Duration::from_millis(20)).await;
    format!("http://{addr}")
}

#[tokio::test]
async fn health_reports_starting_when_no_backend() {
    let base = spawn_host(None, HealthPublic::Off).await;
    let client = Client::connect(&base).expect("client");
    let response = client.health().await.expect("health");
    assert_eq!(response.code, "health");
    assert_eq!(response.status, "starting");
    assert_eq!(response.version, env!("CARGO_PKG_VERSION"));
    assert!(response.backend.is_none());
    assert!(response.backend_error.is_none());
    assert!(response.profile.is_none());
    assert_eq!(response.capabilities_url.as_deref(), Some("/capabilities"));
}

#[tokio::test]
async fn health_reports_degraded_when_backend_cdp_is_unreachable() {
    support::ensure_rustls_provider();
    let profile_tmp = tempfile::tempdir().expect("profile tmp");
    let state = test_state(None, HealthPublic::Off)
        .with_persistent_profile("work", profile_tmp.path().to_path_buf());
    let base = serve_state(state).await;
    let client = Client::connect(&base).expect("client");
    let response = client.health().await.expect("health");
    assert_eq!(response.status, "degraded");
    assert_eq!(response.tabs_active, 0);
    let backend = response.backend.expect("backend");
    assert!(!backend.connected);
    let backend_error = response.backend_error.expect("backend_error");
    assert_eq!(
        backend_error.error_code,
        agent_first_http::shared::error::ErrorCode::CdpUnavailable
    );
    let profile = response.profile.expect("profile");
    assert_eq!(profile.name.as_deref(), Some("work"));
}

#[tokio::test]
async fn capabilities_reports_browser_artifacts_unsupported_when_no_backend() {
    let base = spawn_host(None, HealthPublic::Off).await;
    let client = Client::connect(&base).expect("client");
    let response = client.capabilities().await.expect("capabilities");
    assert_eq!(response.code, "capabilities");
    // Until the backend is connected, host-provided artifacts are unavailable.
    assert!(!response.artifacts["body"].supported);
    assert!(!response.artifacts["network"].supported);
    assert!(!response.artifacts["screenshot"].supported);
    assert!(!response.artifacts["rendered_html"].supported);
    assert!(!response.artifacts["observation"].supported);
    assert_eq!(
        response.artifacts["observation"].source.as_deref(),
        Some("accessibility+dom")
    );
    assert!(response.artifacts["network"].body_capture.is_empty());
    assert!(response.artifacts.contains_key("storage"));
    assert!(response.features.contains_key("selector_visible"));
    assert!(response.features.contains_key("capture_ws"));
    assert!(response.features.contains_key("capture_sse"));
    assert!(response.features.contains_key("recent_requests"));
    assert!(response
        .wait_modes
        .contains(&"selector_visible".to_string()));
}

#[tokio::test]
async fn token_required_when_configured() {
    let base = spawn_host(Some("hunter2"), HealthPublic::Off).await;

    // No token → 401.
    let raw = reqwest::Client::new()
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("send");
    assert_eq!(raw.status(), reqwest::StatusCode::UNAUTHORIZED);

    // Wrong token → 401.
    let raw = reqwest::Client::new()
        .get(format!("{base}/health"))
        .bearer_auth("nope")
        .send()
        .await
        .expect("send");
    assert_eq!(raw.status(), reqwest::StatusCode::UNAUTHORIZED);

    // Right token → 200.
    let client = Client::connect(&base)
        .expect("client")
        .with_token("hunter2");
    let r = client.health().await.expect("health");
    assert_eq!(r.status, "starting");
}

#[tokio::test]
async fn public_minimal_health_allows_unauthenticated_health() {
    let base = spawn_host(Some("hunter2"), HealthPublic::Minimal).await;

    // No token → 200 with minimal payload.
    let r = reqwest::Client::new()
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = r.json().await.expect("json");
    assert_eq!(body["code"], "health");
    // Minimal payload hides backend + profile + capabilities_url.
    assert!(body.get("backend").is_none_or(|v| v.is_null()));
    assert!(body.get("profile").is_none_or(|v| v.is_null()));

    // /capabilities still requires a token.
    let r2 = reqwest::Client::new()
        .get(format!("{base}/capabilities"))
        .send()
        .await
        .expect("send");
    assert_eq!(r2.status(), reqwest::StatusCode::UNAUTHORIZED);

    // A valid token upgrades /health to the full payload (capabilities_url
    // appears in the authenticated response but not the minimal one).
    let full = reqwest::Client::new()
        .get(format!("{base}/health"))
        .bearer_auth("hunter2")
        .send()
        .await
        .expect("send");
    assert_eq!(full.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = full.json().await.expect("json");
    assert!(
        body.get("capabilities_url").is_some_and(|v| !v.is_null()),
        "full health payload must include capabilities_url; got {body}",
    );
}

#[tokio::test]
async fn token_via_query_parameter_is_accepted() {
    let base = spawn_host(Some("abc"), HealthPublic::Off).await;
    let r = reqwest::Client::new()
        .get(format!("{base}/health?token_secret=abc"))
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn token_query_parameter_is_percent_decoded() {
    let token = "a+b&c%20";
    let encoded: String = url::form_urlencoded::byte_serialize(token.as_bytes()).collect();
    let base = spawn_host(Some(token), HealthPublic::Off).await;
    let r = reqwest::Client::new()
        .get(format!("{base}/health?token_secret={encoded}"))
        .send()
        .await
        .expect("send");
    assert_eq!(r.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn sdk_health_decodes_error_envelope_on_non_2xx() {
    let base = spawn_host(Some("hunter2"), HealthPublic::Off).await;
    let err = Client::connect(&base)
        .expect("client")
        .with_token("wrong")
        .health()
        .await
        .err()
        .expect("health error");
    assert_eq!(
        err.error_code,
        agent_first_http::shared::error::ErrorCode::InvalidArgument
    );
}
