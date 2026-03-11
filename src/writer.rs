use crate::types::Output;
use agent_first_data::{cli_output, OutputFormat, RedactionPolicy};
use serde_json::Value;
use std::io::Write;
use tokio::sync::mpsc;

/// Stdout writer task. Receives Output values from a channel,
/// serializes via Agent-First Data output helpers,
/// writes to stdout. Single task prevents interleaving.
pub async fn writer_task(mut rx: mpsc::Receiver<Output>, format: OutputFormat) {
    while let Some(output) = rx.recv().await {
        // Output is fully #[derive(Serialize)] with no custom impls — to_value should
        // never fail. If it somehow does, emit a raw error rather than panicking or
        // silently dropping the message.
        let rendered = match serde_json::to_value(&output) {
            Ok(mut value) => {
                if matches!(format, OutputFormat::Json) {
                    match redaction_policy_for_output(&output) {
                        Some(policy) => agent_first_data::output_json_with(&value, policy),
                        None => cli_output(&value, OutputFormat::Json),
                    }
                } else {
                    // Server payload fields should remain opaque across human-oriented formats.
                    protect_server_body(&mut value);
                    cli_output(&value, format)
                }
            }
            Err(_) => {
                let fallback = serde_json::json!({
                    "code": "error",
                    "error_code": "internal_error",
                    "error": "output serialization failed",
                    "retryable": false,
                    "trace": {"duration_ms": 0}
                });
                cli_output(&fallback, format)
            }
        };
        // Lock stdout per-write — can't hold across await
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let _ = out.write_all(rendered.as_bytes());
        if !rendered.ends_with('\n') {
            let _ = out.write_all(b"\n");
        }
        let _ = out.flush();
    }
}

fn redaction_policy_for_output(output: &Output) -> Option<RedactionPolicy> {
    match output {
        // Server payloads should remain raw; only trace metadata is redacted.
        Output::Response { .. } => Some(RedactionPolicy::RedactionTraceOnly),
        // Stream chunks are opaque server data.
        Output::ChunkData { .. } => Some(RedactionPolicy::RedactionNone),
        // Config/log/startup/error remain fully redacted by default.
        _ => None,
    }
}

fn protect_server_body(value: &mut Value) {
    if let Some(obj) = value.as_object_mut() {
        for key in &["body", "data"] {
            if let Some(v) = obj.get(*key).cloned() {
                if !v.is_null() && !v.is_string() {
                    if let Ok(json_str) = serde_json::to_string(&v) {
                        obj.insert((*key).to_string(), Value::String(json_str));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writer_task_drains_channel() {
        let (tx, rx) = mpsc::channel(4);
        tx.send(Output::Pong {
            trace: crate::types::PongTrace {
                uptime_s: 1,
                requests_total: 2,
                connections_active: 3,
            },
        })
        .await
        .expect("send");
        drop(tx);
        writer_task(rx, OutputFormat::Json).await;
    }

    #[tokio::test]
    async fn writer_task_yaml_and_plain_formats() {
        for format in [OutputFormat::Yaml, OutputFormat::Plain] {
            let (tx, rx) = mpsc::channel(4);
            tx.send(Output::Pong {
                trace: crate::types::PongTrace {
                    uptime_s: 1,
                    requests_total: 2,
                    connections_active: 3,
                },
            })
            .await
            .expect("send");
            drop(tx);
            writer_task(rx, format).await;
        }
    }
}
