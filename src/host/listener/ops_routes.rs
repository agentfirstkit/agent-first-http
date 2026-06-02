//! Ops panel HTTP + WebSocket routes.
//!
//! `GET /ops` serves the embedded HTML; `GET /ops/assets/*` serves the JS
//! and CSS bundle. `WS /ops/screencast` runs the real Page.startScreencast
//! frame relay (see `host::ops_panel::screencast`); `WS /ops/input` runs
//! the timing-preserved input replay (see `host::ops_panel::input_relay`).

#![allow(clippy::useless_conversion)]

use axum::body::{to_bytes, Body};
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{OriginalUri, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite;

use crate::host::listener::AppState;
use crate::host::ops_panel::{assets, input_relay, screencast};

/// KasmVNC client quality settings seeded on the display panel (the client's
/// values override the server's). The `pct` (0-100, from `--display-quality-percent`)
/// maps to KasmVNC's 0-9 JPEG quality tiers. Static/idle content can always
/// climb to tier 9 (`dynamic_quality_max`); `pct` sets the floor for moving
/// content. Regardless of `pct` we stop the client's default 960x540 "video
/// mode" downscale (`max_video_resolution`), which is what blurs detailed
/// images like captcha challenges while they load/animate.
fn display_quality_params(pct: u8) -> String {
    let level = (u32::from(pct.min(100)) * 9 + 50) / 100; // 0-9, rounded
    format!(
        "&quality={level}&dynamic_quality_min={level}&dynamic_quality_max=9\
         &jpeg_video_quality={level}&webp_video_quality={level}\
         &max_video_resolution_x=3840&max_video_resolution_y=2160"
    )
}

pub async fn index(State(_state): State<AppState>) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        assets::INDEX_HTML,
    )
        .into_response()
}

pub async fn js(State(_state): State<AppState>) -> Response {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        assets::APP_JS,
    )
        .into_response()
}

pub async fn css(State(_state): State<AppState>) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        assets::APP_CSS,
    )
        .into_response()
}

pub async fn screencast_route(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    let Some(browser_ws_url) = state.get_profile().map(|e| e.ws_url().to_string()) else {
        return service_unavailable("screencast: no backend connected");
    };
    ws.on_upgrade(move |socket| async move {
        screencast::run(socket, &browser_ws_url).await;
    })
}

pub async fn input_route(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    let Some(browser_ws_url) = state.get_profile().map(|e| e.ws_url().to_string()) else {
        return service_unavailable("input: no backend connected");
    };
    ws.on_upgrade(move |socket| async move {
        input_relay::run(socket, &browser_ws_url).await;
    })
}

pub async fn display_proxy(
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    request: axum::extract::Request,
) -> Response {
    let Some(display) = state.display_takeover.clone() else {
        return service_unavailable("display takeover is not enabled");
    };

    // noVNC builds its WebSocket URL from the host root (default
    // `/websockify`), ignoring this `/ops/display/` mount prefix — so without
    // help the client connects to `/websockify` and 404s. Seed noVNC's `path`
    // setting via query param so it targets the proxied
    // `/ops/display/websockify`. Applied by redirecting the landing page when
    // the param is absent; the redirect also normalizes the missing trailing
    // slash (axum's `{*path}` wildcard never matches the bare `/ops/display/`).
    // `resize=remote` makes the framebuffer (and the WM-maximized browser)
    // track the client window for an exact fit; without a window manager fall
    // back to `resize=scale` (letterboxed) since the browser window can't
    // follow a framebuffer resize on its own.
    let resize = if display.window_manager {
        "remote"
    } else {
        "scale"
    };
    let is_landing = matches!(uri.path(), "/ops/display" | "/ops/display/");
    let q = uri.query().unwrap_or("");
    let has_path = q.split('&').any(|p| p.starts_with("path="));
    // Match the exact preferred value, not just presence, so a client that
    // cached `resize=scale` from before a window manager was available gets
    // upgraded to `resize=remote` on its next landing hit.
    let want_resize = format!("resize={resize}");
    let resize_ok = q.split('&').any(|p| p == want_resize);
    if is_landing && ws.is_err() && !(has_path && resize_ok) {
        // Rebuild canonically, dropping any stale `path`/`resize` and keeping
        // the rest (e.g. `token_secret`). Redirecting whenever *either* is missing
        // also fixes clients that cached a `?path=…` URL from before `resize`
        // existed. Once both are present the condition is false — no loop.
        let quality = display_quality_params(display.quality);
        let mut location =
            format!("/ops/display/?path=ops/display/websockify&resize={resize}{quality}");
        for pair in q.split('&') {
            if pair.is_empty() || pair.starts_with("path=") || pair.starts_with("resize=") {
                continue;
            }
            location.push('&');
            location.push_str(pair);
        }
        return (
            StatusCode::TEMPORARY_REDIRECT,
            [(header::LOCATION, location)],
        )
            .into_response();
    }

    let upstream_path = display_upstream_path_and_query(&uri);
    if let Ok(ws) = ws {
        let upstream = format!("ws://{}{}", display.web_addr, upstream_path);
        // KasmVNC's websockify speaks the `binary` WebSocket subprotocol and
        // closes the stream if it isn't negotiated. Echo it back to the
        // browser (it always offers `binary`) so noVNC accepts the socket;
        // the upstream leg requests it in `forward_display_ws`.
        return ws
            .protocols(["binary"])
            .on_upgrade(move |socket| forward_display_ws(socket, upstream));
    }

    forward_display_http(display.web_addr, upstream_path, request).await
}

async fn forward_display_http(
    upstream: std::net::SocketAddr,
    path_and_query: String,
    request: axum::extract::Request,
) -> Response {
    let (parts, body) = request.into_parts();
    let url = format!("http://{upstream}{path_and_query}");
    let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes())
        .unwrap_or(reqwest::Method::GET);
    let body = match to_bytes(body, 64 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("display proxy: could not read request body: {e}"),
            )
                .into_response();
        }
    };

    let client = reqwest::Client::new();
    let mut upstream_req = client.request(method, url).body(body);
    for (name, value) in &parts.headers {
        if should_forward_header(name) {
            upstream_req = upstream_req.header(name, value);
        }
    }
    upstream_req = upstream_req.header(header::HOST.as_str(), upstream.to_string());

    let resp = match upstream_req.send().await {
        Ok(resp) => resp,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("display proxy: upstream request failed: {e}"),
            )
                .into_response();
        }
    };

    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut headers = HeaderMap::new();
    for (name, value) in resp.headers() {
        if should_forward_response_header(name) {
            headers.insert(name.clone(), value.clone());
        }
    }
    let bytes = match resp.bytes().await {
        Ok(bytes) => bytes,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("display proxy: upstream body failed: {e}"),
            )
                .into_response();
        }
    };

    (status, headers, Body::from(bytes)).into_response()
}

async fn forward_display_ws(client: WebSocket, upstream_url: String) {
    use tungstenite::client::IntoClientRequest;
    let (mut client_tx, mut client_rx) = client.split();
    // Request KasmVNC's `binary` subprotocol on the upstream leg; without it
    // websockify refuses to bridge the RFB stream (101 then immediate close).
    let upstream_req = match upstream_url.as_str().into_client_request() {
        Ok(mut req) => {
            req.headers_mut().insert(
                tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL,
                tungstenite::http::HeaderValue::from_static("binary"),
            );
            // KasmVNC rejects WS upgrades that lack an `Origin` header (404),
            // so synthesize one matching the upstream authority. The browser's
            // own Origin points at afhttp's listener, not KasmVNC, so it can't
            // be forwarded verbatim.
            if let Some(authority) = req.uri().authority().map(|a| a.to_string()) {
                if let Ok(origin) =
                    tungstenite::http::HeaderValue::from_str(&format!("http://{authority}"))
                {
                    req.headers_mut()
                        .insert(tungstenite::http::header::ORIGIN, origin);
                }
            }
            req
        }
        Err(_) => {
            let _ = client_tx.close().await;
            return;
        }
    };
    let upstream_stream = match tokio_tungstenite::connect_async(upstream_req).await {
        Ok((stream, _resp)) => stream,
        Err(_) => {
            let _ = client_tx.close().await;
            return;
        }
    };
    let (mut upstream_tx, mut upstream_rx) = upstream_stream.split();

    let c2u = async {
        while let Some(Ok(msg)) = client_rx.next().await {
            let outbound = match msg {
                Message::Text(t) => tungstenite::Message::Text(t.as_str().into()),
                Message::Binary(b) => tungstenite::Message::Binary(b.to_vec().into()),
                Message::Ping(p) => tungstenite::Message::Ping(p.to_vec().into()),
                Message::Pong(p) => tungstenite::Message::Pong(p.to_vec().into()),
                Message::Close(_) => break,
            };
            if upstream_tx.send(outbound).await.is_err() {
                break;
            }
        }
        let _ = upstream_tx.send(tungstenite::Message::Close(None)).await;
    };
    let u2c = async {
        while let Some(Ok(msg)) = upstream_rx.next().await {
            let inbound = match msg {
                tungstenite::Message::Text(t) => Message::Text(t.as_str().into()),
                tungstenite::Message::Binary(b) => Message::Binary(b.to_vec().into()),
                tungstenite::Message::Ping(p) => Message::Ping(p.to_vec().into()),
                tungstenite::Message::Pong(p) => Message::Pong(p.to_vec().into()),
                tungstenite::Message::Close(_) => break,
                tungstenite::Message::Frame(_) => continue,
            };
            if client_tx.send(inbound).await.is_err() {
                break;
            }
        }
        let _ = client_tx.close().await;
    };
    tokio::pin!(c2u);
    tokio::pin!(u2c);
    tokio::select! {
        _ = &mut c2u => {},
        _ = &mut u2c => {},
    }
}

fn display_upstream_path_and_query(uri: &Uri) -> String {
    let suffix = uri
        .path()
        .strip_prefix("/ops/display")
        .filter(|s| !s.is_empty())
        .unwrap_or("/");
    let path = if suffix.starts_with('/') {
        suffix.to_string()
    } else {
        format!("/{suffix}")
    };
    let Some(query) = uri.query() else {
        return path;
    };
    let filtered: Vec<&str> = query
        .split('&')
        .filter(|pair| !pair.starts_with("token_secret="))
        .collect();
    if filtered.is_empty() {
        path
    } else {
        format!("{path}?{}", filtered.join("&"))
    }
}

fn should_forward_header(name: &header::HeaderName) -> bool {
    !matches!(
        name.as_str().to_ascii_lowercase().as_str(),
        "host"
            | "connection"
            | "upgrade"
            | "content-length"
            | "sec-websocket-key"
            | "sec-websocket-version"
            | "sec-websocket-protocol"
            | "sec-websocket-extensions"
            | "authorization"
            | "cookie"
    )
}

fn should_forward_response_header(name: &header::HeaderName) -> bool {
    !matches!(
        name.as_str().to_ascii_lowercase().as_str(),
        "connection" | "transfer-encoding" | "upgrade" | "content-length"
    )
}

fn service_unavailable(reason: &str) -> Response {
    let body = serde_json::json!({
        "code": "error",
        "error_code": "cdp_unavailable",
        "error": reason,
        "retryable": true,
    });
    (StatusCode::SERVICE_UNAVAILABLE, axum::Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::display_quality_params;

    #[test]
    fn quality_pct_maps_to_kasmvnc_tiers() {
        assert!(display_quality_params(100).contains("&quality=9&"));
        assert!(display_quality_params(0).contains("&quality=0&"));
        assert!(display_quality_params(50).contains("&quality=5&"));
        // Clamps above 100.
        assert!(display_quality_params(200).contains("&quality=9&"));
        // Never downscales, regardless of quality.
        assert!(display_quality_params(10).contains("max_video_resolution_x=3840"));
    }
}
