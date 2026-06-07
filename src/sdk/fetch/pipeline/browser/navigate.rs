//! Navigation request issuance: plain `Page.navigate` for GET, or a
//! `Fetch.enable` intercept that reissues the document request with a custom
//! method and body for non-GET / body-bearing fetches.

use std::time::Duration;

use crate::sdk::cdp::ws_client::Connection;
use crate::sdk::fetch::deadline::FetchDeadline;
use crate::sdk::fetch::pipeline::request_opts::BodyPayload;
use crate::shared::error::Error;

use super::cdp_send;

/// Navigate to `url` using `method`. For GET (the default) this is a plain
/// `Page.navigate`. For other methods, `Fetch.enable` is used to intercept
/// the first Document request and reissue it with the correct method and body.
pub(super) async fn navigate_with_method(
    conn: &Connection,
    session_id: &str,
    url: &str,
    method: &str,
    body_payload: &BodyPayload,
    timeout: Duration,
    deadline: &FetchDeadline,
) -> Result<serde_json::Value, Error> {
    let is_get = method.eq_ignore_ascii_case("GET");
    let has_body = !matches!(body_payload, BodyPayload::None);

    if is_get && !has_body {
        // Standard GET navigation — no interception needed.
        let mut navigate_params = serde_json::json!({"url": url});
        if let Ok(frame_tree) = cdp_send(
            conn,
            session_id,
            "Page.getFrameTree",
            &serde_json::json!({}),
            "navigate",
            deadline,
        )
        .await
        {
            if let Some(frame_id) = frame_tree
                .pointer("/frameTree/frame/id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                if let Some(obj) = navigate_params.as_object_mut() {
                    obj.insert("frameId".into(), serde_json::json!(frame_id));
                }
            }
        }
        return cdp_send(
            conn,
            session_id,
            "Page.navigate",
            &navigate_params,
            "navigate",
            deadline,
        )
        .await;
    }

    // Non-GET or body present: intercept the navigation request and override
    // its method/body via Fetch.enable before the network layer sends it.
    cdp_send(
        conn,
        session_id,
        "Fetch.enable",
        &serde_json::json!({
            "patterns": [{"resourceType": "Document", "requestStage": "Request"}]
        }),
        "navigate",
        deadline,
    )
    .await?;

    let sid = session_id.to_string();
    let method_str = method.to_string();
    let post_data_b64: Option<String> = match body_payload {
        BodyPayload::None => None,
        BodyPayload::Bytes(b) => {
            use base64::Engine;
            Some(base64::engine::general_purpose::STANDARD.encode(b))
        }
        BodyPayload::Form(fields) => {
            let encoded = fields
                .iter()
                .map(|(k, v)| format!("{}={}", urlencoding_simple(k), urlencoding_simple(v)))
                .collect::<Vec<_>>()
                .join("&");
            use base64::Engine;
            Some(base64::engine::general_purpose::STANDARD.encode(encoded.as_bytes()))
        }
    };

    // Register the intercept listener BEFORE sending Page.navigate so we
    // don't miss the Fetch.requestPaused event.
    let intercept = {
        let sid2 = sid.clone();
        async move {
            let ev = conn
                .wait_event(timeout, |ev| {
                    ev.method == "Fetch.requestPaused"
                        && ev.session_id.as_deref() == Some(&sid2)
                        && ev.params.get("resourceType").and_then(|v| v.as_str())
                            == Some("Document")
                })
                .await?;
            let request_id = ev.params["requestId"].as_str().unwrap_or("").to_string();
            let mut params = serde_json::json!({
                "requestId": request_id,
                "method": method_str,
            });
            if let Some(b64) = post_data_b64 {
                params["postData"] = serde_json::json!(b64);
            }
            if matches!(body_payload, BodyPayload::Form(_)) {
                // Inject Content-Type for form-encoded bodies.
                params["headers"] = serde_json::json!([
                    {"name": "Content-Type", "value": "application/x-www-form-urlencoded"}
                ]);
            }
            cdp_send(
                conn,
                &sid2,
                "Fetch.continueRequest",
                &params,
                "navigate",
                deadline,
            )
            .await?;
            cdp_send(
                conn,
                &sid2,
                "Fetch.disable",
                &serde_json::json!({}),
                "navigate",
                deadline,
            )
            .await
        }
    };

    let nav_params = serde_json::json!({"url": url});
    let navigate = cdp_send(
        conn,
        &sid,
        "Page.navigate",
        &nav_params,
        "navigate",
        deadline,
    );

    let (nav_result, intercept_result) = tokio::join!(navigate, intercept);
    intercept_result?;
    nav_result
}

fn urlencoding_simple(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            other => {
                out.push('%');
                out.push_str(&format!("{other:02X}"));
            }
        }
    }
    out
}
