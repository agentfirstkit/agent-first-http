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

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio_tungstenite::tungstenite;

use crate::host::listener::AppState;
use crate::host::listener::target_ledger::TargetLedger;
use crate::shared::error::ErrorCode;

#[derive(Deserialize, Default)]
pub struct CdpQuery {
    pub token: Option<String>,
    /// Optional profile selector. When set and different from the host's active
    /// profile, the host relaunches its browser under this profile before
    /// proxying (see [`AppState::ensure_profile`]).
    pub profile: Option<String>,
}

pub async fn handler(
    ws: WebSocketUpgrade,
    Query(q): Query<CdpQuery>,
    State(state): State<AppState>,
) -> axum::response::Response {
    let _ = q.token;
    if let Some(name) = q.profile.as_deref()
        && let Err(e) = state.ensure_profile(name).await
    {
        let status = match e.error_code {
            ErrorCode::InvalidArgument | ErrorCode::ProfileInvalidName => {
                axum::http::StatusCode::BAD_REQUEST
            }
            ErrorCode::ProfileLocked => axum::http::StatusCode::CONFLICT,
            _ => axum::http::StatusCode::SERVICE_UNAVAILABLE,
        };
        return (
            status,
            axum::Json(crate::shared::afdata::error_value(
                e.error_code.as_str(),
                &format!("profile switch to {name:?} failed: {}", e.detail),
                false,
            )),
        )
            .into_response();
    }
    // Counted from here, not from the upgrade: a profile switch reads the
    // count to decide whether anyone is on the current browser, and this
    // client is from the moment it has picked one.
    let client = state.cdp_client_guard();
    let entry = match state.get_profile() {
        Some(entry) => entry,
        None => {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(crate::shared::afdata::error_value(
                    "cdp_unavailable",
                    "no browser backend is connected yet",
                    true,
                )),
            )
                .into_response();
        }
    };
    ws.on_upgrade(move |socket| async move {
        // Both held for the connection's lifetime. The guard: while any proxy
        // client is connected the host counts as in use, which is what keeps
        // the idle-tab reaper's hands off the tabs it cannot attribute. The
        // entry: its browser outlives a switch that lands anyway, instead of
        // being torn down under a session that only knew its address.
        let _client = client;
        let ledger = forward(socket, entry.ws_url(), &state).await;
        drop(entry);
        // Whatever this driver held and never closed is released; a tab no
        // other live connection holds is nobody's now.
        state.release_targets(ledger.into_owned());
    })
}

const UPSTREAM_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// How long one frame may wait for the other side to take it. A peer that
/// stops reading without closing — a wedged browser, a client gone without a
/// FIN — would otherwise park the forwarder in `send` for good, and with it
/// the client guard that marks the host in use.
const FORWARD_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

async fn forward(client: WebSocket, browser_ws_url: &str, state: &AppState) -> TargetLedger {
    let ledger = std::sync::Mutex::new(TargetLedger::default());
    let (mut client_tx, mut client_rx) = client.split();

    // A wedged browser can accept the connection and never finish the
    // handshake; the client's guard would then keep the host "in use" forever.
    let upstream = tokio::time::timeout(
        UPSTREAM_CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async(browser_ws_url),
    )
    .await;
    let browser_stream = match upstream {
        Ok(Ok((stream, _resp))) => stream,
        _ => {
            let _ = client_tx.close().await;
            return ledger.into_inner().unwrap_or_else(|e| e.into_inner());
        }
    };
    let (mut browser_tx, mut browser_rx) = browser_stream.split();

    // The forwarding futures borrow the ledger; they end before it is read.
    {
        let c2b = async {
            while let Some(Ok(msg)) = client_rx.next().await {
                let outbound = match msg {
                    Message::Text(t) => {
                        let holding = ledger
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .observe_request(t.as_str());
                        state.note_holding(holding);
                        tungstenite::Message::Text(t.as_str().into())
                    }
                    Message::Binary(b) => tungstenite::Message::Binary(b.to_vec().into()),
                    Message::Ping(p) => tungstenite::Message::Ping(p.to_vec().into()),
                    Message::Pong(p) => tungstenite::Message::Pong(p.to_vec().into()),
                    Message::Close(_) => break,
                };
                if !matches!(
                    tokio::time::timeout(FORWARD_SEND_TIMEOUT, browser_tx.send(outbound)).await,
                    Ok(Ok(()))
                ) {
                    break;
                }
            }
            let _ = tokio::time::timeout(
                FORWARD_SEND_TIMEOUT,
                browser_tx.send(tungstenite::Message::Close(None)),
            )
            .await;
        };
        let b2c = async {
            while let Some(Ok(msg)) = browser_rx.next().await {
                let inbound = match msg {
                    tungstenite::Message::Text(t) => {
                        let holding = ledger
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .observe_response(t.as_str());
                        state.note_holding(holding);
                        Message::Text(t.as_str().into())
                    }
                    tungstenite::Message::Binary(b) => Message::Binary(b.to_vec().into()),
                    tungstenite::Message::Ping(p) => Message::Ping(p.to_vec().into()),
                    tungstenite::Message::Pong(p) => Message::Pong(p.to_vec().into()),
                    tungstenite::Message::Close(_) => break,
                    tungstenite::Message::Frame(_) => continue,
                };
                if !matches!(
                    tokio::time::timeout(FORWARD_SEND_TIMEOUT, client_tx.send(inbound)).await,
                    Ok(Ok(()))
                ) {
                    break;
                }
            }
            let _ = tokio::time::timeout(FORWARD_SEND_TIMEOUT, client_tx.close()).await;
        };
        tokio::pin!(c2b);
        tokio::pin!(b2c);
        tokio::select! {
            _ = &mut c2b => {},
            _ = &mut b2c => {},
        }
    }
    ledger.into_inner().unwrap_or_else(|e| e.into_inner())
}
