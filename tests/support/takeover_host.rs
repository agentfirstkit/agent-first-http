//! A takeover-capable afhttp listener in-process, with a stand-in for the
//! display provider behind it.
//!
//! Real KasmVNC needs an X display and is exercised by the ignored tests
//! `tests/test.sh takeover` runs. Everything about the *handoff* — credential
//! minting, constant-time authorization, the cookie, the redirect that strips
//! the credential out of the panel URL — is provider-independent, so these
//! tests point the proxy at a tiny axum upstream and stay deterministic.

use std::time::Duration;

use agent_first_http::host::bootstrap::HealthPublic;
use agent_first_http::host::listener::{AppState, router_for_tests, test_state};
use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::http::{HeaderMap, Uri};
use axum::response::IntoResponse;
use axum::routing::get;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::TcpListener;

/// Stand-in for the display provider's web client. `/echo` reports the request
/// exactly as the proxy forwarded it, which is how the tests see what was
/// rewritten and what was stripped.
pub async fn spawn_fake_provider() -> u16 {
    let app = axum::Router::new()
        // The landing page the display client actually serves, reported the
        // same way as `/echo`: what reaches it decides whether the panel's own
        // settings survived the trip.
        .route("/", get(echo_request))
        .route("/echo", get(echo_request))
        .route(
            "/index.html",
            get(|| async {
                axum::response::Html("<!doctype html><html><body>Display client</body></html>")
            }),
        )
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

/// A takeover-enabled host serving `/takeover/*` against `upstream_port`.
pub async fn spawn_host(token: Option<&str>, upstream_port: u16) -> String {
    spawn_host_with_state(token, upstream_port).await.0
}

/// Same host, plus the state handle — revocation is driven from the state the
/// way a profile switch and a host shutdown drive it, not over HTTP.
pub async fn spawn_host_with_state(token: Option<&str>, upstream_port: u16) -> (String, AppState) {
    super::ensure_rustls_provider();
    let browser = spawn_fake_browser().await;
    let state = test_state(token, HealthPublic::Off)
        .with_takeover_for_tests(upstream_port)
        .with_default_browser(std::sync::Arc::new(
            agent_first_http::host::browser::BrowserHandle::synthetic_at(
                browser,
                std::path::PathBuf::new(),
            ),
        ));
    let app = router_for_tests(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind host");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (format!("http://{addr}"), state)
}

/// Mint one panel URL over the wire, the way `panel` and `fetch --takeover` do.
pub async fn mint_panel_url(base: &str, token: &str) -> String {
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

pub fn handoff_secret_of(takeover_url: &str) -> String {
    url::Url::parse(takeover_url)
        .expect("parse takeover URL")
        .query_pairs()
        .find(|(k, _)| k == "handoff_secret")
        .map(|(_, v)| v.into_owned())
        .expect("handoff query")
}

/// CDP peer for handoff tests: configuration support is separate from whether
/// the backend actually answers. Tests can swap the profile to an exited peer.
pub async fn spawn_fake_browser() -> String {
    async fn cdp(ws: WebSocketUpgrade) -> impl IntoResponse {
        ws.on_upgrade(|socket| async move {
            let (mut tx, mut rx) = socket.split();
            while let Some(Ok(Message::Text(text))) = rx.next().await {
                let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                let response = json!({"id":value["id"], "result":{"targetInfos":[{"type":"page","targetId":"page-1"}]}});
                if tx.send(Message::Text(response.to_string().into())).await.is_err() { break; }
            }
        })
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, axum::Router::new().route("/cdp", get(cdp))).await;
    });
    format!("ws://{addr}/cdp")
}
