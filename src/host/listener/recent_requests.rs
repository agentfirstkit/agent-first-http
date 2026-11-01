//! `/recent-requests` endpoint — bounded in-memory ring of recent network
//! requests observed by the browser. Default off; enable with
//! `--recent-requests-cap N` on the host.

use std::collections::VecDeque;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::host::listener::AppState;

pub(super) struct SubscriberGuard {
    task: std::sync::Mutex<tokio::task::JoinHandle<()>>,
}

impl SubscriberGuard {
    /// Follow a different browser: the one the host now serves after a
    /// profile switch. The old subscription ends, so nothing it saw is mixed
    /// with what the new profile does.
    pub(super) fn follow(&self, ws_url: String, ring: RecentRequests) {
        let next = spawn_task(ws_url, ring);
        let mut task = self.task.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::replace(&mut *task, next).abort();
    }
}

impl Drop for SubscriberGuard {
    fn drop(&mut self) {
        self.task
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .abort();
    }
}

/// A single captured request/response pair.
pub type RequestRecord = serde_json::Value;

/// Shared ring buffer. `None` = feature disabled.
#[derive(Clone, Default)]
pub struct RecentRequests {
    pub cap: usize,
    pub ring: Arc<Mutex<VecDeque<RequestRecord>>>,
}

impl RecentRequests {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            ring: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub async fn push(&self, record: RequestRecord) {
        if self.cap == 0 {
            return;
        }
        let mut guard = self.ring.lock().await;
        if guard.len() >= self.cap {
            guard.pop_front();
        }
        guard.push_back(record);
    }

    /// Forget everything recorded so far — what one profile's browser fetched
    /// is not the next profile's to report.
    pub async fn clear(&self) {
        self.ring.lock().await.clear();
    }

    pub async fn snapshot(&self) -> Vec<RequestRecord> {
        let guard = self.ring.lock().await;
        guard.iter().cloned().collect()
    }
}

/// Spawn a background task that subscribes to network events from `ws_url`
/// and records request/response summaries into `ring`.
/// The task is detached; it ends when the upstream event stream closes.
pub fn spawn_subscriber(ws_url: String, ring: RecentRequests) {
    drop(spawn_task(ws_url, ring));
}

pub(super) fn spawn_scoped_subscriber(ws_url: String, ring: RecentRequests) -> SubscriberGuard {
    SubscriberGuard {
        task: std::sync::Mutex::new(spawn_task(ws_url, ring)),
    }
}

fn spawn_task(ws_url: String, ring: RecentRequests) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        subscribe_loop(ws_url, ring).await;
    })
}

async fn subscribe_loop(ws_url: String, ring: RecentRequests) {
    let conn = match crate::sdk::cdp::ws_client::Connection::connect(&ws_url, None).await {
        Ok(c) => c,
        Err(_) => return,
    };
    // Enable Network domain at the browser level (no session = global).
    let _ = conn
        .send("Network.enable", &serde_json::json!({}), None)
        .await;
    let mut rx = conn.subscribe();
    while let Ok(ev) = rx.recv().await {
        match ev.method.as_str() {
            "Network.responseReceived" => {
                let record = serde_json::json!({
                    "type": "response",
                    "request_id": ev.params.get("requestId").and_then(|v| v.as_str()).unwrap_or(""),
                    "response_url": crate::shared::redact::redact_url(
                        ev.params.pointer("/response/url").and_then(|v| v.as_str()).unwrap_or("")
                    ),
                    "status": ev.params.pointer("/response/status").and_then(|v| v.as_u64()).unwrap_or(0),
                    "mime_type": ev.params.pointer("/response/mimeType").and_then(|v| v.as_str()).unwrap_or(""),
                    "resource_type": ev.params.get("type").and_then(|v| v.as_str()).unwrap_or(""),
                });
                ring.push(record).await;
            }
            "Network.requestWillBeSent" => {
                let record = serde_json::json!({
                    "type": "request",
                    "request_id": ev.params.get("requestId").and_then(|v| v.as_str()).unwrap_or(""),
                    "request_url": crate::shared::redact::redact_url(
                        ev.params.pointer("/request/url").and_then(|v| v.as_str()).unwrap_or("")
                    ),
                    "method": ev.params.pointer("/request/method").and_then(|v| v.as_str()).unwrap_or("GET"),
                    "resource_type": ev.params.get("type").and_then(|v| v.as_str()).unwrap_or(""),
                });
                ring.push(record).await;
            }
            _ => {}
        }
    }
}

#[derive(Deserialize, Default)]
pub struct RecentQuery {
    pub profile: Option<String>,
}

pub async fn handler(Query(q): Query<RecentQuery>, State(state): State<AppState>) -> Response {
    let _ = q.profile; // future: route per-profile
    let Some(ring) = &state.recent_requests else {
        let body = serde_json::json!({
            "enabled": false,
            "reason": "start the host with --recent-requests-cap N to enable",
            "requests": [],
        });
        return Json(crate::shared::afdata::result_value("recent_requests", body)).into_response();
    };
    let requests = ring.snapshot().await;
    let body = serde_json::json!({
        "enabled": true,
        "cap": ring.cap,
        "count": requests.len(),
        "requests": requests,
    });
    Json(crate::shared::afdata::result_value("recent_requests", body)).into_response()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use futures::{SinkExt, StreamExt};
    use tokio::io::AsyncReadExt;
    use tokio_tungstenite::tungstenite::Message;

    async fn subscriber_fixture() -> (
        String,
        tokio::sync::oneshot::Receiver<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind subscriber fixture");
        let address = listener.local_addr().expect("subscriber fixture address");
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept subscriber");
            let mut ws = tokio_tungstenite::accept_async(stream)
                .await
                .expect("accept websocket");
            let command = ws
                .next()
                .await
                .expect("command")
                .expect("websocket command");
            let command: serde_json::Value =
                serde_json::from_str(command.to_text().expect("text command"))
                    .expect("CDP command");
            ws.send(Message::Text(
                serde_json::json!({"id": command["id"], "result": {}})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("Network.enable response");
            started_tx.send(()).expect("subscriber started");
            let mut bytes = [0; 1024];
            loop {
                match ws.get_mut().read(&mut bytes).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
        (format!("ws://{address}/cdp"), started_rx, peer)
    }

    #[tokio::test]
    async fn last_host_owner_stops_the_subscriber_and_releases_its_transport_and_ring() {
        let (ws_url, started, peer) = subscriber_fixture().await;
        let ring = RecentRequests::new(8);
        let weak_ring = Arc::downgrade(&ring.ring);
        let subscriber = Arc::new(spawn_scoped_subscriber(ws_url, ring.clone()));
        let mut state =
            crate::host::listener::test_state(None, crate::host::bootstrap::HealthPublic::Off);
        state.recent_requests = Some(ring);
        state._recent_requests_subscriber = Some(subscriber);
        let owner = state.clone();
        started.await.expect("subscriber is connected");
        drop(state);
        assert!(
            weak_ring.upgrade().is_some(),
            "the remaining host clone owns the subscriber"
        );
        drop(owner);
        tokio::time::timeout(std::time::Duration::from_secs(1), peer)
            .await
            .expect("last host drop closes the subscriber socket")
            .expect("peer completed");
        assert!(
            weak_ring.upgrade().is_none(),
            "subscriber retains no ring after host teardown"
        );
    }
}
