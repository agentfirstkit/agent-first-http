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
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::TcpListener;

use support::takeover_host::{
    handoff_secret_of, mint_panel_url as takeover_handoff_url,
    spawn_fake_provider as spawn_fake_kasm, spawn_host as spawn_display_router,
    spawn_host_with_state as spawn_display_router_with_state,
};

#[cfg(unix)]
#[tokio::test]
async fn browser_exit_degrades_live_display_and_blocks_takeover() {
    support::ensure_rustls_provider();
    let Some(bin) = support::env::discover_browser() else {
        return;
    };
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
    let browser = Arc::new(
        agent_first_http::host::browser::launch(&args)
            .await
            .unwrap(),
    );
    let pid = browser.process_id.unwrap();
    let provider_port = spawn_fake_kasm().await;
    let state = test_state(Some("test-token"), HealthPublic::Off)
        .with_default_browser(browser)
        .with_takeover_for_tests(provider_port);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router_for_tests(state)).await;
    });
    let base = format!("http://{addr}");
    let client = agent_first_http::sdk::Client::connect(&base)
        .unwrap()
        .with_token("test-token");
    let health = client.health().await.unwrap();
    assert!(health.takeover.unwrap().ready);
    let session = client.takeover_ui_session().await.unwrap();
    assert!(
        tokio::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .await
            .unwrap()
            .success()
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    let health = loop {
        let health = client.health().await.unwrap();
        if !health.backend.as_ref().unwrap().connected
            && health.takeover.as_ref().unwrap().browser_running == Some(false)
        {
            break health;
        }
        assert!(std::time::Instant::now() < deadline, "browser did not exit");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(health.status, "degraded");
    let takeover = health.takeover.unwrap();
    assert!(takeover.configured);
    assert_eq!(takeover.browser_running, Some(false));
    assert!(!takeover.cdp_connected);
    assert!(takeover.display_connected, "VNC remains alive");
    assert!(!takeover.ready);
    assert!(takeover.reason.unwrap().contains("browser process exited"));
    assert_eq!(
        client
            .takeover_handoff(None, None)
            .await
            .unwrap_err()
            .error_code,
        ErrorCode::CdpUnavailable
    );
    let secret = handoff_secret_of(session.takeover_url_secret());
    let renewal = reqwest::Client::new()
        .put(format!("{base}/takeover/ui-session"))
        .header(reqwest::header::AUTHORIZATION, format!("Handoff {secret}"))
        .send()
        .await
        .unwrap();
    assert_eq!(renewal.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    for path in ["/takeover/handoff", "/takeover/ui-session"] {
        let response = reqwest::Client::new()
            .post(format!("{base}{path}"))
            .bearer_auth("test-token")
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    }
    let response = reqwest::Client::new()
        .get(format!("{base}/takeover/panel/"))
        .bearer_auth("test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let body = response.json::<serde_json::Value>().await.unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("browser process exited")
    );
    agent_first_data::validate_protocol_event(&body, true).unwrap();
}

#[tokio::test]
async fn panel_html_watches_authenticated_live_health() {
    let provider = spawn_fake_kasm().await;
    let base = spawn_display_router(Some("secret"), provider).await;
    let http = reqwest::Client::new();
    let response = http
        .get(format!("{base}/takeover/panel/index.html"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body = response.text().await.unwrap();
    assert!(body.contains("Display client"));
    assert!(body.contains("new URL('./afhttp-status', location.href)"));
    assert!(body.contains("Browser or display connection lost"));
    assert!(!body.contains("handoff_secret="));
    let response = http
        .get(format!("{base}/takeover/panel/afhttp-status"))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body = response.json::<serde_json::Value>().await.unwrap();
    agent_first_data::validate_protocol_event(&body, true).unwrap();
    assert_eq!(body["result"]["ready"], true);
    assert_eq!(
        http.get(format!("{base}/takeover/panel/afhttp-status"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn takeover_probes_cdp_when_public_health_endpoint_is_disabled() {
    support::ensure_rustls_provider();
    let provider = spawn_fake_kasm().await;
    let cdp = support::takeover_host::spawn_fake_browser().await;
    let mut state = test_state(Some("secret"), HealthPublic::Off)
        .with_default_browser(Arc::new(BrowserHandle::synthetic_at(
            cdp,
            std::path::PathBuf::new(),
        )))
        .with_takeover_for_tests(provider);
    state.health_enabled = false;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router_for_tests(state)).await;
    });
    let client = agent_first_http::sdk::Client::connect(&format!("http://{addr}"))
        .unwrap()
        .with_token("secret");
    assert!(client.health().await.is_err());
    assert!(client.takeover_handoff(None, None).await.is_ok());
    let session = client.takeover_ui_session().await.unwrap();
    assert!(session.revoke().await.unwrap());
}

/// A host with no display provider at all — the one shape the shared helper
/// deliberately cannot build, because every other test needs a provider.
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

    let landing = no_redirect
        .get(&takeover_url)
        .send()
        .await
        .expect("landing");
    assert_eq!(landing.status(), reqwest::StatusCode::OK);
    let cookie = landing
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("set-cookie")
        .split(';')
        .next()
        .expect("cookie pair")
        .to_string();
    assert!(cookie.starts_with("afhttp_handoff="));
    let bootstrap = landing.text().await.expect("landing body");
    // The landing page seeds noVNC's `resize` and quality settings and drops
    // the one-time handoff query, having just set the takeover cookie.
    assert!(
        bootstrap.contains("&resize=scale"),
        "missing resize setting: {bootstrap}"
    );
    assert!(
        bootstrap.contains("&max_video_resolution_x="),
        "missing quality params: {bootstrap}"
    );
    assert!(
        !bootstrap.contains("handoff_secret=") && !bootstrap.contains("handoff="),
        "handoff capability leaked: {bootstrap}"
    );
    // noVNC roots its WebSocket URL at the origin, so `path` must name wherever
    // this panel is publicly served from — and that is the one thing the
    // listener cannot know, because a proxy framing the panel elsewhere
    // forwards nothing that says so. It is derived in the browser instead, and
    // an absolute panel path baked in here is exactly the regression that made
    // the canvas unreachable through `afui session serve`.
    assert!(
        bootstrap.contains("location.pathname"),
        "the websocket path must be derived from where the browser is: {bootstrap}"
    );
    assert!(
        !bootstrap.contains("path=takeover/panel/websockify"),
        "the websocket path must not be hardcoded to this listener's own prefix: {bootstrap}"
    );

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

    let handoff = handoff_secret_of(&takeover_url);
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

/// A panel framed somewhere else is served, not corrected.
///
/// Once the browser has answered "here is where I am", the listener has no
/// opinion about it: a `path` naming a prefix that is not this listener's own
/// is passed to the display client unchanged, and the landing page is not
/// bootstrapped a second time. Bootstrapping again would loop, and rewriting
/// `path` would put the canvas back on a prefix only the direct delivery has.
#[tokio::test]
async fn a_panel_framed_under_a_foreign_prefix_keeps_the_prefix_the_browser_reported() {
    let upstream_port = spawn_fake_kasm().await;
    let base = spawn_display_router(Some("secret"), upstream_port).await;
    let handoff = handoff_secret_of(&takeover_handoff_url(&base, "secret").await);
    // What `afui session serve` produces: the browser is at `/s/<credential>/`,
    // so that is the prefix it reports, and the framing listener adds the
    // announced credential back on to every upstream request.
    let framed = "s%2F0123456789abcdef%2Fwebsockify";

    let landed = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client")
        .get(format!(
            "{base}/takeover/panel/?handoff_secret={handoff}&path={framed}&resize=scale"
        ))
        .send()
        .await
        .expect("send");
    assert_eq!(
        landed.status(),
        reqwest::StatusCode::OK,
        "a settled landing page must be served, not bootstrapped again"
    );
    let seen = landed.json::<serde_json::Value>().await.expect("json");
    assert_eq!(
        seen["path_and_query"],
        format!("/?path={framed}&resize=scale"),
        "the client's own prefix must reach the display unchanged"
    );
}

/// Revocation, which the handoff map this replaced had no way to express: the
/// only way to end a live panel used to be shutting the whole host down. A
/// profile switch tore the browser out from under a panel someone still had
/// open, and every credential minted for it kept authorizing.
#[tokio::test]
async fn revoking_takeover_credentials_ends_a_panel_that_is_still_open() {
    let upstream_port = spawn_fake_kasm().await;
    let (base, state) = spawn_display_router_with_state(Some("secret"), upstream_port).await;
    let takeover_url = takeover_handoff_url(&base, "secret").await;
    let secret = handoff_secret_of(&takeover_url);
    let panel = format!("{base}/takeover/panel/echo?handoff_secret={secret}");

    let live = reqwest::Client::new()
        .get(&panel)
        .send()
        .await
        .expect("before revoke");
    assert_eq!(live.status(), reqwest::StatusCode::OK);
    let cookie = live
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("set-cookie")
        .split(';')
        .next()
        .expect("cookie pair")
        .to_string();

    // What `ensure_profile` and host shutdown call.
    assert_eq!(state.revoke_takeover_handoffs(), 1);

    let by_query = reqwest::Client::new()
        .get(&panel)
        .send()
        .await
        .expect("after revoke");
    assert_eq!(by_query.status(), reqwest::StatusCode::UNAUTHORIZED);

    // The cookie the browser already holds is the same credential, so it dies
    // with it — otherwise the panel's own asset requests would outlive revoke.
    let by_cookie = reqwest::Client::new()
        .get(format!("{base}/takeover/panel/echo"))
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await
        .expect("after revoke, by cookie");
    assert_eq!(by_cookie.status(), reqwest::StatusCode::UNAUTHORIZED);

    // A credential minted after the switch works: revoke ends the live ones, it
    // does not disable the route.
    let reminted = takeover_handoff_url(&base, "secret").await;
    let again = reqwest::Client::new()
        .get(format!(
            "{base}/takeover/panel/echo?handoff_secret={}",
            handoff_secret_of(&reminted)
        ))
        .send()
        .await
        .expect("after remint");
    assert_eq!(again.status(), reqwest::StatusCode::OK);
}

/// A credential is only ever accepted by the host that minted it, and only in
/// its exact form. The map this replaced compared secrets with a hash lookup;
/// `UiAccessToken::authorize` compares them in constant time, and a wrong
/// secret of the right length must still be rejected.
#[tokio::test]
async fn a_near_miss_credential_is_rejected() {
    let upstream_port = spawn_fake_kasm().await;
    let base = spawn_display_router(Some("secret"), upstream_port).await;
    let secret = handoff_secret_of(&takeover_handoff_url(&base, "secret").await);

    // Flip the last character to something else in the same alphabet: same
    // length, same shape, one byte different.
    let mut near_miss = secret.clone();
    let last = near_miss.pop().expect("non-empty secret");
    near_miss.push(if last == '0' { '1' } else { '0' });
    assert_ne!(near_miss, secret);

    let resp = reqwest::Client::new()
        .get(format!(
            "{base}/takeover/panel/echo?handoff_secret={near_miss}"
        ))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
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
    let handoff = handoff_secret_of(&takeover_url);
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
        tab_idle_timeout_s: 0,
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
            tab_idle_timeout_s: 0,
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

/// The takeover download regression, pinned where it actually reproduces.
///
/// A download started on the takeover display used to stop on a native GTK
/// "wants to save" dialog aimed at `~/Downloads`. Nothing reached
/// `download_dir`, and the dialog blocked the display until a human clicked it.
///
/// The host did issue `Browser.setDownloadBehavior` at launch, which looked
/// like it covered this — but on a connection it closed immediately, and
/// Chromium scopes that override to the client that set it. Nothing survived to
/// the moment anything downloaded. The profile prefs written by
/// `preseed_chromium_profile` are what actually carry it now.
///
/// It must run headful on a real display: `--headless=new` never shows a save
/// dialog, so the same assertion passes headless with the fix reverted.
///
/// It must also NOT go through `Client::fetch`. That pipeline sets its own
/// `Browser.setDownloadBehavior` on its own live connection, so a fetch-driven
/// download succeeds even with this bug fully present. Driving a bare CDP
/// session — like the human whose click is not a CDP client at all — is the
/// only way this fails when regressed.
#[tokio::test]
#[ignore]
async fn takeover_download_lands_in_profile_without_a_save_dialog() {
    use agent_first_http::sdk::cdp::ws_client::Connection;

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

    let fixture = support::fixture_server::spawn().await;
    let bin = support::env::discover_brave()
        .expect("brave is a takeover backend and must be present in the test image");

    let args = HostArgs {
        listen: "tcp:127.0.0.1:0".into(),
        profile: ProfileChoice::Ephemeral,
        display: DisplayMode::Headful,
        takeover: Takeover::On {
            provider: TakeoverProviderKind::KasmVnc,
        },
        display_quality: 100,
        browser: BrowserChoice::Brave,
        browser_bin: Some(bin),
        token: None,
        takeover_enabled: true,
        health_enabled: true,
        health_public: HealthPublic::Off,
        engine_envs: vec![("DISPLAY".to_string(), provider.display.clone())],
        browser_args: Vec::new(),
        proxy: None,
        recent_requests_cap: 0,
        tab_idle_timeout_s: 0,
    };
    let handle = agent_first_http::host::browser::launch(&args)
        .await
        .unwrap_or_else(|e| panic!("brave headful launch: {e:?}"));
    let download_dir = handle.download_dir.clone();

    let conn = Connection::connect(&handle.ws_url, None)
        .await
        .expect("connect to browser");
    let targets = conn
        .send("Target.getTargets", &serde_json::json!({}), None)
        .await
        .expect("Target.getTargets");
    let target_id = targets["targetInfos"]
        .as_array()
        .and_then(|list| {
            list.iter()
                .find(|t| t["type"] == "page")
                .and_then(|t| t["targetId"].as_str())
        })
        .expect("a page target")
        .to_string();
    let attached = conn
        .send(
            "Target.attachToTarget",
            &serde_json::json!({"targetId": target_id, "flatten": true}),
            None,
        )
        .await
        .expect("Target.attachToTarget");
    let session_id = attached["sessionId"]
        .as_str()
        .expect("sessionId")
        .to_string();

    // Navigating to an attachment is what a human clicking a download link
    // does. The navigation itself aborts with ERR_ABORTED by design — the
    // download, not the page load, is the outcome under test.
    let _ = conn
        .send(
            "Page.navigate",
            &serde_json::json!({"url": format!("{}/download.bin", fixture.base_url())}),
            Some(&session_id),
        )
        .await;

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut landed = Vec::new();
    while std::time::Instant::now() < deadline {
        landed = std::fs::read_dir(&download_dir)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    // Chromium writes .crdownload while a transfer is in flight.
                    .filter(|n| !n.ends_with(".crdownload"))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !landed.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    assert!(
        !landed.is_empty(),
        "takeover download never reached {}: a save dialog is blocking the display, \
         which is the regression this pins",
        download_dir.display()
    );
}
