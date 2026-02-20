use crate::types::Output;
use std::io::Write;
use tokio::sync::mpsc;

/// Stdout writer task. Receives Output values from a channel,
/// serializes to JSONL via AFD output_json (auto-redacts _secret fields),
/// writes to stdout. Single task prevents interleaving.
pub async fn writer_task(mut rx: mpsc::Receiver<Output>) {
    while let Some(output) = rx.recv().await {
        // Output is fully #[derive(Serialize)] with no custom impls — to_value should
        // never fail. If it somehow does, emit a raw error rather than panicking or
        // silently dropping the message.
        let json = match serde_json::to_value(&output) {
            Ok(value) => agent_first_data::output_json(&value),
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
