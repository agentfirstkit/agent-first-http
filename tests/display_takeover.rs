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
use agent_first_http::host::listener::{AppState, router_for_tests, test_state};
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
    agent_first_data::validate_protocol_event(&body, true).expect("strict AFDATA event");
    let url = body["result"]["takeover_url_secret"]
        .as_str()
        .expect("takeover_url_secret");
    assert!(url.contains("handoff_secret="), "{body}");
    assert!(
        body["result"]["takeover_url_ttl_s"]
            .as_u64()
            .unwrap_or_default()
            > 0
    );
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
    agent_first_data::validate_protocol_event(&body, true).expect("strict AFDATA event");
    assert_eq!(body["error"]["code"], "backend_unsupported");
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
    assert!(
        !location.contains("handoff_secret=") && !location.contains("handoff="),
        "handoff capability leaked: {location}"
    );
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
        .find(|(k, _)| k == "handoff_secret")
        .map(|(_, v)| v.into_owned())
        .expect("handoff query");
    let stripped_query = reqwest::Client::new()
        .get(format!(
            "{base}/takeover/panel/echo?handoff_secret={handoff}&x=2"
        ))
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
        .find(|(k, _)| k == "handoff_secret")
        .map(|(_, v)| v.into_owned())
        .expect("handoff query");
    let ws_url = base.replacen("http://", "ws://", 1).to_string()
        + &format!("/takeover/panel/ws?handoff_secret={handoff}");

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
    // SAFETY: this ignored hardware integration test is run in isolation, so
    // no other thread reads or mutates the process environment concurrently.
    unsafe { std::env::set_var("AFHTTP_KASMVNC_BIN", bin) };
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

/// The `--disable-gpu` regression, pinned where it actually reproduces.
///
/// The host used to pass `--disable-gpu` unconditionally, which switched the
/// GPU process off and took WebGL with it: a takeover browser reported
/// `webgl: false`, `webgl2: false` and zero extensions. No real browser looks
/// like that, and anti-bot scorers read it as automation — the takeover host
/// was advertising itself as a bot on the one path built to get past walls.
///
/// This has to run headful on a real X display. Under `--headless=new`,
/// Chromium serves WebGL whether or not `--disable-gpu` is passed, so the same
/// assertion in a headless test passes with the fix reverted and guards
/// nothing. Ignored and driven by `tests/test.sh takeover`, alongside the other
/// tests that need a live KasmVNC display.
#[tokio::test]
#[ignore]
async fn takeover_browsers_expose_webgl_on_a_real_display() {
    use agent_first_http::sdk::Client;
    use agent_first_http::sdk::fetch::{RenderMode, Wait};
    use agent_first_http::shared::artifacts::Artifact;

    support::ensure_rustls_provider();
    let Some(kasm) = support::env::discover_kasmvnc() else {
        panic!("KasmVNC is required for the takeover gate; set AFHTTP_TEST_KASMVNC_BIN");
    };
    // SAFETY: the takeover gate runs these ignored display tests with
    // --test-threads=1, so no other thread touches the environment here.
    unsafe { std::env::set_var("AFHTTP_KASMVNC_BIN", kasm) };
    let provider = agent_first_http::host::takeover::launch_kasmvnc_provider()
        .await
        .expect("launch kasmvnc");

    // Writes the verdict into the DOM: evaluate_after_wait runs JS but returns
    // nothing to the caller, so the page has to carry the answer out.
    const PROBE: &str = r#"(() => {
      const one = document.createElement('canvas').getContext('webgl');
      const two = document.createElement('canvas').getContext('webgl2');
      const out = document.createElement('div');
      out.id = 'afhttp-webgl-probe';
      out.textContent = JSON.stringify({
        webgl: !!one,
        webgl2: !!two,
        extensions: one ? one.getSupportedExtensions().length : 0,
      });
      document.body.appendChild(out);
    })()"#;

    let fixture = support::fixture_server::spawn().await;

    for (choice, label, bin) in [
        (
            BrowserChoice::Brave,
            "brave",
            support::env::discover_brave(),
        ),
        (
            BrowserChoice::Chrome,
            "chrome",
            support::env::discover_chrome(),
        ),
    ] {
        let bin = bin.unwrap_or_else(|| {
            panic!("{label} is a takeover backend and must be present in the test image")
        });
        let args = HostArgs {
            listen: "tcp:127.0.0.1:0".into(),
            profile: ProfileChoice::Ephemeral,
            display: DisplayMode::Headful,
            takeover: Takeover::On {
                provider: TakeoverProviderKind::KasmVnc,
            },
            display_quality: 100,
            browser: choice,
            browser_bin: Some(bin),
            token: None,
            takeover_enabled: true,
            health_enabled: true,
            health_public: HealthPublic::Off,
            engine_envs: vec![("DISPLAY".to_string(), provider.display.clone())],
            browser_args: Vec::new(),
            proxy: None,
            recent_requests_cap: 0,
        };
        let handle = agent_first_http::host::browser::launch(&args)
            .await
            .unwrap_or_else(|e| panic!("{label} headful launch: {e:?}"));

        let state = test_state(None, HealthPublic::Off).with_default_browser(Arc::new(handle));
        let app = router_for_tests(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let tmp = tempfile::tempdir().expect("tmpdir");
        let client = Client::connect(&format!("ws://{addr}")).expect("client");
        let result = client
            .fetch(format!("{}/identity.html", fixture.base_url()))
            .render(RenderMode::Always)
            .wait(Wait::Load)
            .timeout(Duration::from_secs(30))
            .evaluate_after_wait(PROBE)
            .want([Artifact::RenderedHtml])
            .out_dir(tmp.path().to_path_buf())
            .send()
            .await
            .unwrap_or_else(|e| panic!("{label} fetch: {e:?}"));

        let rendered = std::fs::read_to_string(
            result
                .rendered_html_file
                .as_ref()
                .expect("rendered_html_file"),
        )
        .expect("read rendered html");

        assert!(
            rendered.contains("\"webgl\":true") && rendered.contains("\"webgl2\":true"),
            "{label} lost WebGL on a real display — that is the --disable-gpu regression: {rendered}"
        );
        assert!(
            !rendered.contains("\"extensions\":0"),
            "{label} exposed a WebGL context with no extensions: {rendered}"
        );
    }
}
