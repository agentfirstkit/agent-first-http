//! Display-takeover HTTP + WebSocket proxy routes.
//!
//! `ANY /takeover/panel[/*]` proxies to the display provider (currently
//! KasmVNC), which serves its own web UI and RFB-over-WebSocket transport.

#![allow(clippy::useless_conversion)]

use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{OriginalUri, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::host::listener::AppState;

pub async fn display_proxy(
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    request: axum::extract::Request,
) -> Response {
    let Some(display) = state.takeover.clone() else {
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
