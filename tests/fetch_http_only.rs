//! Integration tests for the `--render none` (HTTP fast path) fetch.

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
use agent_first_http::shared::error::ErrorCode;

#[tokio::test]
async fn http_only_fetch_writes_body_artifact() {
    let fixture = support::fixture_server::spawn().await;
    let tmpdir = tempfile::tempdir().expect("tempdir");

    let client = Client::connect("ws://localhost:9999").expect("client");
    let result = client
        .fetch(format!("{}/plain.html", fixture.base_url()))
        .render(RenderMode::None)
        .want([Artifact::Body])
        .out_dir(tmpdir.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    assert_eq!(result.status, 200);
    assert!(
        result.final_url.ends_with("/plain.html"),
        "final_url = {}",
        result.final_url
    );
    assert!(
        result.warnings.is_empty(),
        "warnings: {:?}",
        result.warnings
    );

    let body_file = result.body_file.as_ref().expect("body_file in response");
    assert!(body_file.exists(), "{} should exist", body_file.display());
    assert_eq!(
        body_file.extension().and_then(|s| s.to_str()),
        Some("html"),
        "body should be saved as .html for text/html content-type",
    );
    let bytes = tokio::fs::read(body_file).await.expect("read");
    let text = String::from_utf8(bytes).expect("utf8");
    assert!(
        text.contains("Hello"),
        "body content was not preserved: {text:?}"
    );

    assert_eq!(
        result.trace.render_decision,
        agent_first_http::sdk::fetch::result::RenderDecision::HttpOnly
    );
    assert!(result.trace.main_request_observed);
    // The new convenience fields agents use for retry logic.
    assert!(
        !result.trace.render_used,
        "HTTP-only path must report render_used=false"
    );
    assert_eq!(
        result.trace.render_mode,
        agent_first_http::sdk::fetch::result::TraceRenderMode::None,
        "render_mode should echo the agent's --render none request",
    );
}

#[tokio::test]
async fn http_only_max_response_bytes_truncates_with_warning() {
    let fixture = support::fixture_server::spawn().await;
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let client = Client::connect("ws://localhost:9999").expect("client");

    // /large-body returns 128 KiB; cap at 32 KiB and expect a truncation
    // warning plus exactly the prefix on disk.
    let result = client
        .fetch(format!("{}/large-body", fixture.base_url()))
        .render(RenderMode::None)
        .max_response_bytes(32 * 1024)
        .want([Artifact::Body])
        .out_dir(tmpdir.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    assert_eq!(result.status, 200);
    let body_file = result.body_file.as_ref().expect("body_file");
    let on_disk = tokio::fs::read(body_file).await.expect("read");
    assert_eq!(
        on_disk.len(),
        32 * 1024,
        "truncated body must be exactly the cap size",
    );
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.code == ErrorCode::NetworkBodyTruncated),
        "expected network_body_truncated warning; got {:?}",
        result.warnings
    );
}

#[tokio::test]
async fn http_only_max_response_bytes_zero_disables_cap() {
    let fixture = support::fixture_server::spawn().await;
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let client = Client::connect("ws://localhost:9999").expect("client");

    let result = client
        .fetch(format!("{}/large-body", fixture.base_url()))
        .render(RenderMode::None)
        .max_response_bytes(0)
        .want([Artifact::Body])
        .out_dir(tmpdir.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    let body_file = result.body_file.as_ref().expect("body_file");
    let on_disk = tokio::fs::read(body_file).await.expect("read");
    assert_eq!(on_disk.len(), 128 * 1024, "cap=0 must store the full body");
    assert!(
        result
            .warnings
            .iter()
            .all(|w| w.code != ErrorCode::NetworkBodyTruncated),
        "cap=0 must not produce a truncation warning",
    );
}

#[tokio::test]
async fn http_only_fetch_chooses_json_extension() {
    let fixture = support::fixture_server::spawn().await;
    let tmpdir = tempfile::tempdir().expect("tempdir");

    let client = Client::connect("ws://localhost:9999").expect("client");
    let result = client
        .fetch(format!("{}/data.json", fixture.base_url()))
        .render(RenderMode::None)
        .want([Artifact::Body])
        .out_dir(tmpdir.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    let body_file = result.body_file.as_ref().expect("body_file");
    assert_eq!(body_file.extension().and_then(|s| s.to_str()), Some("json"),);
}

#[tokio::test]
async fn http_only_fetch_applies_request_overrides() {
    let fixture = support::fixture_server::spawn().await;
    let tmpdir = tempfile::tempdir().expect("tempdir");

    let client = Client::connect("ws://localhost:9999").expect("client");
    let result = client
        .fetch(format!("{}/headers.json", fixture.base_url()))
        .render(RenderMode::None)
        .want([Artifact::Body])
        .header("X-Afhttp-Test", "present")
        .user_agent("afhttp-test-agent/1")
        .cookie("sid", "abc")
        .cookie("theme", "light")
        .cookie_full(
            cookie::Cookie::build(("scoped", "yes"))
                .path("/headers")
                .http_only(true)
                .same_site(cookie::SameSite::Lax)
                .build(),
        )
        .out_dir(tmpdir.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    let body_file = result.body_file.as_ref().expect("body_file");
    let body = tokio::fs::read_to_string(body_file).await.expect("read");
    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(json["x-afhttp-test"], "present");
    assert_eq!(json["user-agent"], "afhttp-test-agent/1");
    assert_eq!(json["cookie"], "sid=abc; theme=light; scoped=yes");
}

#[tokio::test]
async fn header_user_agent_normalizes_to_user_agent_override() {
    let fixture = support::fixture_server::spawn().await;
    let tmpdir = tempfile::tempdir().expect("tempdir");

    let client = Client::connect("ws://localhost:9999").expect("client");
    let result = client
        .fetch(format!("{}/headers.json", fixture.base_url()))
        .render(RenderMode::None)
        .want([Artifact::Body])
        .header("User-Agent", "header-agent/1")
        .out_dir(tmpdir.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    let body_file = result.body_file.as_ref().expect("body_file");
    let body = tokio::fs::read_to_string(body_file).await.expect("read");
    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(json["user-agent"], "header-agent/1");
}

#[tokio::test]
async fn user_agent_header_conflict_is_invalid_argument() {
    let fixture = support::fixture_server::spawn().await;
    let tmpdir = tempfile::tempdir().expect("tempdir");

    let client = Client::connect("ws://localhost:9999").expect("client");
    let err = client
        .fetch(format!("{}/headers.json", fixture.base_url()))
        .render(RenderMode::None)
        .header("User-Agent", "header-agent/1")
        .user_agent("method-agent/1")
        .out_dir(tmpdir.path().to_path_buf())
        .send()
        .await
        .err()
        .expect("error");
    assert_eq!(err.error_code, ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn cookie_header_conflict_is_invalid_argument() {
    let fixture = support::fixture_server::spawn().await;
    let tmpdir = tempfile::tempdir().expect("tempdir");

    let client = Client::connect("ws://localhost:9999").expect("client");
    let err = client
        .fetch(format!("{}/headers.json", fixture.base_url()))
        .render(RenderMode::None)
        .header("Cookie", "raw=1")
        .cookie("sid", "abc")
        .out_dir(tmpdir.path().to_path_buf())
        .send()
        .await
        .err()
        .expect("error");
    assert_eq!(err.error_code, ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn full_cookie_path_mismatch_is_invalid_argument() {
    let fixture = support::fixture_server::spawn().await;
    let tmpdir = tempfile::tempdir().expect("tempdir");

    let client = Client::connect("ws://localhost:9999").expect("client");
    let err = client
        .fetch(format!("{}/headers.json", fixture.base_url()))
        .render(RenderMode::None)
        .cookie_full(
            cookie::Cookie::build(("scoped", "yes"))
                .path("/other")
                .build(),
        )
        .out_dir(tmpdir.path().to_path_buf())
        .send()
        .await
        .err()
        .expect("error");
    assert_eq!(err.error_code, ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn secure_cookie_on_http_url_is_skipped_not_invalid() {
    let fixture = support::fixture_server::spawn().await;
    let tmpdir = tempfile::tempdir().expect("tempdir");

    let client = Client::connect("ws://localhost:9999").expect("client");
    let result = client
        .fetch(format!("{}/headers.json", fixture.base_url()))
        .render(RenderMode::None)
        .want([Artifact::Body])
        .cookie_full(
            cookie::Cookie::build(("secure_only", "secret"))
                .secure(true)
                .build(),
        )
        .out_dir(tmpdir.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    let body_file = result.body_file.as_ref().expect("body_file");
    let body = tokio::fs::read_to_string(body_file).await.expect("read");
    let json: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert!(json["cookie"].is_null(), "secure cookie leaked over HTTP");
}

#[tokio::test]
async fn evaluate_after_wait_requires_browser_path() {
    let fixture = support::fixture_server::spawn().await;
    let tmpdir = tempfile::tempdir().expect("tempdir");

    let client = Client::connect("ws://localhost:9999").expect("client");
    let err = client
        .fetch(format!("{}/plain.html", fixture.base_url()))
        .render(RenderMode::None)
        .evaluate_after_wait("document.body.dataset.x = '1'")
        .out_dir(tmpdir.path().to_path_buf())
        .send()
        .await
        .err()
        .expect("error");
    assert_eq!(err.error_code, ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn http_only_fetch_follows_redirect() {
    let fixture = support::fixture_server::spawn().await;
    let tmpdir = tempfile::tempdir().expect("tempdir");

    let client = Client::connect("ws://localhost:9999").expect("client");
    let result = client
        .fetch(format!("{}/redirect", fixture.base_url()))
        .render(RenderMode::None)
        .out_dir(tmpdir.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    assert_eq!(result.status, 200);
    assert!(
        result.final_url.ends_with("/plain.html"),
        "redirect should land on /plain.html, got {}",
        result.final_url,
    );
}

#[tokio::test]
async fn http_only_fetch_records_404_status() {
    let fixture = support::fixture_server::spawn().await;
    let tmpdir = tempfile::tempdir().expect("tempdir");

    let client = Client::connect("ws://localhost:9999").expect("client");
    let result = client
        .fetch(format!("{}/404", fixture.base_url()))
        .render(RenderMode::None)
        .out_dir(tmpdir.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    assert_eq!(
        result.status, 404,
        "404 is not an error; it's a structured fact"
    );
}

#[tokio::test]
async fn http_only_fetch_on_unreachable_host_returns_target_unreachable() {
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let client = Client::connect("ws://localhost:9999").expect("client");
    let err = client
        .fetch("http://127.0.0.1:1") // port 1 is reserved; connect fails fast
        .render(RenderMode::None)
        .out_dir(tmpdir.path().to_path_buf())
        .send()
        .await
        .err()
        .expect("expected an error");
    assert_eq!(
        err.error_code,
        agent_first_http::shared::error::ErrorCode::TargetUnreachable,
        "got {err:?}"
    );
    assert!(err.retryable, "target_unreachable should be retryable");
}

#[tokio::test]
async fn cli_render_none_without_endpoint_does_not_require_browser_env() {
    let fixture = support::fixture_server::spawn().await;
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_afhttp"))
        .arg("fetch")
        .arg(format!("{}/plain.html", fixture.base_url()))
        .arg("--render")
        .arg("none")
        .arg("--want")
        .arg("body")
        .arg("--out")
        .arg(tmpdir.path())
        .env_remove("AFHTTP_TEST_BROWSER_BIN")
        .output()
        .await
        .expect("run afhttp");
    assert!(
        output.status.success(),
        "afhttp failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(body["code"], "fetch");
    assert_eq!(body["trace"]["render_decision"], "http_only");
    assert!(body.get("artifacts").is_none(), "fetch output must be flat");
    let body_file = body["body_file"].as_str().expect("body_file");
    assert!(
        std::path::Path::new(body_file).is_absolute(),
        "body_file must be absolute: {body_file}"
    );
}
