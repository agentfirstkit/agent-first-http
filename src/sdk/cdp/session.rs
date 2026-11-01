//! Helpers around target attach / detach for one-shot CDP sessions.

use std::time::Duration;

use serde_json::Value;

use crate::sdk::cdp::ws_client::Connection;
use crate::shared::error::{Error, ErrorCode};

/// Create a fresh blank target and return its `targetId`. Attaching is the
/// caller's next step, kept separate so a target that exists but could not be
/// attached is still the caller's to close.
pub async fn create_blank_target(conn: &Connection) -> Result<String, Error> {
    let target = conn
        .send(
            "Target.createTarget",
            &serde_json::json!({"url": "about:blank"}),
            None,
        )
        .await?;
    target["targetId"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| {
            Error::new(
                ErrorCode::CdpError,
                "Target.createTarget: no targetId returned",
            )
        })
}

pub async fn attach_to_target(conn: &Connection, target_id: &str) -> Result<String, Error> {
    let attach = conn
        .send(
            "Target.attachToTarget",
            &serde_json::json!({"targetId": target_id, "flatten": true}),
            None,
        )
        .await?;
    attach["sessionId"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| {
            Error::new(
                ErrorCode::CdpError,
                "Target.attachToTarget: no sessionId returned",
            )
        })
}

pub async fn close_target(conn: &Connection, target_id: &str) -> Result<(), Error> {
    close_target_preserving_browser(conn, target_id, Duration::from_secs(2)).await
}

/// Keep a headful browser alive when reclaiming its final page. If listing
/// targets or creating a replacement fails, the original target stays open.
/// The whole operation, including the guard, shares one bounded budget.
pub(crate) async fn close_target_preserving_browser(
    conn: &Connection,
    target_id: &str,
    timeout: Duration,
) -> Result<(), Error> {
    tokio::time::timeout(timeout, async {
        let targets = conn
            .send("Target.getTargets", &serde_json::json!({}), None)
            .await?;
        let pages = targets["targetInfos"].as_array().ok_or_else(|| {
            Error::new(
                ErrorCode::CdpError,
                "cannot protect the last browser window: target list unavailable",
            )
        })?;
        // Counting other work tabs races when independent drivers both close.
        // A dedicated inert anchor survives all normal fetch/reaper cleanup.
        let anchor_exists = pages.iter().any(|page| {
            page["type"].as_str() == Some("page")
                && page["targetId"].as_str().is_some_and(|id| id != target_id)
                && page["url"].as_str() == Some("about:blank#afhttp-window-anchor")
        });
        if !anchor_exists {
            let guarded = match conn
                .send(
                    "Target.createTarget",
                    &serde_json::json!({"url":"about:blank#afhttp-window-anchor", "background":true}),
                    None,
                )
                .await
            {
                Ok(replacement) => replacement["targetId"]
                    .as_str()
                    .is_some_and(|id| !id.is_empty() && id != target_id),
                // A browser that holds a single page has no window to lose
                // and cannot host an anchor; closing its page is safe.
                Err(error) if refuses_second_target(&error) => true,
                Err(error) => return Err(error),
            };
            if !guarded {
                return Err(Error::new(
                    ErrorCode::CdpError,
                    "cannot close the last browser window: replacement target was not created",
                ));
            }
        }
        let response = conn
            .send(
                "Target.closeTarget",
                &serde_json::json!({"targetId": target_id}),
                None,
            )
            .await?;
        if response["success"].as_bool() != Some(true) {
            return Err(Error::new(
                ErrorCode::CdpError,
                "Target.closeTarget did not confirm closure",
            ));
        }
        Ok(())
    })
    .await
    .map_err(|_| {
        Error::new(
            ErrorCode::CdpTimeout,
            "safe target cleanup timed out; tab may remain open",
        )
    })?
}

/// Lightpanda answers `Target.createTarget` with `TargetAlreadyLoaded` while
/// its one page exists.
fn refuses_second_target(error: &Error) -> bool {
    error.error_code == ErrorCode::CdpError && error.detail.ends_with(": TargetAlreadyLoaded")
}

pub async fn detach_from_target(conn: &Connection, session_id: &str) -> Result<(), Error> {
    let _ = conn
        .send(
            "Target.detachFromTarget",
            &serde_json::json!({"sessionId": session_id}),
            None,
        )
        .await?;
    Ok(())
}

/// Convenience: send a CDP command with no params, returning the JSON
/// result.
pub async fn call(conn: &Connection, method: &str, session_id: &str) -> Result<Value, Error> {
    conn.send(method, &serde_json::json!({}), Some(session_id))
        .await
}

/// Convenience: send a CDP command and wait up to `timeout` for it. Used
/// by callers that need an explicit ceiling rather than the connection's
/// default.
pub async fn call_with_timeout<P: serde::Serialize>(
    conn: &Connection,
    method: &str,
    params: &P,
    session_id: Option<&str>,
    timeout: Duration,
) -> Result<Value, Error> {
    tokio::time::timeout(timeout, conn.send(method, params, session_id))
        .await
        .map_err(|_| Error::new(ErrorCode::CdpTimeout, format!("CDP {method}: timeout")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{SinkExt, StreamExt};

    #[tokio::test]
    async fn safe_cleanup_keeps_last_page_when_replacement_fails() {
        for malformed_response in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let peer = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                while let Some(Ok(message)) = ws.next().await {
                    let tokio_tungstenite::tungstenite::Message::Text(text) = message else {
                        continue;
                    };
                    let request: Value = serde_json::from_str(&text).unwrap();
                    let id = &request["id"];
                    let response = match request["method"].as_str().unwrap() {
                        "Target.getTargets" => {
                            serde_json::json!({"id":id,"result":{"targetInfos":[{"type":"page","targetId":"last"}]}})
                        }
                        "Target.createTarget" if malformed_response => {
                            serde_json::json!({"id":id,"result":{}})
                        }
                        "Target.createTarget" => {
                            serde_json::json!({"id":id,"error":{"code":-32000,"message":"creation failed"}})
                        }
                        method => {
                            panic!("must not close the last page after guard failure: {method}")
                        }
                    };
                    ws.send(tokio_tungstenite::tungstenite::Message::Text(
                        response.to_string().into(),
                    ))
                    .await
                    .unwrap();
                }
            });
            let conn = Connection::connect(&format!("ws://{addr}"), None)
                .await
                .unwrap();
            assert!(
                close_target_preserving_browser(&conn, "last", Duration::from_secs(1))
                    .await
                    .is_err()
            );
            conn.close();
            peer.await.unwrap();
        }
    }

    #[tokio::test]
    async fn safe_cleanup_closes_the_only_page_of_a_single_target_browser() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut closed = false;
            while let Some(Ok(message)) = ws.next().await {
                let tokio_tungstenite::tungstenite::Message::Text(text) = message else {
                    continue;
                };
                let request: Value = serde_json::from_str(&text).unwrap();
                let id = &request["id"];
                let response = match request["method"].as_str().unwrap() {
                    "Target.getTargets" => {
                        serde_json::json!({"id":id,"result":{"targetInfos":[{"type":"page","targetId":"only"}]}})
                    }
                    "Target.createTarget" => {
                        serde_json::json!({"id":id,"error":{"code":-31998,"message":"TargetAlreadyLoaded"}})
                    }
                    "Target.closeTarget" => {
                        closed = true;
                        serde_json::json!({"id":id,"result":{"success":true}})
                    }
                    method => panic!("unexpected CDP method: {method}"),
                };
                ws.send(tokio_tungstenite::tungstenite::Message::Text(
                    response.to_string().into(),
                ))
                .await
                .unwrap();
            }
            closed
        });
        let conn = Connection::connect(&format!("ws://{addr}"), None)
            .await
            .unwrap();
        close_target_preserving_browser(&conn, "only", Duration::from_secs(1))
            .await
            .unwrap();
        conn.close();
        assert!(peer.await.unwrap(), "the only page must be closed");
    }
}
