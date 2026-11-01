//! Browser-path request shaping: caller credential headers reach the target
//! origin and no other, and a form POST keeps the caller's other headers.
//! Requires a chromium binary; skipped otherwise.

#![cfg(feature = "host")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods,
    clippy::disallowed_macros,
    clippy::print_stdout
)]

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_first_http::host::bootstrap::{
    BrowserChoice, DisplayMode, HealthPublic, HostArgs, ProfileChoice, Takeover,
};
use agent_first_http::host::{browser, listener::router_for_tests, listener::test_state};
use agent_first_http::sdk::Client;
use agent_first_http::sdk::fetch::{RenderMode, Wait};
use agent_first_http::shared::artifacts::Artifact;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, Uri, header};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
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
        tab_idle_timeout_s: 0,
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
    Some((format!("ws://{addr}"), tempfile::tempdir().expect("tmpdir")))
}

/// `(path, headers)` for every request a fixture origin received.
type Seen = Arc<Mutex<Vec<(String, HeaderMap)>>>;

#[derive(Clone)]
struct Origin {
    seen: Seen,
    other_origin: String,
}

async fn record(State(origin): State<Origin>, uri: Uri, headers: HeaderMap) -> impl IntoResponse {
    origin
        .seen
        .lock()
        .unwrap()
        .push((uri.path().to_string(), headers));
    match uri.path() {
        "/page" => Html(format!(
            "<!doctype html><html><body><h1>Scoped headers</h1>\
             <img src=\"{}/pixel\">\
             <script>fetch('/api').then(r => r.text()).then(t => \
               document.body.setAttribute('data-api', t));</script>\
             </body></html>",
            origin.other_origin
        ))
        .into_response(),
        "/form" => Html("<!doctype html><html><body><p>posted</p></body></html>").into_response(),
        _ => ([(header::CONTENT_TYPE, "text/plain")], "ok").into_response(),
    }
}

async fn spawn_origin(other_origin: String, seen: Seen) -> String {
    let app = Router::new()
        .route("/page", get(record))
        .route("/api", get(record))
        .route("/pixel", get(record))
        .route("/form", post(record))
        .with_state(Origin { seen, other_origin });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    base
}

fn headers_for(seen: &Seen, path: &str) -> HeaderMap {
    seen.lock()
        .unwrap()
        .iter()
        .find(|(p, _)| p == path)
        .map(|(_, h)| h.clone())
        .unwrap_or_else(|| panic!("{path} was never requested"))
}

#[tokio::test]
async fn credential_headers_reach_the_target_origin_only() {
    let Some((endpoint, tmp)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let third_party_seen = Seen::default();
    let third_party = spawn_origin(String::new(), third_party_seen.clone()).await;
    let target_seen = Seen::default();
    let target = spawn_origin(third_party, target_seen.clone()).await;

    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch(format!("{target}/page"))
        .render(RenderMode::Always)
        .wait(Wait::Idle)
        .timeout(Duration::from_secs(15))
        .want([Artifact::RenderedHtml])
        .header("Authorization", "Bearer caller-secret")
        .header("X-Afhttp-Trace", "trace-1")
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");
    assert_eq!(result.status, 200, "{:?}", result.warnings);

    for path in ["/page", "/api"] {
        let headers = headers_for(&target_seen, path);
        assert_eq!(
            headers.get(header::AUTHORIZATION).unwrap(),
            "Bearer caller-secret",
            "{path}"
        );
        assert_eq!(headers.get("x-afhttp-trace").unwrap(), "trace-1", "{path}");
    }
    let pixel = headers_for(&third_party_seen, "/pixel");
    assert!(pixel.get(header::AUTHORIZATION).is_none(), "{pixel:?}");
    assert_eq!(pixel.get("x-afhttp-trace").unwrap(), "trace-1");
}

#[tokio::test]
async fn form_post_keeps_the_callers_other_headers() {
    let Some((endpoint, tmp)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let seen = Seen::default();
    let target = spawn_origin(String::new(), seen.clone()).await;
    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch(format!("{target}/form"))
        .render(RenderMode::Always)
        .wait(Wait::Load)
        .timeout(Duration::from_secs(15))
        .want([Artifact::RenderedHtml])
        .method("POST")
        .form_field("account", "a b")
        .header("Authorization", "Bearer caller-secret")
        .header("X-Afhttp-Trace", "trace-2")
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");
    assert_eq!(result.status, 200, "{:?}", result.warnings);
    let headers = headers_for(&seen, "/form");
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "application/x-www-form-urlencoded"
    );
    assert_eq!(headers.get("x-afhttp-trace").unwrap(), "trace-2");
    assert_eq!(
        headers.get(header::AUTHORIZATION).unwrap(),
        "Bearer caller-secret"
    );
}
