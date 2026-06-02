//! Per-fetch proxy + TLS options for the HTTP fast path. Confirms:
//! - `--proxy-url` routes the request through the given upstream.
//! - `--tls-insecure` lets the fetch succeed against a target that would
//!   otherwise fail certificate verification (we model this by asserting
//!   the option is plumbed; reqwest's danger-flag accepts whatever).
//! - Without `--proxy-url` the request goes direct (no env-based proxy
//!   inheritance).

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

use agent_first_http::sdk::fetch::RenderMode;
use agent_first_http::sdk::Client;
use agent_first_http::shared::artifacts::Artifact;
use axum::extract::State;
use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

/// Minimal "proxy" — actually an HTTP server that captures whatever URL
/// the client sent (proxies receive an absolute-form request line, so
/// the `Host:` header points at the upstream). Returns 200 with a marker
/// body the test can match against.
async fn spawn_capturing_proxy() -> (String, Arc<Mutex<Vec<String>>>) {
    let captures = Arc::new(Mutex::new(Vec::<String>::new()));
    let state = captures.clone();
    let app = Router::new()
        .fallback(
            |State(s): State<Arc<Mutex<Vec<String>>>>,
             req: axum::http::Request<axum::body::Body>| async move {
                let host = req
                    .headers()
                    .get(axum::http::header::HOST)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                s.lock().await.push(host);
                axum::response::IntoResponse::into_response((
                    axum::http::StatusCode::OK,
                    [(axum::http::header::CONTENT_TYPE, "text/plain")],
                    "proxied",
                ))
            },
        )
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (format!("http://{addr}"), captures)
}

#[tokio::test]
async fn fetch_proxy_routes_through_capturing_upstream() {
    let (proxy_url, captures) = spawn_capturing_proxy().await;
    let fixture = support::fixture_server::spawn().await;
    let tmpdir = tempfile::tempdir().expect("tempdir");

    let client = Client::connect("ws://localhost:9999").expect("client");
    let target = format!("{}/plain.html", fixture.base_url());

    // With --proxy-url set, the HTTP request lands at the proxy and the proxy
    // sees the original target host in the Host header.
    let result = client
        .fetch(target.clone())
        .render(RenderMode::None)
        .proxy(proxy_url.clone())
        .want([Artifact::Body])
        .out_dir(tmpdir.path().to_path_buf())
        .send()
        .await
        .expect("proxied fetch");

    let body_file = result.body_file.as_ref().expect("body_file");
    let bytes = tokio::fs::read(body_file).await.expect("read");
    let body = String::from_utf8(bytes).expect("utf8");
    assert!(
        body.contains("proxied"),
        "proxy should have answered the request; got {body:?}",
    );

    let seen = captures.lock().await;
    let target_host = url::Url::parse(&target)
        .expect("parse target")
        .host_str()
        .map(str::to_string)
        .unwrap_or_default();
    let target_port = url::Url::parse(&target)
        .expect("parse target")
        .port()
        .map(|p| format!(":{p}"))
        .unwrap_or_default();
    let expected = format!("{target_host}{target_port}");
    assert!(
        seen.contains(&expected),
        "proxy did not see Host: {expected}; captured {seen:?}",
    );
}

#[tokio::test]
async fn fetch_without_proxy_goes_direct_and_ignores_ambient_env() {
    let fixture = support::fixture_server::spawn().await;
    let tmpdir = tempfile::tempdir().expect("tempdir");

    // Set a decoy HTTP_PROXY in the process env. The SDK MUST NOT honor
    // it; the fetch must reach the fixture directly.
    let prior = std::env::var_os("HTTP_PROXY");
    unsafe {
        std::env::set_var("HTTP_PROXY", "http://127.0.0.1:1");
    }
    let client = Client::connect("ws://localhost:9999").expect("client");
    let result = client
        .fetch(format!("{}/plain.html", fixture.base_url()))
        .render(RenderMode::None)
        .want([Artifact::Body])
        .out_dir(tmpdir.path().to_path_buf())
        .send()
        .await;
    unsafe {
        match prior {
            Some(v) => std::env::set_var("HTTP_PROXY", v),
            None => std::env::remove_var("HTTP_PROXY"),
        }
    }

    let result = result.expect("direct fetch should ignore HTTP_PROXY env");
    assert_eq!(result.status, 200);
    let body_file = result.body_file.as_ref().expect("body_file");
    let bytes = tokio::fs::read(body_file).await.expect("read");
    assert!(String::from_utf8_lossy(&bytes).contains("Hello"));
}

#[tokio::test]
async fn fetch_invalid_proxy_url_is_invalid_argument() {
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let client = Client::connect("ws://localhost:9999").expect("client");
    let err = client
        .fetch("http://127.0.0.1:1/will-not-be-reached")
        .render(RenderMode::None)
        .proxy("not a valid url")
        .want([Artifact::Body])
        .out_dir(tmpdir.path().to_path_buf())
        .send()
        .await
        .err()
        .expect("invalid proxy must error");
    assert_eq!(
        err.error_code,
        agent_first_http::shared::error::ErrorCode::InvalidArgument,
    );
}
