//! Integration tests for `--render always` browser fetch end-to-end.
//! Requires a chromium binary; skipped otherwise.

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

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use agent_first_http::host::bootstrap::{
    BrowserChoice, DisplayMode, HealthPublic, HostArgs, ProfileChoice, Takeover,
};
use agent_first_http::host::{browser, listener::router_for_tests, listener::test_state};
use agent_first_http::sdk::fetch::{RenderMode, Wait};
use agent_first_http::sdk::Client;
use agent_first_http::shared::artifacts::Artifact;
use agent_first_http::shared::ids::TabId;
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

async fn spawn_host_with_browser() -> Option<(String, tempfile::TempDir, OwnedSemaphorePermit)> {
    let permit = browser_test_permit().await;
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
        ops_enabled: true,
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
    let tmp = tempfile::tempdir().expect("tmpdir");
    Some((format!("ws://{addr}"), tmp, permit))
}

async fn browser_test_permit() -> OwnedSemaphorePermit {
    static SEMAPHORE: OnceLock<Arc<Semaphore>> = OnceLock::new();
    SEMAPHORE
        .get_or_init(|| Arc::new(Semaphore::new(1)))
        .clone()
        .acquire_owned()
        .await
        .expect("browser test semaphore")
}

#[tokio::test]
async fn render_always_returns_rendered_html_and_artifacts() {
    let Some((endpoint, tmp, _browser_guard)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let url = format!("{}/js.html", fixture.base_url());

    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch(url.clone())
        .render(RenderMode::Always)
        .wait(Wait::Ms(150)) // give the setTimeout 50ms a chance
        .timeout(Duration::from_secs(15))
        .want([
            Artifact::Body,
            Artifact::RenderedHtml,
            Artifact::Text,
            Artifact::Screenshot,
            Artifact::Console,
            Artifact::Network,
            Artifact::Observation,
        ])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    assert_eq!(
        result.trace.render_decision,
        agent_first_http::sdk::fetch::result::RenderDecision::Browser
    );
    assert!(result.tab_id.is_some());
    assert_eq!(result.status, 200);

    // All seven artifacts produced.
    let rendered = result
        .rendered_html_file
        .as_ref()
        .unwrap_or_else(|| panic!("rendered_html_file; warnings={:?}", result.warnings));
    let html = std::fs::read_to_string(rendered).expect("read rendered");
    assert!(
        html.contains("ready"),
        "rendered HTML missing 'ready': {html}"
    );

    let text = result
        .text_file
        .as_ref()
        .unwrap_or_else(|| panic!("text_file; warnings={:?}", result.warnings));
    let text_str = std::fs::read_to_string(text).expect("read text");
    assert!(text_str.contains("ready"), "innerText: {text_str}");

    let png = result.screenshot_file.as_ref().expect("screenshot_file");
    let png_bytes = std::fs::read(png).expect("read png");
    assert!(
        png_bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]),
        "PNG magic missing"
    );

    let obs = result.observation_file.as_ref().expect("observation_file");
    let obs_str = std::fs::read_to_string(obs).expect("read obs");
    // Mechanical-only invariant from observation.rs::DISALLOWED_LABELS.
    for forbidden in ["login", "captcha", "paywall", "important", "best"] {
        assert!(
            !obs_str.contains(&format!("\"{forbidden}\"")),
            "observation contained forbidden label: {forbidden}"
        );
    }

    let net = result.network_file.as_ref().expect("network_file");
    let net_str = std::fs::read_to_string(net).expect("read net");
    assert!(net_str.contains("\"schema_version\""));

    let console = result.console_file.as_ref().expect("console_file");
    let console_str = std::fs::read_to_string(console).expect("read console");
    assert!(console_str.contains("\"events\""));

    assert!(
        result.trace.navigation_duration_ms.is_some(),
        "navigation_duration_ms should be set"
    );
}

#[tokio::test]
async fn render_always_applies_request_overrides_and_evaluate_hook() {
    let Some((endpoint, tmp, _browser_guard)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let url = format!("{}/identity.html", fixture.base_url());

    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch(url.clone())
        .render(RenderMode::Always)
        .wait(Wait::Load)
        .timeout(Duration::from_secs(15))
        .want([Artifact::RenderedHtml, Artifact::Network])
        .header("X-Afhttp-Test", "browser-present")
        .user_agent("afhttp-browser-agent/1")
        .cookie("sid", "abc")
        .cookie_full(
            cookie::Cookie::build(("http_only", "1"))
                .path("/")
                .http_only(true)
                .same_site(cookie::SameSite::Lax)
                .build(),
        )
        .evaluate_after_wait("document.body.setAttribute('data-after-wait', 'ok')")
        .network_redact(false)
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    let rendered = result
        .rendered_html_file
        .as_ref()
        .unwrap_or_else(|| panic!("rendered_html_file; warnings={:?}", result.warnings));
    let html = std::fs::read_to_string(rendered).expect("read rendered");
    assert!(
        html.contains("afhttp-browser-agent/1"),
        "navigator.userAgent was not overridden: {html}"
    );
    assert!(
        html.contains("sid=abc"),
        "document.cookie did not include injected cookie: {html}"
    );
    assert!(
        html.contains("http_only=1"),
        "server did not receive HttpOnly cookie: {html}"
    );
    assert!(
        html.contains("data-after-wait=\"ok\""),
        "evaluate_after_wait did not run before capture: {html}"
    );

    let network = result.network_file.as_ref().expect("network_file");
    let log: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(network).expect("read network"))
            .expect("network json");
    let doc = log["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| {
            entry["resource_type"] == "Document"
                && entry["url"]
                    .as_str()
                    .is_some_and(|u| u.ends_with("/identity.html"))
        })
        .expect("document entry");
    let headers = doc["request_headers"].as_object().expect("request_headers");
    let custom = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("x-afhttp-test"))
        .map(|(_, value)| value.as_str().unwrap_or_default());
    assert_eq!(custom, Some("browser-present"));
}

#[tokio::test]
async fn render_always_reports_real_http_status() {
    let Some((endpoint, tmp, _browser_guard)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch(format!("{}/404", fixture.base_url()))
        .render(RenderMode::Always)
        .wait(Wait::Load)
        .timeout(Duration::from_secs(10))
        .want([Artifact::Network])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    assert_eq!(
        result.status, 404,
        "browser status must be the main response"
    );
    assert!(result.trace.main_request_observed);
}

#[tokio::test]
async fn browser_trace_reports_when_main_request_is_not_observed() {
    let Some((endpoint, tmp, _browser_guard)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch("about:blank")
        .render(RenderMode::Always)
        .wait(Wait::Load)
        .timeout(Duration::from_secs(10))
        .want([Artifact::Network])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    assert_eq!(result.status, 0);
    assert!(!result.trace.main_request_observed);
    assert!(result.warnings.iter().any(|w| w
        .detail
        .contains("main document network request was not observed")));
}

#[tokio::test]
async fn wait_selector_visible_waits_for_layout_flip() {
    let Some((endpoint, tmp, _browser_guard)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let client = Client::connect(&endpoint).expect("client");

    // The fixture inserts `.target` immediately with `display:none`, then
    // flips it to visible after 150ms. Wait::SelectorVisible must NOT
    // resolve until after the flip.
    let start = std::time::Instant::now();
    let result = client
        .fetch(format!("{}/hidden-then-visible.html", fixture.base_url()))
        .render(RenderMode::Always)
        .wait(Wait::SelectorVisible(".target".into()))
        .timeout(Duration::from_secs(5))
        .want([Artifact::RenderedHtml])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(100),
        "Wait::SelectorVisible should have waited for the 150ms layout flip, took {:?}",
        elapsed,
    );
    let rendered_path = result
        .rendered_html_file
        .as_ref()
        .expect("rendered_html_file");
    let html = std::fs::read_to_string(rendered_path).expect("read rendered");
    assert!(
        html.contains("visible"),
        "rendered HTML should contain the post-flip text: {html}",
    );
}

#[tokio::test]
async fn wait_selector_visible_times_out_when_node_stays_hidden() {
    let Some((endpoint, tmp, _browser_guard)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let client = Client::connect(&endpoint).expect("client");

    // /plain.html has no `.target`, so SelectorVisible will time out
    // with wait_selector_unmatched — the same error code as the
    // existence-only variant, kept distinct from navigation_timeout.
    let err = client
        .fetch(format!("{}/plain.html", fixture.base_url()))
        .render(RenderMode::Always)
        .wait(Wait::SelectorVisible(".never-exists".into()))
        .timeout(Duration::from_secs(2))
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .err()
        .expect("expected error");
    assert_eq!(
        err.error_code,
        agent_first_http::shared::error::ErrorCode::WaitSelectorUnmatched,
    );
    assert!(!err.retryable, "selector mismatch is not retryable");
}

#[tokio::test]
async fn observe_main_wait_ms_is_honored() {
    // Lower bound: about:blank never produces a main-document network
    // event. With observe_main_wait_ms=50 the wait must return quickly
    // (single-digit-second total fetch); the default 500ms also returns
    // quickly but the test asserts the knob is wired by raising it and
    // confirming the wait does not impose its full timeout when the
    // event arrives early. We pick about:blank so the wait is the only
    // variable that could expand the fetch.
    let Some((endpoint, tmp, _browser_guard)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let client = Client::connect(&endpoint).expect("client");
    let start = std::time::Instant::now();
    let result = client
        .fetch("about:blank")
        .render(RenderMode::Always)
        .wait(Wait::Load)
        .timeout(Duration::from_secs(10))
        .observe_main_wait_ms(50)
        .want([Artifact::Network])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");
    let elapsed = start.elapsed();
    assert!(!result.trace.main_request_observed);
    assert!(
        elapsed < Duration::from_secs(5),
        "fetch with observe_main_wait_ms=50 took {:?}; the knob is not plumbed",
        elapsed,
    );
}

#[tokio::test]
async fn browser_body_file_is_raw_main_response_not_rendered_dom() {
    let Some((endpoint, tmp, _browser_guard)) = spawn_host_with_browser().await else {
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
        .want([Artifact::Body, Artifact::RenderedHtml])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    let body_file = result.body_file.as_ref().expect("body_file");
    let body = std::fs::read_to_string(body_file).expect("read body");
    assert!(body.contains("fetch('/data.json')"), "raw body = {body}");
    assert!(
        body.contains("pending"),
        "raw body should predate JS mutation: {body}"
    );

    let rendered_file = result
        .rendered_html_file
        .as_ref()
        .expect("rendered_html_file");
    let rendered = std::fs::read_to_string(rendered_file).expect("read rendered");
    assert!(
        rendered.contains("hello") || rendered.contains("world"),
        "rendered DOM should reflect XHR result: {rendered}"
    );
}

#[tokio::test]
async fn fetch_tab_reuses_existing_target_and_does_not_close_it() {
    let Some((endpoint, tmp, _browser_guard)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let client = Client::connect(&endpoint).expect("client");
    let target = client
        .cdp("Target.createTarget")
        .params(serde_json::json!({"url": "about:blank"}))
        .send()
        .await
        .expect("create target");
    let target_id = target["targetId"].as_str().expect("target id").to_string();

    let result = client
        .fetch(format!("{}/plain.html", fixture.base_url()))
        .render(RenderMode::Always)
        .tab(TabId::new(target_id.clone()))
        .wait(Wait::Load)
        .timeout(Duration::from_secs(10))
        .want([Artifact::Text])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");
    assert_eq!(
        result.tab_id.as_ref().map(TabId::as_str),
        Some(target_id.as_str())
    );

    let targets = client
        .cdp("Target.getTargets")
        .send()
        .await
        .expect("get targets");
    let still_open = targets["targetInfos"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .any(|t| t["targetId"].as_str() == Some(target_id.as_str()));
    assert!(still_open, "fetch --tab must not close reused target");
}

#[tokio::test]
async fn observation_contains_agent_action_fields() {
    let Some((endpoint, tmp, _browser_guard)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch(format!("{}/interactive.html", fixture.base_url()))
        .render(RenderMode::Always)
        .wait(Wait::Load)
        .timeout(Duration::from_secs(10))
        .want([Artifact::Observation])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    let obs_file = result.observation_file.as_ref().expect("observation_file");
    let obs: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(obs_file).expect("read obs"))
            .expect("observation json");
    let nodes = obs["nodes"].as_array().expect("nodes");
    let button = nodes
        .iter()
        .find(|n| n["role"].as_str() == Some("button") && n["name"].as_str() == Some("Go"))
        .expect("button node");
    assert_eq!(button["visible"], true);
    assert!(button["bbox"]["width"].as_f64().unwrap_or(0.0) > 0.0);
    assert!(button["actions"]
        .as_array()
        .expect("actions")
        .iter()
        .any(|a| a.as_str() == Some("click")));
    assert_eq!(button["selector_hint"].as_str(), Some("#go"));
    assert_eq!(button["selector_hint_unique"], true);
    let input = nodes
        .iter()
        .find(|n| n["input_type"].as_str() == Some("text"))
        .expect("text input");
    assert_eq!(input["value_redacted"], true);
    assert_eq!(input["selector_hint_unique"], true);

    let iframe = nodes
        .iter()
        .find(|n| n["role"].as_str() == Some("iframe"))
        .expect("iframe node");
    let frame_ref = iframe["frame_ref"].as_str().expect("frame_ref");
    assert!(obs["frames"]
        .as_array()
        .expect("frames")
        .iter()
        .any(|frame| frame["frame_id"].as_str() == Some(frame_ref)));

    let role_for_selector = |selector: &str| {
        nodes
            .iter()
            .find(|n| n["selector_hint"].as_str() == Some(selector))
            .and_then(|n| n["role"].as_str())
            .expect("select node role")
    };
    assert_eq!(role_for_selector("#single"), "combobox");
    assert_eq!(role_for_selector("#menu"), "combobox");
    assert_eq!(role_for_selector("#multi"), "listbox");

    let cursor_div = nodes
        .iter()
        .find(|n| n["selector_hint"].as_str() == Some("#click-div"))
        .expect("cursor pointer div");
    assert_eq!(cursor_div["role"].as_str(), Some("div"));
    assert!(cursor_div["actions"]
        .as_array()
        .expect("actions")
        .iter()
        .any(|a| a.as_str() == Some("click")));
}

#[tokio::test]
async fn observation_pierces_shadow_and_same_origin_iframes_only() {
    let Some((endpoint, tmp, _browser_guard)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let cross_fixture = support::fixture_server::spawn().await;
    let cross_url = format!("{}/plain.html", cross_fixture.base_url());
    let url = format!(
        "{}/observation-contexts.html?cross={}",
        fixture.base_url(),
        cross_url
    );
    let client = Client::connect(&endpoint).expect("client");
    let target = client
        .cdp("Target.createTarget")
        .params(serde_json::json!({"url": "about:blank"}))
        .send()
        .await
        .expect("create target");
    let target_id = target["targetId"].as_str().expect("target id").to_string();
    let result = client
        .fetch(url)
        .render(RenderMode::Always)
        .tab(TabId::new(target_id.clone()))
        .wait(Wait::Load)
        .timeout(Duration::from_secs(10))
        .want([Artifact::Observation])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    let obs_file = result.observation_file.as_ref().expect("observation_file");
    let obs: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(obs_file).expect("read obs"))
            .expect("observation json");
    assert!(obs.get("truncated").is_none() || obs["truncated"].is_null());
    let nodes = obs["nodes"].as_array().expect("nodes");
    let frames = obs["frames"].as_array().expect("frames");

    let shadow_button = nodes
        .iter()
        .find(|n| {
            n["frame_id"].as_str() == Some("main")
                && n["role"].as_str() == Some("button")
                && n["name"].as_str() == Some("Shadow Action")
        })
        .expect("shadow button");
    let shadow_hint = shadow_button["selector_hint"]
        .as_str()
        .expect("shadow selector hint");
    assert!(
        shadow_hint.contains(" >> shadow >> "),
        "shadow hint should include host chain: {shadow_hint}"
    );
    assert_eq!(shadow_button["selector_hint_unique"], true);

    let same_iframe = nodes
        .iter()
        .find(|n| n["selector_hint"].as_str() == Some("#same-frame"))
        .expect("same-origin iframe node");
    let same_frame_ref = same_iframe["frame_ref"].as_str().expect("same frame ref");
    assert!(frames.iter().any(|frame| {
        frame["frame_id"].as_str() == Some(same_frame_ref)
            && frame["url"]
                .as_str()
                .is_some_and(|url| url.ends_with("/frame-inner.html"))
    }));
    let frame_button = nodes
        .iter()
        .find(|n| {
            n["frame_id"].as_str() == Some(same_frame_ref)
                && n["role"].as_str() == Some("button")
                && n["name"].as_str() == Some("Frame Action")
        })
        .expect("same-origin frame button");
    let frame_hint = frame_button["selector_hint"]
        .as_str()
        .expect("frame selector hint");
    assert_eq!(frame_hint, "#frame-action");
    assert_eq!(frame_button["selector_hint_unique"], true);

    let cross_iframe = nodes
        .iter()
        .find(|n| n["selector_hint"].as_str() == Some("#cross-frame"))
        .expect("cross-origin iframe node");
    let cross_frame_ref = cross_iframe["frame_ref"].as_str().expect("cross frame ref");
    assert!(frames.iter().any(|frame| {
        frame["frame_id"].as_str() == Some(cross_frame_ref)
            && frame["url"]
                .as_str()
                .is_some_and(|url| url == cross_url.as_str())
    }));
    assert!(
        !nodes
            .iter()
            .any(|node| node["frame_id"].as_str() == Some(cross_frame_ref)),
        "cross-origin frame content must not be traversed"
    );

    let expression = format!(
        r#"(() => {{
          const shadowHint = {shadow_hint};
          const frameHint = {frame_hint};
          function resolve(root, hint) {{
            const parts = hint.split(' >> shadow >> ');
            let scope = root;
            for (let i = 0; i < parts.length - 1; i++) {{
              const host = scope.querySelector(parts[i]);
              if (!host || !host.shadowRoot) return false;
              scope = host.shadowRoot;
            }}
            return !!scope.querySelector(parts[parts.length - 1]);
          }}
          const sameFrame = document.querySelector('#same-frame');
          return JSON.stringify({{
            shadow: resolve(document, shadowHint),
            frame: !!(sameFrame && sameFrame.contentDocument && resolve(sameFrame.contentDocument, frameHint))
          }});
        }})()"#,
        shadow_hint = serde_json::to_string(shadow_hint).expect("shadow hint json"),
        frame_hint = serde_json::to_string(frame_hint).expect("frame hint json")
    );
    let resolved = client
        .cdp("Runtime.evaluate")
        .tab(TabId::new(target_id.clone()))
        .params(serde_json::json!({
            "expression": expression,
            "returnByValue": true,
        }))
        .send()
        .await
        .expect("resolve hints");
    let resolved: serde_json::Value = serde_json::from_str(
        resolved["result"]["value"]
            .as_str()
            .expect("resolver json string"),
    )
    .expect("resolver json");
    assert_eq!(resolved["shadow"], true);
    assert_eq!(resolved["frame"], true);

    let truncated_result = client
        .fetch(format!("{}/observation-truncated.html", fixture.base_url()))
        .render(RenderMode::Always)
        .tab(TabId::new(target_id))
        .wait(Wait::Load)
        .timeout(Duration::from_secs(10))
        .want([Artifact::Observation])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");
    let truncated_file = truncated_result
        .observation_file
        .as_ref()
        .expect("observation_file");
    let truncated: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(truncated_file).expect("read obs"))
            .expect("observation json");
    assert_eq!(truncated["nodes"].as_array().expect("nodes").len(), 100);
    assert_eq!(
        truncated["truncated"]["reason"].as_str(),
        Some("node_limit_exceeded")
    );
    assert_eq!(truncated["truncated"]["node_limit"].as_u64(), Some(100));
}

#[tokio::test]
async fn render_always_selector_wait_resolves() {
    let Some((endpoint, tmp, _browser_guard)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let url = format!("{}/js.html", fixture.base_url());
    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch(url)
        .render(RenderMode::Always)
        .wait(Wait::Selector("#root".into()))
        .timeout(Duration::from_secs(10))
        .want([Artifact::RenderedHtml])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");
    assert!(result.rendered_html_file.is_some());
}

#[tokio::test]
async fn render_always_with_wait_idle_completes_against_js_fixture() {
    let Some((endpoint, tmp, _browser_guard)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let url = format!("{}/js.html", fixture.base_url());
    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch(url)
        .render(RenderMode::Always)
        .wait(Wait::Idle)
        .timeout(Duration::from_secs(5))
        .want([Artifact::RenderedHtml])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch with Wait::Idle should complete");

    assert_eq!(
        result.trace.render_decision,
        agent_first_http::sdk::fetch::result::RenderDecision::Browser
    );
    assert!(
        result.trace.navigation_duration_ms.is_some(),
        "Wait::Idle should still populate navigation_duration_ms",
    );
    let rendered = result
        .rendered_html_file
        .as_ref()
        .expect("rendered_html_file");
    let html = std::fs::read_to_string(rendered).expect("read rendered");
    // Wait::Idle should fire after the setTimeout(50ms) injection completes
    // and the network goes quiescent.
    assert!(
        html.contains("ready"),
        "Wait::Idle should fire after JS finishes; rendered = {html}"
    );
}

#[tokio::test]
async fn wait_auto_captures_delayed_xhr_text_and_trace_signals() {
    let Some((endpoint, tmp, _browser_guard)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch(format!("{}/delayed-xhr.html", fixture.base_url()))
        .render(RenderMode::Always)
        .wait(Wait::Auto)
        .timeout(Duration::from_secs(6))
        .want([Artifact::Text, Artifact::Network])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    let text = std::fs::read_to_string(
        result
            .text_file
            .as_ref()
            .unwrap_or_else(|| panic!("text_file; warnings={:?}", result.warnings)),
    )
    .expect("read text");
    assert!(
        text.contains("delayed ready"),
        "--wait auto should wait for delayed XHR text; text={text}"
    );
    assert_eq!(result.trace.wait_mode.as_deref(), Some("auto"));
    assert_eq!(
        result.trace.wait_satisfied_by.as_deref(),
        Some("network_quiet_dom_text_stable")
    );
    assert_eq!(result.trace.network_quiet, Some(true));
    assert_eq!(result.trace.dom_stable, Some(true));
    assert_eq!(result.trace.text_stable, Some(true));

    let network: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(result.network_file.as_ref().expect("network_file"))
            .expect("read network"),
    )
    .expect("network json");
    let entries = network["entries"].as_array().expect("entries");
    let delayed = entries
        .iter()
        .find(|entry| {
            entry["url"]
                .as_str()
                .is_some_and(|url| url.ends_with("/delayed-data.json"))
        })
        .expect("delayed xhr entry");
    assert_eq!(delayed["state"].as_str(), Some("finished"));
    assert!(
        delayed["body_file"].as_str().is_some(),
        "--wait auto should default to xhr network body capture"
    );
}

#[tokio::test]
async fn wait_load_can_capture_before_delayed_xhr_settles() {
    let Some((endpoint, tmp, _browser_guard)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch(format!("{}/delayed-xhr.html", fixture.base_url()))
        .render(RenderMode::Always)
        .wait(Wait::Load)
        .timeout(Duration::from_secs(6))
        .want([Artifact::Text])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");

    let text =
        std::fs::read_to_string(result.text_file.as_ref().expect("text_file")).expect("read text");
    assert!(
        text.contains("loading"),
        "explicit --wait load should preserve old early-capture semantics; text={text}"
    );
    assert_eq!(result.trace.wait_mode.as_deref(), Some("load"));
}

#[tokio::test]
async fn wait_auto_reports_pending_xhr_without_hanging() {
    let Some((endpoint, tmp, _browser_guard)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let client = Client::connect(&endpoint).expect("client");
    let start = std::time::Instant::now();
    let result = client
        .fetch(format!("{}/never-xhr.html", fixture.base_url()))
        .render(RenderMode::Always)
        .wait(Wait::Auto)
        .timeout(Duration::from_secs(3))
        .want([Artifact::Text, Artifact::Network])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch should return a partial structured result");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "--wait auto should not hang on never-ending XHR"
    );
    assert_eq!(result.trace.wait_mode.as_deref(), Some("auto"));
    assert_eq!(result.trace.network_quiet, Some(false));
    assert!(result
        .warnings
        .iter()
        .any(|w| { w.code == agent_first_http::shared::error::ErrorCode::ReadinessTimeout }));
    assert!(result
        .warnings
        .iter()
        .any(|w| { w.code == agent_first_http::shared::error::ErrorCode::NetworkNotIdle }));
    assert!(result
        .warnings
        .iter()
        .any(|w| { w.code == agent_first_http::shared::error::ErrorCode::PendingXhrAtCapture }));

    let network: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(result.network_file.as_ref().expect("network_file"))
            .expect("read network"),
    )
    .expect("network json");
    assert!(
        network["summary"]["inflight_total_at_capture"]
            .as_u64()
            .unwrap_or(0)
            > 0,
        "network summary should expose in-flight requests: {network}"
    );
    let pending = network["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| {
            entry["url"]
                .as_str()
                .is_some_and(|url| url.ends_with("/never.json"))
        })
        .expect("pending xhr entry");
    assert!(matches!(
        pending["state"].as_str(),
        Some("pending" | "responded")
    ));
}

#[tokio::test]
async fn empty_artifacts_emit_quality_warnings() {
    let Some((endpoint, tmp, _browser_guard)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch(format!("{}/empty.html", fixture.base_url()))
        .render(RenderMode::Always)
        .wait(Wait::Auto)
        .timeout(Duration::from_secs(5))
        .want([Artifact::Text, Artifact::Observation])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch");
    assert!(result.warnings.iter().any(|w| {
        w.artifact == Artifact::Text
            && w.code == agent_first_http::shared::error::ErrorCode::ArtifactEmpty
    }));
    assert!(result.warnings.iter().any(|w| {
        w.artifact == Artifact::Observation
            && w.code == agent_first_http::shared::error::ErrorCode::ObservationEmpty
    }));
}

#[tokio::test]
async fn render_always_unmatched_selector_returns_wait_selector_unmatched() {
    let Some((endpoint, tmp, _browser_guard)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    // The page loads fine but the CSS selector never matches; agents need
    // this distinct from `navigation_timeout` so they fix the selector
    // instead of blind-retrying the whole fetch.
    let fixture = support::fixture_server::spawn().await;
    let url = format!("{}/plain.html", fixture.base_url());
    let client = Client::connect(&endpoint).expect("client");
    let err = client
        .fetch(url)
        .render(RenderMode::Always)
        .wait(Wait::Selector("#this-never-exists".into()))
        .timeout(Duration::from_secs(2))
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .err()
        .expect("expected error");
    assert_eq!(
        err.error_code,
        agent_first_http::shared::error::ErrorCode::WaitSelectorUnmatched,
        "got {err:?}"
    );
    // wait_selector_unmatched defaults to non-retryable: a selector typo
    // won't be fixed by trying again.
    assert!(!err.retryable);
}

#[tokio::test]
async fn render_auto_escalates_empty_html_shell_to_browser() {
    let Some((endpoint, tmp, _browser_guard)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    // /spa-shell.html returns 200 + HTML that is structurally empty in the
    // HTTP body but populates <h1>hydrated</h1> after JS runs. Auto mode
    // must escalate to the browser path and surface escalation_reason.
    let fixture = support::fixture_server::spawn().await;
    let url = format!("{}/spa-shell.html", fixture.base_url());
    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch(url)
        .render(RenderMode::Auto)
        .wait(Wait::Load)
        .timeout(Duration::from_secs(10))
        .want([Artifact::RenderedHtml])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("fetch should succeed via browser escalation");

    assert_eq!(
        result.trace.render_decision,
        agent_first_http::sdk::fetch::result::RenderDecision::Browser,
        "auto mode should have escalated to browser",
    );
    assert_eq!(
        result.trace.escalation_reason.as_deref(),
        Some("empty_html_shell"),
        "escalation_reason should expose the stable token",
    );
    // The Auto-mode escalation case: render_mode reflects what the agent
    // ASKED for, render_used reflects whether the browser actually ran.
    assert_eq!(
        result.trace.render_mode,
        agent_first_http::sdk::fetch::result::TraceRenderMode::Auto,
        "render_mode should be auto even after escalation",
    );
    assert!(
        result.trace.render_used,
        "render_used must be true once the browser path ran",
    );
    // After browser hydration the rendered HTML must contain real content.
    let rendered = result
        .rendered_html_file
        .as_ref()
        .expect("rendered_html_file");
    let html = std::fs::read_to_string(rendered).expect("read rendered");
    assert!(
        html.contains("hydrated"),
        "rendered HTML should contain post-hydration text, got: {html}",
    );
}

#[tokio::test]
async fn browser_navigation_download_is_captured_in_profile() {
    let Some((endpoint, tmp, _browser_guard)) = spawn_host_with_browser().await else {
        println!("(skipping: no chromium)");
        return;
    };
    let fixture = support::fixture_server::spawn().await;
    let url = format!("{}/download.bin", fixture.base_url());
    let client = Client::connect(&endpoint).expect("client");
    let result = client
        .fetch(url)
        .render(RenderMode::Always)
        .wait(Wait::Load)
        .timeout(Duration::from_secs(10))
        .want([Artifact::Body, Artifact::Observation, Artifact::Network])
        .out_dir(tmp.path().to_path_buf())
        .send()
        .await
        .expect("download fetch");

    let download = result.download_file.as_ref().expect("download_file");
    assert!(
        download.exists(),
        "download should exist: {}",
        download.display()
    );
    assert!(
        download.ends_with("fixture-download.bin"),
        "suggested filename should be preserved: {}",
        download.display()
    );
    assert_eq!(result.download_state.as_deref(), Some("completed"));
    assert_eq!(
        result.download_bytes,
        Some("downloaded from browser".len() as u64)
    );
    assert!(
        result.body_file.is_none(),
        "downloads are not body artifacts"
    );
    assert!(
        result.observation_file.is_none(),
        "downloads are not page observations"
    );
}
