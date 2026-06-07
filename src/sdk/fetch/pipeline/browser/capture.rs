//! Page/DOM/body capture helpers for the browser path: location and snapshot
//! reads, rendered HTML / inner text / screenshot, and response-body decoding.

use serde_json::Value;

use crate::sdk::cdp::ws_client::Connection;
use crate::sdk::fetch::deadline::FetchDeadline;
use crate::shared::error::{Error, ErrorCode};

use super::cdp_send;

#[derive(Debug, Clone)]
pub(super) struct PageSnapshot {
    pub(super) url: String,
    pub(super) title: String,
    pub(super) ready_state: String,
    pub(super) text: String,
    pub(super) html: String,
}

impl PageSnapshot {
    pub(super) fn has_dom_content(&self) -> bool {
        !self.html.trim().is_empty()
            && self.html.trim() != "<html><head></head><body></body></html>"
    }
}

pub(super) async fn capture_location(
    conn: &Connection,
    session_id: &str,
    deadline: &FetchDeadline,
) -> Result<Option<String>, Error> {
    let doc = cdp_send(
        conn,
        session_id,
        "Runtime.evaluate",
        &serde_json::json!({
            "expression": "location.href",
            "returnByValue": true,
        }),
        "capture_location",
        deadline,
    )
    .await?;
    Ok(doc["result"]["value"].as_str().map(str::to_string))
}

pub(super) async fn capture_page_snapshot(
    conn: &Connection,
    session_id: &str,
    deadline: &FetchDeadline,
) -> Result<PageSnapshot, Error> {
    let doc = cdp_send(
        conn,
        session_id,
        "Runtime.evaluate",
        &serde_json::json!({
            "expression": PAGE_SNAPSHOT_JS,
            "returnByValue": true,
        }),
        "capture_page_snapshot",
        deadline,
    )
    .await?;
    let s = doc["result"]["value"].as_str().unwrap_or("{}");
    let v: Value = serde_json::from_str(s).unwrap_or(Value::Null);
    Ok(PageSnapshot {
        url: v["url"].as_str().unwrap_or("").to_string(),
        title: v["title"].as_str().unwrap_or("").to_string(),
        ready_state: v["ready_state"].as_str().unwrap_or("").to_string(),
        text: v["text"].as_str().unwrap_or("").to_string(),
        html: v["html"].as_str().unwrap_or("").to_string(),
    })
}

const PAGE_SNAPSHOT_JS: &str = r#"(() => {
  const text = document.body ? document.body.innerText : '';
  const html = document.documentElement ? document.documentElement.outerHTML : '';
  return JSON.stringify({
    url: location.href,
    title: document.title || '',
    ready_state: document.readyState || '',
    text: text.slice(0, 200000),
    html: html.slice(0, 200000)
  });
})()"#;

pub(super) async fn capture_response_body(
    conn: &Connection,
    session_id: &str,
    request_id: &str,
    deadline: &FetchDeadline,
) -> Result<Vec<u8>, Error> {
    let resp = cdp_send(
        conn,
        session_id,
        "Network.getResponseBody",
        &serde_json::json!({"requestId": request_id}),
        "capture_body",
        deadline,
    )
    .await?;
    decode_response_body(request_id, &resp)
}

pub(super) fn decode_response_body(
    request_id: &str,
    resp: &serde_json::Value,
) -> Result<Vec<u8>, Error> {
    let body_str = resp.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let base64_encoded = resp
        .get("base64Encoded")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if base64_encoded {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(body_str)
            .map_err(|e| {
                Error::new(
                    ErrorCode::ArtifactCaptureFailed,
                    format!("base64 decode for {request_id}: {e}"),
                )
            })
    } else {
        Ok(body_str.as_bytes().to_vec())
    }
}

pub(super) async fn navigation_status_from_performance(
    conn: &Connection,
    session_id: &str,
    deadline: &FetchDeadline,
) -> Option<u16> {
    let r = cdp_send(
        conn,
        session_id,
        "Runtime.evaluate",
        &serde_json::json!({
            "expression": "(() => { const nav = performance.getEntriesByType('navigation')[0]; return nav && Number.isFinite(nav.responseStatus) ? nav.responseStatus : 0; })()",
            "returnByValue": true,
        }),
        "capture_status",
        deadline,
    )
    .await
    .ok()?;
    let status = r["result"]["value"].as_u64()?;
    if (100..=599).contains(&status) {
        Some(status as u16)
    } else {
        None
    }
}

pub(super) async fn capture_outer_html(
    conn: &Connection,
    session_id: &str,
    deadline: &FetchDeadline,
) -> Result<String, Error> {
    if let Ok(html) = capture_outer_html_via_runtime(conn, session_id, deadline).await {
        if !html.is_empty() {
            return Ok(html);
        }
    }
    let dom_outer = async {
        let doc = cdp_send(
            conn,
            session_id,
            "DOM.getDocument",
            &serde_json::json!({"depth": -1, "pierce": true}),
            "capture_rendered_html",
            deadline,
        )
        .await?;
        let node_id = doc["root"]["nodeId"].as_i64().ok_or_else(|| {
            Error::new(ErrorCode::CdpError, "DOM.getDocument: missing root nodeId")
        })?;
        let outer = cdp_send(
            conn,
            session_id,
            "DOM.getOuterHTML",
            &serde_json::json!({"nodeId": node_id}),
            "capture_rendered_html",
            deadline,
        )
        .await?;
        outer["outerHTML"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| Error::new(ErrorCode::CdpError, "DOM.getOuterHTML: missing outerHTML"))
    }
    .await;

    match dom_outer {
        Ok(html) if !html.is_empty() => Ok(html),
        Err(e) => Err(e),
        _ => Ok(String::new()),
    }
}

async fn capture_outer_html_via_runtime(
    conn: &Connection,
    session_id: &str,
    deadline: &FetchDeadline,
) -> Result<String, Error> {
    let r = cdp_send(
        conn,
        session_id,
        "Runtime.evaluate",
        &serde_json::json!({
            "expression": "document.documentElement ? document.documentElement.outerHTML : ''",
            "returnByValue": true,
        }),
        "capture_rendered_html",
        deadline,
    )
    .await?;
    Ok(r["result"]["value"].as_str().unwrap_or("").to_string())
}

pub(super) async fn capture_inner_text(
    conn: &Connection,
    session_id: &str,
    deadline: &FetchDeadline,
) -> Result<String, Error> {
    let r = cdp_send(
        conn,
        session_id,
        "Runtime.evaluate",
        &serde_json::json!({
            "expression": "document.body ? document.body.innerText : ''",
            "returnByValue": true,
        }),
        "capture_text",
        deadline,
    )
    .await?;
    Ok(r["result"]["value"].as_str().unwrap_or("").to_string())
}

pub(super) async fn capture_screenshot(
    conn: &Connection,
    session_id: &str,
    deadline: &FetchDeadline,
) -> Result<Vec<u8>, Error> {
    let r = cdp_send(
        conn,
        session_id,
        "Page.captureScreenshot",
        &serde_json::json!({"format": "png", "captureBeyondViewport": false}),
        "capture_screenshot",
        deadline,
    )
    .await?;
    let b64 = r["data"]
        .as_str()
        .ok_or_else(|| Error::new(ErrorCode::CdpError, "captureScreenshot: missing data"))?;
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| {
            Error::new(
                ErrorCode::ArtifactCaptureFailed,
                format!("screenshot base64 decode: {e}"),
            )
        })
}
