//! Helpers around target attach / detach for one-shot CDP sessions.

use std::time::Duration;

use serde_json::Value;

use crate::sdk::cdp::ws_client::Connection;
use crate::shared::error::{Error, ErrorCode};

/// Attach to a fresh target and return its `targetId` + `sessionId`.
pub async fn open_blank_target(conn: &Connection) -> Result<(String, String), Error> {
    let target = conn
        .send(
            "Target.createTarget",
            &serde_json::json!({"url": "about:blank"}),
            None,
        )
        .await?;
    let target_id = target["targetId"]
        .as_str()
        .ok_or_else(|| {
            Error::new(
                ErrorCode::CdpError,
                "Target.createTarget: no targetId returned",
            )
        })?
        .to_string();
    let session_id = attach_to_target(conn, &target_id).await?;
    Ok((target_id, session_id))
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
    let _ = conn
        .send(
            "Target.closeTarget",
            &serde_json::json!({"targetId": target_id}),
            None,
        )
        .await?;
    Ok(())
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
