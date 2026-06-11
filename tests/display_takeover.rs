//! Display-takeover routing and capability tests. Most tests use a tiny fake
//! KasmVNC upstream so the proxy/token/path behavior stays deterministic; the
//! real KasmVNC launch smoke is ignored and run by `tests/test.sh takeover`.

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
    TakeoverProviderKind,
};
use agent_first_http::host::browser::BrowserHandle;
use agent_first_http::host::listener::{router_for_tests, test_state, AppState};
use agent_first_http::shared::error::ErrorCode;
use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::http::{HeaderMap, Uri};
use axum::response::IntoResponse;
use axum::routing::get;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::TcpListener;

async fn spawn_fake_kasm() -> u16 {
    let app = axum::Router::new()
        .route("/", get(|| async { "fake kasmvnc" }))
        .route("/echo", get(echo_request))
        .route("/ws", get(fake_ws));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind fake");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    port
}

async fn echo_request(uri: Uri, headers: HeaderMap) -> impl IntoResponse {
    axum::Json(json!({
        "path_and_query": uri.path_and_query().map(|pq| pq.as_str()).unwrap_or(""),
        "saw_cookie": headers.get(axum::http::header::COOKIE).is_some(),
        "saw_authorization": headers.get(axum::http::header::AUTHORIZATION).is_some(),
    }))
}

async fn fake_ws(ws: WebSocketUpgrade) -> impl IntoResponse {
    // Mirror KasmVNC/websockify: agree to the `binary` subprotocol the proxy
    // now requests on the upstream leg.
    ws.protocols(["binary"]).on_upgrade(|socket| async move {
        let (mut tx, mut rx) = socket.split();
        while let Some(Ok(msg)) = rx.next().await {
            if let Message::Text(text) = msg {
                let _ = tx.send(Message::Text(format!("echo:{text}").into())).await;
            }
        }
    })
}

async fn spawn_display_router(token: Option<&str>, upstream_port: u16) -> String {
    support::ensure_rustls_provider();
    let state = test_state(token, HealthPublic::Off).with_takeover_for_tests(upstream_port);
    let app = router_for_tests(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind host");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    format!("http://{addr}")
}

async fn spawn_screencast_only_router(token: Option<&str>) -> String {
    support::ensure_rustls_provider();
    let state = test_state(token, HealthPublic::Off);
    let app = router_for_tests(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind host");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    format!("http://{addr}")
}

async fn takeover_handoff_url(base: &str, token: &str) -> String {
    let body = reqwest::Client::new()
        .post(format!("{base}/takeover/handoff"))
        .bearer_auth(token)
        .json(&json!({}))
        .send()
        .await
        .expect("handoff send")
        .json::<serde_json::Value>()
        .await
        .expect("handoff json");
    let url = body["takeover_url"].as_str().expect("takeover_url");
    assert!(url.contains("handoff="), "{body}");
    assert!(body["takeover_url_ttl_s"].as_u64().unwrap_or_default() > 0);
    url.to_string()
}

#[tokio::test]
async fn display_route_is_provider_neutral_and_unavailable_without_provider() {
    let base = spawn_screencast_only_router(None).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/takeover/panel"))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn takeover_handoff_is_unavailable_without_display_provider() {
    let base = spawn_screencast_only_router(Some("secret")).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/takeover/handoff"))
        .bearer_auth("secret")
        .json(&json!({}))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let body = resp.json::<serde_json::Value>().await.expect("json");
    assert_eq!(body["error_code"], "backend_unsupported");
}

#[tokio::test]
async fn display_proxy_rewrites_paths_strips_auth_and_accepts_takeover_cookie() {
    let upstream_port = spawn_fake_kasm().await;
    let base = spawn_display_router(Some("secret"), upstream_port).await;
    let no_redirect = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client");
    let takeover_url = takeover_handoff_url(&base, "secret").await;

    let redirected = no_redirect
        .get(&takeover_url)
        .send()
        .await
        .expect("redirect");
    assert_eq!(redirected.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
    // The redirect seeds noVNC's `path` (so its websocket targets the proxied
    // prefix instead of a root-level `/websockify`) and `resize` settings,
    // appends quality params, and drops the one-time handoff query after
    // setting the takeover cookie.
    let location = redirected
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .expect("location header");
    assert!(
        location.starts_with("/takeover/panel/?path=takeover/panel/websockify&resize=scale"),
        "unexpected redirect target: {location}"
    );
    assert!(
        location.contains("&max_video_resolution_x="),
        "missing quality params: {location}"
    );
    assert!(!location.contains("handoff="), "handoff leaked: {location}");
    let cookie = redirected
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("set-cookie")
        .split(';')
        .next()
        .expect("cookie pair")
        .to_string();
    assert!(cookie.starts_with("afhttp_handoff="));

    let through_cookie = reqwest::Client::new()
        .get(format!("{base}/takeover/panel/echo?x=1"))
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await
        .expect("cookie auth")
        .json::<serde_json::Value>()
        .await
        .expect("json");
    assert_eq!(through_cookie["path_and_query"], "/echo?x=1");
    assert_eq!(through_cookie["saw_cookie"], false);
    assert_eq!(through_cookie["saw_authorization"], false);

    let handoff = url::Url::parse(&takeover_url)
        .expect("parse takeover URL")
        .query_pairs()
        .find(|(k, _)| k == "handoff")
        .map(|(_, v)| v.into_owned())
        .expect("handoff query");
    let stripped_query = reqwest::Client::new()
        .get(format!("{base}/takeover/panel/echo?handoff={handoff}&x=2"))
        .send()
        .await
        .expect("query auth")
        .json::<serde_json::Value>()
        .await
        .expect("json");
    assert_eq!(stripped_query["path_and_query"], "/echo?x=2");
}

#[tokio::test]
async fn display_proxy_rejects_long_lived_token_query() {
    let upstream_port = spawn_fake_kasm().await;
    let base = spawn_display_router(Some("secret"), upstream_port).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/takeover/panel?token_secret=secret"))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn display_proxy_forwards_websocket_upgrades_behind_token() {
    let upstream_port = spawn_fake_kasm().await;
    let base = spawn_display_router(Some("secret"), upstream_port).await;
    let takeover_url = takeover_handoff_url(&base, "secret").await;
    let handoff = url::Url::parse(&takeover_url)
        .expect("parse takeover URL")
        .query_pairs()
        .find(|(k, _)| k == "handoff")
        .map(|(_, v)| v.into_owned())
        .expect("handoff query");
    let ws_url = base.replacen("http://", "ws://", 1).to_string()
        + &format!("/takeover/panel/ws?handoff={handoff}");

    let (mut socket, _resp) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("connect display ws");
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "hello".into(),
        ))
        .await
        .expect("send");
    let msg = socket.next().await.expect("next").expect("message");
    assert_eq!(msg.into_text().expect("text"), "echo:hello");
}

#[test]
fn capabilities_advertise_display_takeover_by_backend_family() {
    let tmp = tempfile::tempdir().expect("tmp");

    let mut chromium = BrowserHandle::synthetic(tmp.path().join("chromium"));
    chromium.family = "chromium".to_string();
    let chromium_state =
        test_state(None, HealthPublic::Off).with_default_browser(Arc::new(chromium));
    assert!(
        agent_first_http::host::listener::capabilities::build(&chromium_state)
            .takeover
            .backend_capable
    );

    let mut camoufox = BrowserHandle::synthetic(tmp.path().join("camoufox"));
    camoufox.family = "camoufox".to_string();
    let camoufox_state =
        test_state(None, HealthPublic::Off).with_default_browser(Arc::new(camoufox));
    assert!(
        agent_first_http::host::listener::capabilities::build(&camoufox_state)
            .takeover
            .backend_capable
    );

    let mut lightpanda = BrowserHandle::synthetic(tmp.path().join("lightpanda"));
    lightpanda.family = "lightpanda".to_string();
    let lightpanda_state =
        test_state(None, HealthPublic::Off).with_default_browser(Arc::new(lightpanda));
    assert!(
        !agent_first_http::host::listener::capabilities::build(&lightpanda_state)
            .takeover
            .backend_capable
    );
}

#[test]
fn capabilities_include_provider_neutral_display_fields() {
    let state = test_state(None, HealthPublic::Off).with_takeover_for_tests(5900);
    let caps = agent_first_http::host::listener::capabilities::build(&state);
    assert!(caps.takeover.supported);
    assert_eq!(caps.takeover.panel_url.as_deref(), Some("/takeover/panel"));
    assert_eq!(caps.takeover.provider.as_deref(), Some("kasmvnc"));
}

#[tokio::test]
async fn lightpanda_rejects_kasmvnc_takeover_before_launch() {
    let args = HostArgs {
        listen: "tcp:127.0.0.1:0".into(),
        profile: ProfileChoice::Ephemeral,
        display: DisplayMode::Headful,
        takeover: Takeover::On {
            provider: TakeoverProviderKind::KasmVnc,
        },
        display_quality: 100,
        browser: BrowserChoice::Lightpanda,
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
    let err = AppState::launch(&args).await.err().expect("expected error");
    assert_eq!(err.error_code, ErrorCode::BackendUnsupported);
}

#[tokio::test]
#[ignore]
async fn kasmvnc_process_launches_when_binary_available() {
    support::ensure_rustls_provider();
    let Some(bin) = support::env::discover_kasmvnc() else {
        println!("(skipping: no KasmVNC Xvnc binary; set AFHTTP_TEST_KASMVNC_BIN)");
        return;
    };
    std::env::set_var("AFHTTP_KASMVNC_BIN", bin);
    let handle = agent_first_http::host::takeover::launch_kasmvnc_provider()
        .await
        .expect("launch kasmvnc");
    assert!(handle.display.starts_with(':'));
    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{}/", handle.web_port))
        .send()
        .await
        .expect("kasm web request");
    assert!(resp.status().is_success());
}
