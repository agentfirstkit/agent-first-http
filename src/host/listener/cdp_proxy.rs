//! Transparent CDP WebSocket proxy at `/cdp`.
//!
//! Each incoming client gets a fresh WebSocket to the browser's CDP
//! endpoint, and bytes are forwarded bidirectionally. Multiple `/cdp`
//! clients can connect concurrently — chromium accepts many parallel
//! browser-level CDP connections, so multi-attach is delivered by the
//! browser itself rather than by us multiplexing flattened sessions.

// The Message ⇄ tungstenite::Message conversions look like no-ops but
// cross between axum's Bytes/Utf8Bytes wrappers and tungstenite's String/
// Vec<u8>. clippy::useless_conversion doesn't see the wrapper boundary.
#![allow(clippy::useless_conversion)]

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio_tungstenite::tungstenite;

use crate::host::listener::AppState;

#[derive(Deserialize, Default)]
pub struct CdpQuery {
    pub token: Option<String>,
}

pub async fn handler(
    ws: WebSocketUpgrade,
    Query(q): Query<CdpQuery>,
    State(state): State<AppState>,
) -> axum::response::Response {
    let _ = q.token;
    let entry = state.get_profile();
    let backend = match entry {
        Some(e) => Arc::new(e.ws_url().to_string()),
        None => {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(serde_json::json!({
                    "code": "error",
                    "error_code": "cdp_unavailable",
                    "error": "no browser backend is connected yet",
                    "retryable": true,
                })),
            )
                .into_response();
        }
    };
    ws.on_upgrade(move |socket| forward(socket, backend))
}

async fn forward(client: WebSocket, browser_ws_url: Arc<String>) {
    let (mut client_tx, mut client_rx) = client.split();

    let browser_stream = match tokio_tungstenite::connect_async(browser_ws_url.as_str()).await {
        Ok((stream, _resp)) => stream,
        Err(_) => {
            let _ = client_tx.close().await;
            return;
        }
    };
    let (mut browser_tx, mut browser_rx) = browser_stream.split();

    let c2b = async {
        while let Some(Ok(msg)) = client_rx.next().await {
            let outbound = match msg {
                Message::Text(t) => tungstenite::Message::Text(t.as_str().into()),
                Message::Binary(b) => tungstenite::Message::Binary(b.to_vec().into()),
                Message::Ping(p) => tungstenite::Message::Ping(p.to_vec().into()),
                Message::Pong(p) => tungstenite::Message::Pong(p.to_vec().into()),
                Message::Close(_) => break,
            };
            if browser_tx.send(outbound).await.is_err() {
                break;
            }
        }
        let _ = browser_tx.send(tungstenite::Message::Close(None)).await;
    };
    let b2c = async {
        while let Some(Ok(msg)) = browser_rx.next().await {
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
    tokio::pin!(c2b);
    tokio::pin!(b2c);
    tokio::select! {
        _ = &mut c2b => {},
        _ = &mut b2c => {},
    }
}
