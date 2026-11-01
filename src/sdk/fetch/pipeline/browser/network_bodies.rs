//! Network response-body capture: pull finished response bodies eligible for
//! the active `--network-bodies` mode and write them under `network-bodies/`.

use std::time::Duration;

use crate::sdk::cdp::ws_client::Connection;
use crate::sdk::fetch::artifacts::collectors::NetworkCollector;
use crate::sdk::fetch::artifacts::network::NetworkEntry;
use crate::sdk::fetch::artifacts::network_bodies as network_bodies_artifact;
use crate::sdk::fetch::deadline::FetchDeadline;
use crate::sdk::fetch::pipeline::NetworkBodies;
use crate::sdk::fetch::result::Warning;
use crate::shared::artifacts::{Artifact, ArtifactPaths};
use crate::shared::error::ErrorCode;

use super::capture::{CDP_RESPONSE_BODY_MAX_BYTES, decode_response_body};

pub(super) struct NetworkBodyCapture<'a> {
    pub(super) conn: &'a Connection,
    pub(super) session_id: &'a str,
    pub(super) collector: &'a NetworkCollector,
    pub(super) paths: &'a ArtifactPaths,
    pub(super) mode: NetworkBodies,
    pub(super) max_bytes: u64,
    pub(super) deadline: &'a FetchDeadline,
}

pub(super) async fn capture_network_bodies(
    ctx: NetworkBodyCapture<'_>,
    warnings: &mut Vec<Warning>,
) {
    let finished = ctx.collector.take_finished().await;
    for request_id in finished {
        let Some(entry) = ctx.collector.entry(&request_id).await else {
            continue;
        };
        if !network_bodies_eligible(ctx.mode, &entry.resource_type) {
            continue;
        }
        // The CDP call returns a complete JSON body. Skip known oversized
        // responses before requesting/decoding it; unknown or compressed
        // decoded sizes still require the post-decode truncation below. Past
        // the CDP message bound a body is skipped whatever the cap says.
        let skip_above = ctx.max_bytes.min(CDP_RESPONSE_BODY_MAX_BYTES);
        if known_body_exceeds_limit(&entry, skip_above) {
            ctx.collector
                .set_hint(
                    &request_id,
                    "body_skipped_size_limit",
                    serde_json::Value::Bool(true),
                )
                .await;
            warnings.push(Warning {
                artifact: Artifact::Network,
                code: ErrorCode::NetworkBodyTruncated,
                detail: format!(
                    "body for {request_id} skipped before capture: known size exceeds {skip_above} bytes"
                ),
            });
            continue;
        }
        let body_timeout = match ctx
            .deadline
            .bounded_remaining("capture_network_bodies", Duration::from_millis(750))
        {
            Ok(timeout) => timeout,
            Err(e) => {
                warnings.push(Warning {
                    artifact: Artifact::Network,
                    code: e.error_code,
                    detail: e.detail,
                });
                break;
            }
        };
        let resp = match ctx
            .conn
            .send_timeout(
                "Network.getResponseBody",
                &serde_json::json!({"requestId": request_id}),
                Some(ctx.session_id),
                body_timeout,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                warnings.push(Warning {
                    artifact: Artifact::Network,
                    code: if e.error_code == ErrorCode::CdpTimeout {
                        ErrorCode::ArtifactCaptureTimeout
                    } else {
                        ErrorCode::ArtifactCaptureFailed
                    },
                    detail: format!("getResponseBody({request_id}): {}", e.detail),
                });
                continue;
            }
        };
        let bytes = match decode_response_body(&request_id, &resp) {
            Ok(bytes) => bytes,
            Err(e) => {
                warnings.push(Warning {
                    artifact: Artifact::Network,
                    code: e.error_code,
                    detail: e.detail,
                });
                continue;
            }
        };
        let truncated = bytes.len() as u64 > ctx.max_bytes;
        let max_len = usize::try_from(ctx.max_bytes).unwrap_or(usize::MAX);
        let final_bytes: &[u8] = if truncated {
            &bytes[..max_len.min(bytes.len())]
        } else {
            &bytes
        };
        let ext = ext_for_mime(entry.mime_type.as_deref());
        match network_bodies_artifact::write(ctx.paths, &request_id, ext, final_bytes).await {
            Ok(path) => {
                ctx.collector.set_body_file(&request_id, path).await;
                if truncated {
                    warnings.push(Warning {
                        artifact: Artifact::Network,
                        code: ErrorCode::NetworkBodyTruncated,
                        detail: format!(
                            "body for {request_id} truncated to {} bytes",
                            ctx.max_bytes
                        ),
                    });
                }
                if entry
                    .mime_type
                    .as_deref()
                    .is_some_and(|m| m.contains("json"))
                    && serde_json::from_slice::<serde_json::Value>(final_bytes).is_ok()
                {
                    ctx.collector
                        .set_hint(&request_id, "json_valid", serde_json::Value::Bool(true))
                        .await;
                }
            }
            Err(e) => warnings.push(Warning {
                artifact: Artifact::Network,
                code: e.error_code,
                detail: e.detail,
            }),
        }
    }
}

pub(super) fn known_body_exceeds_limit(entry: &NetworkEntry, max_bytes: u64) -> bool {
    let length = |key: &str| {
        entry
            .hints
            .get(key)
            .and_then(|value| value.as_f64())
            .filter(|length| length.is_finite() && *length >= 0.0)
    };
    // CDP's total counts the header block too; only the body is limited.
    let encoded_exceeds = length("encoded_data_length").is_some_and(|total| {
        let body = total - length("encoded_header_length").unwrap_or(0.0);
        body >= u64::MAX as f64 || (body > 0.0 && body.ceil() as u64 > max_bytes)
    });
    encoded_exceeds
        || entry.response_headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("content-length")
                && value
                    .trim()
                    .parse::<u64>()
                    .is_ok_and(|length| length > max_bytes)
        })
}

fn network_bodies_eligible(mode: NetworkBodies, resource_type: &str) -> bool {
    match mode {
        NetworkBodies::Off => false,
        NetworkBodies::All => true,
        NetworkBodies::Xhr => matches!(resource_type, "XHR" | "Fetch" | "EventSource"),
    }
}

fn ext_for_mime(mime: Option<&str>) -> &'static str {
    match mime.unwrap_or("") {
        m if m.starts_with("application/json") => "json",
        m if m.starts_with("text/html") => "html",
        m if m.starts_with("text/css") => "css",
        m if m.starts_with("application/javascript") || m.starts_with("text/javascript") => "js",
        m if m.starts_with("text/plain") => "txt",
        m if m.starts_with("image/png") => "png",
        m if m.starts_with("image/jpeg") => "jpg",
        m if m.starts_with("image/webp") => "webp",
        m if m.starts_with("image/svg") => "svg",
        m if m.starts_with("text/event-stream") => "txt",
        _ => "bin",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio::sync::{Mutex, oneshot};
    use tokio_tungstenite::tungstenite::Message;

    #[tokio::test]
    async fn known_oversized_network_bodies_are_not_requested() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (start_tx, start_rx) = oneshot::channel();
        let requested = Arc::new(Mutex::new(Vec::new()));
        let requested_w = requested.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            start_rx.await.unwrap();
            for (request_id, encoded_length, headers) in [
                ("encoded-large", json!(1000.0), json!({})),
                (
                    "header-large",
                    json!(0),
                    json!({"cOnTeNt-LeNgTh": " 1000 "}),
                ),
                (
                    "unknown-compressed",
                    json!(0),
                    json!({"Content-Encoding": "gzip"}),
                ),
            ] {
                for event in [
                    json!({"method": "Network.requestWillBeSent", "sessionId": "capture", "params": {"requestId": request_id, "type": "XHR", "request": {"url": "https://example.test/data"}}}),
                    json!({"method": "Network.responseReceived", "sessionId": "capture", "params": {"requestId": request_id, "response": {"status": 200, "mimeType": "text/plain", "headers": headers}}}),
                    json!({"method": "Network.loadingFinished", "sessionId": "capture", "params": {"requestId": request_id, "encodedDataLength": encoded_length}}),
                ] {
                    socket
                        .send(Message::Text(event.to_string().into()))
                        .await
                        .unwrap();
                }
            }
            // A main-document status is an ordered completion marker: all
            // preceding response metadata has reached the same collector.
            for event in [
                json!({"method": "Network.requestWillBeSent", "sessionId": "capture", "params": {"requestId": "main", "type": "Document", "request": {"url": "https://example.test/"}}}),
                json!({"method": "Network.responseReceived", "sessionId": "capture", "params": {"requestId": "main", "response": {"status": 200}}}),
            ] {
                socket
                    .send(Message::Text(event.to_string().into()))
                    .await
                    .unwrap();
            }
            while let Some(Ok(Message::Text(text))) = socket.next().await {
                let command: Value = serde_json::from_str(text.as_str()).unwrap();
                if command["method"] == "Network.getResponseBody" {
                    requested_w
                        .lock()
                        .await
                        .push(command["params"]["requestId"].as_str().unwrap().to_string());
                    // The decoded size of this compressed/unknown response
                    // exceeds the cap. It still uses post-decode truncation.
                    socket.send(Message::Text(json!({"id": command["id"], "result": {"body": "YWJjZGVmZ2hpamts", "base64Encoded": true}}).to_string().into())).await.unwrap();
                }
            }
        });
        let conn = Connection::connect(&format!("ws://{address}/cdp"), None)
            .await
            .unwrap();
        let collector = NetworkCollector::start(&conn, "capture", true, false, false);
        start_tx.send(()).unwrap();
        assert_eq!(
            collector
                .wait_for_main_status(Duration::from_secs(1), None)
                .await
                .unwrap()
                .status,
            Some(200)
        );
        let temp = tempfile::tempdir().unwrap();
        let paths = ArtifactPaths {
            root: temp.path().to_path_buf(),
        };
        let deadline = FetchDeadline::new(
            Duration::from_secs(2),
            crate::sdk::fetch::result::TraceRenderMode::Always,
        );
        let mut warnings = Vec::new();
        capture_network_bodies(
            NetworkBodyCapture {
                conn: &conn,
                session_id: "capture",
                collector: &collector,
                paths: &paths,
                mode: NetworkBodies::Xhr,
                max_bytes: 8,
                deadline: &deadline,
            },
            &mut warnings,
        )
        .await;
        assert_eq!(*requested.lock().await, ["unknown-compressed"]);
        assert_eq!(warnings.len(), 3);
        assert!(
            warnings
                .iter()
                .all(|warning| warning.code == ErrorCode::NetworkBodyTruncated)
        );
        for request_id in ["encoded-large", "header-large"] {
            let entry = collector.entry(request_id).await.unwrap();
            assert!(entry.body_file.is_none());
            assert_eq!(entry.hints["body_skipped_size_limit"], true);
            assert!(
                warnings
                    .iter()
                    .any(|warning| warning.detail.contains(request_id)
                        && warning.detail.contains("skipped before capture"))
            );
        }
        let captured = collector.entry("unknown-compressed").await.unwrap();
        assert_eq!(
            std::fs::read(captured.body_file.unwrap()).unwrap(),
            b"abcdefgh"
        );
        let log = collector.finish().await;
        // Body omission/truncation cannot retroactively change readiness.
        assert!(!log.summary.truncated);
        assert_eq!(log.summary.events_dropped, 0);
        conn.close();
        server.abort();
    }

    #[test]
    fn known_body_sizes_are_compared_without_rounding_down() {
        let mut entry: NetworkEntry = serde_json::from_value(json!({
            "request_id": "response", "state": "finished", "resource_type": "XHR",
            "request_url": "https://example.test/", "method": "GET",
            "request_headers": {}, "response_headers": {},
            "timing": {"start_monotonic_ms": 0}, "hints": {},
        }))
        .unwrap();
        for (size, max_bytes, exceeds) in [
            (json!(1000.0), 1000, false),
            (json!(1000.1), 1000, true),
            (json!(-1.0), 0, false),
            (json!(1e100), u64::MAX, true),
            (json!(u64::MAX), u64::MAX - 1, true),
        ] {
            entry.hints.insert("encoded_data_length".into(), size);
            assert_eq!(known_body_exceeds_limit(&entry, max_bytes), exceeds);
        }
        // A body just under the limit is not refused for its header block.
        entry
            .hints
            .insert("encoded_data_length".into(), json!(1300));
        entry
            .hints
            .insert("encoded_header_length".into(), json!(400));
        assert!(!known_body_exceeds_limit(&entry, 1000));
        assert!(known_body_exceeds_limit(&entry, 899));
        entry.hints.clear();
        entry
            .response_headers
            .insert("Content-Length".into(), "invalid".into());
        assert!(!known_body_exceeds_limit(&entry, 0));
        entry
            .response_headers
            .insert("Content-Length".into(), "8".into());
        assert!(!known_body_exceeds_limit(&entry, 8));
        assert!(known_body_exceeds_limit(&entry, 7));
    }
}
