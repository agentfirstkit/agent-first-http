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

    pub async fn snapshot(&self) -> Vec<RequestRecord> {
        let guard = self.ring.lock().await;
        guard.iter().cloned().collect()
    }
}

/// Spawn a background task that subscribes to network events from `ws_url`
/// and records request/response summaries into `ring`.
pub fn spawn_subscriber(ws_url: String, ring: RecentRequests) {
    tokio::spawn(async move {
        subscribe_loop(ws_url, ring).await;
    });
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
                    "url": ev.params.pointer("/response/url").and_then(|v| v.as_str()).unwrap_or(""),
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
                    "url": ev.params.pointer("/request/url").and_then(|v| v.as_str()).unwrap_or(""),
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
        return Json(body).into_response();
    };
    let requests = ring.snapshot().await;
    let body = serde_json::json!({
        "enabled": true,
        "cap": ring.cap,
        "count": requests.len(),
        "requests": requests,
    });
    Json(body).into_response()
}
