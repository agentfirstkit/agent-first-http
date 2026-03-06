use crate::types::Output;
use agent_first_data::RedactionPolicy;
use std::io::Write;
use tokio::sync::mpsc;

/// Stdout writer task. Receives Output values from a channel,
/// serializes to JSONL via Agent-First Data output_json_with (policy per output type),
/// writes to stdout. Single task prevents interleaving.
pub async fn writer_task(mut rx: mpsc::Receiver<Output>) {
    while let Some(output) = rx.recv().await {
        let redaction_policy = redaction_policy_for_output(&output);
        // Output is fully #[derive(Serialize)] with no custom impls — to_value should
        // never fail. If it somehow does, emit a raw error rather than panicking or
        // silently dropping the message.
        let json = match serde_json::to_value(&output) {
            Ok(value) => match redaction_policy {
                Some(policy) => agent_first_data::output_json_with(&value, policy),
                None => agent_first_data::output_json(&value),
            },
            Err(_) => {
                r#"{"code":"error","error_code":"internal_error","error":"output serialization failed","retryable":false,"trace":{"duration_ms":0}}"#.to_string()
            }
        };
        // Lock stdout per-write — can't hold across await
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let _ = out.write_all(json.as_bytes());
        let _ = out.write_all(b"\n");
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
        writer_task(rx).await;
    }
}
