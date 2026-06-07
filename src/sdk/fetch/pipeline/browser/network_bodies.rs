//! Network response-body capture: pull finished response bodies eligible for
//! the active `--network-bodies` mode and write them under `network-bodies/`.

use std::time::Duration;

use crate::sdk::cdp::ws_client::Connection;
use crate::sdk::fetch::artifacts::collectors::NetworkCollector;
use crate::sdk::fetch::artifacts::network_bodies as network_bodies_artifact;
use crate::sdk::fetch::deadline::FetchDeadline;
use crate::sdk::fetch::pipeline::NetworkBodies;
use crate::sdk::fetch::result::Warning;
use crate::shared::artifacts::{Artifact, ArtifactPaths};
use crate::shared::error::ErrorCode;

use super::capture::decode_response_body;

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
