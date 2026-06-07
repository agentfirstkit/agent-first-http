//! Ops panel HTTP + WebSocket routes.
//!
//! `GET /ops/screencast` serves the embedded HTML; `GET
//! /ops/screencast/assets/*` serves the JS and CSS bundle. `WS
//! /ops/screencast/ws` runs the real Page.startScreencast frame relay (see
//! `host::ops_panel::screencast`); `WS /ops/screencast/input` runs the
//! timing-preserved input replay (see `host::ops_panel::input_relay`).

#![allow(clippy::useless_conversion)]

use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{OriginalUri, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::host::listener::AppState;
use crate::host::ops_panel::{assets, input_relay, screencast};

pub async fn screencast_entry(State(_state): State<AppState>) -> Response {
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
    display.proxy(ws, uri, request).await
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
