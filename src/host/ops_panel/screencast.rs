//! Page.startScreencast frame relay helpers for `/ops/screencast/ws`.
//!
//! Each WS client gets a fresh CDP connection to the local browser, picks
//! a page target, attaches a flattened session, starts a JPEG screencast,
//! and forwards `Page.screencastFrame` events to the WS as binary frames.
//! Each frame is acked immediately after queuing the WS send so chromium
//! keeps emitting; the ack runs before the WS flush so a slow client only
//! drops frames, never stalls capture.

use axum::extract::ws::{Message, WebSocket};
use base64::Engine;
use futures::{SinkExt, StreamExt};

use crate::sdk::cdp::ws_client::Connection;

/// Resolve a page target to attach to. Preference order, newest first:
///   1. A user-meaningful page (http/https/file/data/blob) — what the
///      agent actually fetched.
///   2. A chromium-internal page (`chrome://`, `chrome-untrusted://`,
///      `devtools://`, `chrome-extension://`) — typically the New Tab
///      Page chromium parks an unused tab on. Useful when the panel is
///      opened before any fetch has run, but never preferred over a
///      real page the agent navigated to.
///   3. `about:blank` and any other URL, as a last resort.
///
/// The earlier (simpler) heuristic just skipped `about:` prefixes, which
/// silently broke when chromium transitioned the default tab from
/// `about:blank` to `chrome://newtab/` — events landed on the New Tab
/// Page instead of the agent's tab.
pub async fn resolve_page_target(conn: &Connection) -> Option<String> {
    let targets = conn
        .send("Target.getTargets", &serde_json::json!({}), None)
        .await
        .ok()?;
    let arr = targets.get("targetInfos")?.as_array()?;
    let mut pages: Vec<(&str, &str)> = Vec::new();
    for t in arr {
        if t.get("type").and_then(|v| v.as_str()) == Some("page") {
            let id = t.get("targetId").and_then(|v| v.as_str())?;
            let url = t.get("url").and_then(|v| v.as_str()).unwrap_or("");
            pages.push((id, url));
        }
    }
    if pages.is_empty() {
        return None;
    }
    if let Some((id, _)) = pages.iter().rev().find(|(_, url)| is_user_page(url)) {
        return Some((*id).to_string());
    }
    if let Some((id, _)) = pages.iter().rev().find(|(_, url)| is_browser_internal(url)) {
        return Some((*id).to_string());
    }
    pages.last().map(|(id, _)| (*id).to_string())
}

fn is_user_page(url: &str) -> bool {
    url.starts_with("http://")
        || url.starts_with("https://")
        || url.starts_with("file://")
        || url.starts_with("data:")
        || url.starts_with("blob:")
}

fn is_browser_internal(url: &str) -> bool {
    url.starts_with("chrome://")
        || url.starts_with("chrome-untrusted://")
        || url.starts_with("chrome-extension://")
        || url.starts_with("devtools://")
        || url.starts_with("edge://")
        || url.starts_with("brave://")
        || url.starts_with("view-source:")
}

/// Run the screencast loop: forward frames until either side closes.
pub async fn run(client_ws: WebSocket, browser_ws_url: &str) {
    let conn = match Connection::connect(browser_ws_url, None).await {
        Ok(c) => c,
        Err(_) => {
            let _ = close_with_error(client_ws, "browser connect failed").await;
            return;
        }
    };
    let Some(target_id) = resolve_page_target(&conn).await else {
        let _ = close_with_error(client_ws, "no page target available").await;
        return;
    };
    let attach = match conn
        .send(
            "Target.attachToTarget",
            &serde_json::json!({"targetId": target_id, "flatten": true}),
            None,
        )
        .await
    {
        Ok(v) => v,
        Err(_) => {
            let _ = close_with_error(client_ws, "attach failed").await;
            return;
        }
    };
    let Some(session_id) = attach["sessionId"].as_str().map(str::to_string) else {
        let _ = close_with_error(client_ws, "no session id").await;
        return;
    };

    let _ = conn
        .send("Page.enable", &serde_json::json!({}), Some(&session_id))
        .await;
    if conn
        .send(
            "Page.startScreencast",
            &serde_json::json!({
                "format": "jpeg",
                "quality": 60,
                "maxWidth": 1280,
                "maxHeight": 720,
                "everyNthFrame": 1,
            }),
            Some(&session_id),
        )
        .await
        .is_err()
    {
        let _ = close_with_error(client_ws, "startScreencast failed").await;
        return;
    }

    forward_frames(client_ws, conn, session_id, target_id).await;
}

async fn forward_frames(
    client_ws: WebSocket,
    conn: Connection,
    session_id: String,
    target_id: String,
) {
    let (mut tx, mut rx) = client_ws.split();
    let mut events = conn.subscribe();

    // Inbound: drain client messages and exit on Close.
    let client_drain = async {
        while let Some(Ok(msg)) = rx.next().await {
            if let Message::Close(_) = msg {
                break;
            }
        }
    };

    // Outbound: forward frames + ack each.
    let sid = session_id.clone();
    let frame_loop = async {
        // Last viewport metadata sent to the client. The screencast frame is
        // the target viewport scaled to fit maxWidth/maxHeight, so the frame's
        // pixel size is NOT the page's CSS pixel size whenever the viewport
        // isn't exactly 1280×720 (headless chromium defaults to 800×600 →
        // 960×720 frames). The client needs the real CSS deviceWidth/Height to
        // map operator clicks to CSS pixels; without it clicks land off by the
        // scale factor, an error that grows with distance from the origin.
        let mut last_meta: Option<String> = None;
        loop {
            let ev = match events.recv().await {
                Ok(e) => e,
                Err(_) => break,
            };
            if ev.method != "Page.screencastFrame" || ev.session_id.as_deref() != Some(&sid) {
                continue;
            }
            let Some(b64) = ev.params.get("data").and_then(|v| v.as_str()) else {
                continue;
            };
            // Forward viewport metadata (CSS px) ahead of the frame, but only
            // when it changes — typically once, then again on a resize.
            if let Some(md) = ev.params.get("metadata") {
                if let (Some(dw), Some(dh)) = (
                    md.get("deviceWidth").and_then(serde_json::Value::as_f64),
                    md.get("deviceHeight").and_then(serde_json::Value::as_f64),
                ) {
                    let meta = serde_json::json!({
                        "type": "meta",
                        "deviceWidth": dw,
                        "deviceHeight": dh,
                        "offsetTop": md
                            .get("offsetTop")
                            .and_then(serde_json::Value::as_f64)
                            .unwrap_or(0.0),
                        "pageScaleFactor": md
                            .get("pageScaleFactor")
                            .and_then(serde_json::Value::as_f64)
                            .unwrap_or(1.0),
                    })
                    .to_string();
                    if last_meta.as_deref() != Some(meta.as_str()) {
                        if tx.send(Message::Text(meta.clone().into())).await.is_err() {
                            break;
                        }
                        last_meta = Some(meta);
                    }
                }
            }
            let session_for_ack = ev
                .params
                .get("sessionId")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let jpeg = match base64::engine::general_purpose::STANDARD.decode(b64) {
                Ok(b) => b,
                Err(_) => continue,
            };
            if tx.send(Message::Binary(jpeg.into())).await.is_err() {
                break;
            }
            // Ack so chromium keeps emitting; spawn so a slow ack doesn't
            // block the next frame.
            let conn_for_ack = &conn;
            let _ = conn_for_ack
                .send(
                    "Page.screencastFrameAck",
                    &serde_json::json!({"sessionId": session_for_ack}),
                    Some(&sid),
                )
                .await;
        }
    };

    tokio::pin!(client_drain);
    tokio::pin!(frame_loop);
    tokio::select! {
        _ = &mut client_drain => {},
        _ = &mut frame_loop => {},
    }

    let _ = conn
        .send(
            "Page.stopScreencast",
            &serde_json::json!({}),
            Some(&session_id),
        )
        .await;
    let _ = conn
        .send(
            "Target.detachFromTarget",
            &serde_json::json!({"sessionId": session_id}),
            None,
        )
        .await;
    let _ = target_id; // session is gone; target stays alive for other clients
    conn.close();
    // tx + rx drop here, which signals the WS close frame to the client.
}

async fn close_with_error(mut client_ws: WebSocket, reason: &str) -> Result<(), axum::Error> {
    let body = serde_json::json!({
        "code": "ops_error",
        "channel": "screencast",
        "error": reason,
    });
    let _ = client_ws.send(Message::Text(body.to_string().into())).await;
    client_ws.close().await
}

#[cfg(test)]
mod tests {
    use super::{is_browser_internal, is_user_page};

    #[test]
    fn user_pages_match_http_data_file_blob() {
        for u in [
            "http://example.com/",
            "https://example.com/",
            "data:text/html,<p>hi</p>",
            "file:///tmp/x.html",
            "blob:https://example.com/abcd",
        ] {
            assert!(is_user_page(u), "{u} should be a user page");
            assert!(!is_browser_internal(u), "{u} must not be browser-internal");
        }
    }

    #[test]
    fn browser_internals_match_chromium_schemes() {
        for u in [
            "chrome://newtab/",
            "chrome-untrusted://new-tab-page/one.html",
            "chrome-extension://abc/page.html",
            "devtools://devtools/bundled/inspector.html",
            "edge://settings",
            "brave://settings",
            "view-source:https://example.com/",
        ] {
            assert!(is_browser_internal(u), "{u} should be browser-internal");
            assert!(!is_user_page(u), "{u} must not be a user page");
        }
    }

    #[test]
    fn about_blank_is_neither_user_nor_internal() {
        // Falls through to the third tier in resolve_page_target.
        assert!(!is_user_page("about:blank"));
        assert!(!is_browser_internal("about:blank"));
    }
}
