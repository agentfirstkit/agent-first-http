//! Display-takeover HTTP + WebSocket proxy routes.
//!
//! `ANY /takeover/panel[/*]` proxies to the display provider (currently
//! KasmVNC), which serves its own web UI and RFB-over-WebSocket transport.

#![allow(clippy::useless_conversion)]

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
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
    // Gate document loads and WebSocket upgrades. Static assets do not each
    // need an independent CDP connection while a client is bootstrapping.
    let path = uri.path();
    if ws.is_ok()
        || path.ends_with("/panel")
        || path.ends_with('/')
        || path.ends_with(".html")
        || path.ends_with("/afhttp-status")
    {
        match super::health::require_takeover_ready(&state).await {
            Ok(health) if path.ends_with("/afhttp-status") => {
                return axum::Json(crate::shared::afdata::result_value(
                    "takeover_health",
                    serde_json::to_value(health).unwrap_or_default(),
                ))
                .into_response();
            }
            Ok(_) => {}
            Err(error) => return service_unavailable(&error.detail),
        }
    }
    display.proxy(ws, uri, request).await
}

fn service_unavailable(reason: &str) -> Response {
    let body = crate::shared::afdata::error_value("cdp_unavailable", reason, true);
    (StatusCode::SERVICE_UNAVAILABLE, axum::Json(body)).into_response()
}
