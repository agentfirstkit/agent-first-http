//! Async event subscribers for the browser fetch path.
//!
//! Each collector spawns a tokio task that drains a CDP event stream
//! (`Connection::subscribe`) into a shared in-memory aggregate. The task
//! is aborted on [`NetworkCollector::finish`] / [`ConsoleCollector::finish`];
//! whatever has been observed up to that point is returned.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;

use crate::sdk::cdp::ws_client::Connection;
use crate::sdk::fetch::artifacts::{
    console::{ConsoleEvent, ConsoleLevel, ConsoleLog},
    network::{NetworkEntry, NetworkLog, NetworkSummary, NetworkTiming},
};
use crate::shared::redact;

/// Per-request slot. Held until `loadingFinished` / `loadingFailed` lands.
struct NetworkSlot {
    entry: NetworkEntry,
}

fn default_entry() -> NetworkEntry {
    NetworkEntry {
        request_id: String::new(),
        redirect_from_request_id: None,
        frame_id: None,
        loader_id: None,
        resource_type: "Other".into(),
        url: String::new(),
        method: "GET".into(),
        initiator: None,
        status: None,
        mime_type: None,
        request_headers: BTreeMap::new(),
        response_headers: BTreeMap::new(),
        request_post_data_present: false,
        request_post_data_size_bytes: None,
        body_file: None,
        timing: NetworkTiming {
            start_ms: 0,
            end_ms: None,
        },
        failure: None,
        hints: BTreeMap::new(),
    }
}

/// Drains `Network.*` events into a `NetworkLog`. Optionally tracks
/// matching entries for body capture via [`take_finished`] and collects
/// WebSocket frames / SSE events when `capture_ws` / `capture_sse` are set.
pub struct NetworkCollector {
    inner: Arc<Mutex<NetworkInner>>,
    task: JoinHandle<()>,
    redact_headers: bool,
    /// Notified each time the main-document entry changes (set, status
    /// received, finished, or failed). Lets the pipeline wait for the
    /// real HTTP status without a magic-number sleep.
    main_notify: Arc<Notify>,
}

#[derive(Default)]
struct NetworkInner {
    slots: HashMap<String, NetworkSlot>,
    main_request_id: Option<String>,
    /// Request IDs whose loadingFinished has been seen since the last drain.
    finished: Vec<String>,
    /// WebSocket frames keyed by requestId. Populated when capture_ws=true.
    ws_frames: HashMap<String, Vec<Value>>,
    /// SSE events keyed by requestId. Populated when capture_sse=true.
    sse_events: HashMap<String, Vec<Value>>,
}

impl NetworkCollector {
    pub fn start(
        conn: &Connection,
        redact_headers: bool,
        capture_ws: bool,
        capture_sse: bool,
    ) -> Self {
        let inner = Arc::new(Mutex::new(NetworkInner::default()));
        let main_notify = Arc::new(Notify::new());
        let mut rx = conn.subscribe();
        let inner_w = inner.clone();
        let notify_w = main_notify.clone();
        let task = tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                let mut guard = inner_w.lock().await;
                let touched_main = handle_event(&mut guard, &ev, redact_headers);
                if capture_ws {
                    handle_ws_event(&mut guard, &ev);
                }
                if capture_sse {
                    handle_sse_event(&mut guard, &ev);
                }
                drop(guard);
                if touched_main {
                    notify_w.notify_waiters();
                }
            }
        });
        Self {
            inner,
            task,
            redact_headers,
            main_notify,
        }
    }

    /// Return all collected WebSocket frames, keyed by requestId.
    pub async fn take_ws_frames(&self) -> HashMap<String, Vec<Value>> {
        let mut guard = self.inner.lock().await;
        std::mem::take(&mut guard.ws_frames)
    }

    /// Return all collected SSE events, keyed by requestId.
    pub async fn take_sse_events(&self) -> HashMap<String, Vec<Value>> {
        let mut guard = self.inner.lock().await;
        std::mem::take(&mut guard.sse_events)
    }

    /// Wait until the main-document entry has a status or failure, or
    /// until `timeout` elapses. Returns whatever main_entry is at that
    /// point (possibly None if the navigation never produced a Document
    /// resource — e.g. data: URLs).
    pub async fn wait_for_main_status(&self, timeout: Duration) -> Option<NetworkEntry> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Register the notify future BEFORE checking, so a notification
            // that arrives between check and await is not lost.
            let notified = self.main_notify.notified();
            tokio::pin!(notified);
            if let Some(entry) = self.main_entry().await {
                if entry.status.is_some() || entry.failure.is_some() {
                    return Some(entry);
                }
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return self.main_entry().await;
            }
            if tokio::time::timeout(remaining, notified).await.is_err() {
                return self.main_entry().await;
            }
        }
    }

    /// Return request_ids whose `Network.loadingFinished` event has been
    /// observed since the last call. The internal "finished" buffer is
    /// drained.
    pub async fn take_finished(&self) -> Vec<String> {
        let mut guard = self.inner.lock().await;
        std::mem::take(&mut guard.finished)
    }

    /// Look up the current state of an entry by request_id (e.g. to read
    /// `resource_type` or `mime_type` before issuing
    /// `Network.getResponseBody`).
    pub async fn entry(&self, request_id: &str) -> Option<NetworkEntry> {
        let guard = self.inner.lock().await;
        guard.slots.get(request_id).map(|s| s.entry.clone())
    }

    pub async fn main_request_id(&self) -> Option<String> {
        let guard = self.inner.lock().await;
        guard.main_request_id.clone()
    }

    pub async fn main_entry(&self) -> Option<NetworkEntry> {
        let guard = self.inner.lock().await;
        let request_id = guard.main_request_id.as_ref()?;
        guard.slots.get(request_id).map(|s| s.entry.clone())
    }

    /// Attach a captured body file path to the entry.
    pub async fn set_body_file(&self, request_id: &str, path: std::path::PathBuf) {
        let mut guard = self.inner.lock().await;
        if let Some(s) = guard.slots.get_mut(request_id) {
            s.entry.body_file = Some(path);
        }
    }

    /// Stamp a free-form hint onto an entry.
    pub async fn set_hint(&self, request_id: &str, key: &str, value: Value) {
        let mut guard = self.inner.lock().await;
        if let Some(s) = guard.slots.get_mut(request_id) {
            s.entry.hints.insert(key.to_string(), value);
        }
    }

    /// Drain the aggregate. Entries are returned sorted by `request_id`
    /// for deterministic output. The background task is aborted.
    pub async fn finish(self) -> NetworkLog {
        self.task.abort();
        let mut guard = self.inner.lock().await;
        let mut entries: Vec<NetworkEntry> = guard.slots.drain().map(|(_, s)| s.entry).collect();
        entries.sort_by(|a, b| a.request_id.cmp(&b.request_id));
        let failed_total = entries.iter().filter(|e| e.failure.is_some()).count();
        let captured_body_files = entries.iter().filter(|e| e.body_file.is_some()).count();
        let requests_total = entries.len();
        NetworkLog {
            schema_version: 1,
            main_request_id: guard.main_request_id.clone(),
            entries,
            summary: NetworkSummary {
                requests_total,
                failed_total,
                captured_body_files,
                redacted: self.redact_headers,
            },
        }
    }

    #[allow(dead_code)]
    pub fn redact_headers(&self) -> bool {
        self.redact_headers
    }
}

/// Returns `true` when the event mutated the main-document entry (set
/// the main_request_id, updated its status, finished, or failed it) so
/// the caller can wake `main_notify` waiters.
fn handle_event(
    inner: &mut NetworkInner,
    ev: &crate::sdk::cdp::ws_client::CdpEvent,
    redact_headers: bool,
) -> bool {
    match ev.method.as_str() {
        "Network.requestWillBeSent" => {
            let req_id = string_field(&ev.params, "requestId");
            let frame_id = ev
                .params
                .get("frameId")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let loader_id = ev
                .params
                .get("loaderId")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let resource_type = ev
                .params
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("Other")
                .to_string();
            if resource_type == "Document" && inner.main_request_id.is_none() {
                inner.main_request_id = Some(req_id.clone());
            }
            let timestamp = ev
                .params
                .get("timestamp")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let request = &ev.params["request"];
            let initiator = ev.params.get("initiator").cloned();
            let url = request
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let method = request
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("GET")
                .to_string();
            let mut request_headers = headers_map(request.get("headers"));
            if redact_headers {
                redact_inplace(&mut request_headers);
            }
            // Redirect chain: if redirectResponse is present, finalize the
            // existing entry first as a redirect and link via hint.
            if let Some(redirect) = ev.params.get("redirectResponse") {
                if let Some(existing) = inner.slots.get_mut(&req_id) {
                    let status = redirect
                        .get("status")
                        .and_then(|v| v.as_u64())
                        .map(|s| s as u16);
                    existing.entry.status = status;
                    let mut h = headers_map(redirect.get("headers"));
                    if redact_headers {
                        redact_inplace(&mut h);
                    }
                    existing.entry.response_headers = h;
                    existing
                        .entry
                        .hints
                        .insert("redirected".into(), Value::Bool(true));
                    inner.finished.push(req_id.clone());
                }
            }
            let mut entry = default_entry();
            entry.request_id = req_id.clone();
            if ev.params.get("redirectResponse").is_some() {
                entry.redirect_from_request_id = Some(req_id.clone());
            }
            entry.frame_id = frame_id;
            entry.loader_id = loader_id;
            entry.resource_type = resource_type;
            entry.url = url;
            entry.method = method;
            entry.initiator = initiator;
            entry.request_headers = request_headers;
            entry.timing.start_ms = (timestamp * 1000.0) as u64;
            // Mechanical JSON hint on request body.
            if let Some(post) = request.get("postData").and_then(|v| v.as_str()) {
                entry.request_post_data_present = true;
                entry.request_post_data_size_bytes = Some(post.len());
                if serde_json::from_str::<Value>(post).is_ok() {
                    entry
                        .hints
                        .insert("request_body_json_valid".into(), Value::Bool(true));
                }
                if let Ok(v) = serde_json::from_str::<Value>(post) {
                    if let Some(op) = v.get("operationName").and_then(|v| v.as_str()) {
                        entry.hints.insert(
                            "graphql_operation_name".into(),
                            Value::String(op.to_string()),
                        );
                    }
                    if v.get("query").is_some() {
                        entry.hints.insert(
                            "graphql_operation_type".into(),
                            Value::String("request".into()),
                        );
                    }
                }
            }
            inner.slots.insert(req_id.clone(), NetworkSlot { entry });
            return inner.main_request_id.as_deref() == Some(req_id.as_str());
        }
        "Network.responseReceived" => {
            let req_id = string_field(&ev.params, "requestId");
            let response = &ev.params["response"];
            let status = response
                .get("status")
                .and_then(|v| v.as_u64())
                .map(|s| s as u16);
            let mime_type = response
                .get("mimeType")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let mut headers = headers_map(response.get("headers"));
            if redact_headers {
                redact_inplace(&mut headers);
            }
            let protocol = response
                .get("protocol")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let remote = response
                .get("remoteIPAddress")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            if let Some(s) = inner.slots.get_mut(&req_id) {
                s.entry.status = status;
                if mime_type.is_some() {
                    s.entry.mime_type = mime_type;
                }
                s.entry.response_headers = headers;
                if let Some(p) = protocol {
                    s.entry.hints.insert("protocol".into(), Value::String(p));
                }
                if let Some(r) = remote {
                    s.entry
                        .hints
                        .insert("remote_address".into(), Value::String(r));
                }
            }
            return inner.main_request_id.as_deref() == Some(req_id.as_str());
        }
        "Network.loadingFinished" => {
            let req_id = string_field(&ev.params, "requestId");
            let timestamp = ev
                .params
                .get("timestamp")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let encoded = ev.params.get("encodedDataLength").and_then(|v| v.as_u64());
            if let Some(s) = inner.slots.get_mut(&req_id) {
                s.entry.timing.end_ms = Some((timestamp * 1000.0) as u64);
                if let Some(n) = encoded {
                    s.entry
                        .hints
                        .insert("encoded_data_length".into(), Value::from(n));
                }
            }
            let is_main = inner.main_request_id.as_deref() == Some(req_id.as_str());
            inner.finished.push(req_id);
            return is_main;
        }
        "Network.loadingFailed" => {
            let req_id = string_field(&ev.params, "requestId");
            let err = ev
                .params
                .get("errorText")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(s) = inner.slots.get_mut(&req_id) {
                s.entry.failure = Some(err);
            }
            let is_main = inner.main_request_id.as_deref() == Some(req_id.as_str());
            inner.finished.push(req_id);
            return is_main;
        }
        "Network.requestServedFromCache" => {
            let req_id = string_field(&ev.params, "requestId");
            if let Some(s) = inner.slots.get_mut(&req_id) {
                s.entry
                    .hints
                    .insert("served_from_cache".into(), Value::Bool(true));
            }
        }
        _ => {}
    }
    false
}

fn string_field(params: &Value, key: &str) -> String {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn headers_map(value: Option<&Value>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(Value::Object(map)) = value {
        for (k, v) in map {
            if let Some(s) = v.as_str() {
                out.insert(k.clone(), s.to_string());
            } else {
                out.insert(k.clone(), v.to_string());
            }
        }
    }
    out
}

fn redact_inplace(map: &mut BTreeMap<String, String>) {
    for (name, value) in map.iter_mut() {
        if redact::should_redact(name) {
            *value = redact::REDACTED_VALUE.to_string();
        }
    }
}

// -- Console collector -------------------------------------------------------

pub struct ConsoleCollector {
    inner: Arc<Mutex<Vec<ConsoleEvent>>>,
    task: JoinHandle<()>,
}

impl ConsoleCollector {
    pub fn start(conn: &Connection) -> Self {
        let inner: Arc<Mutex<Vec<ConsoleEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let mut rx = conn.subscribe();
        let inner_w = inner.clone();
        let task = tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                if let Some(console_event) = map_console_event(&ev) {
                    inner_w.lock().await.push(console_event);
                }
            }
        });
        Self { inner, task }
    }

    pub async fn finish(self) -> ConsoleLog {
        self.task.abort();
        let events = std::mem::take(&mut *self.inner.lock().await);
        ConsoleLog {
            schema_version: 1,
            events,
        }
    }
}

fn handle_ws_event(inner: &mut NetworkInner, ev: &crate::sdk::cdp::ws_client::CdpEvent) {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    match ev.method.as_str() {
        "Network.webSocketFrameSent" => {
            let req_id = string_field(&ev.params, "requestId");
            if req_id.is_empty() {
                return;
            }
            let response = ev.params.get("response").cloned().unwrap_or_default();
            let frame = serde_json::json!({
                "type": "sent",
                "opcode": response.get("opcode").and_then(|v| v.as_u64()).unwrap_or(1),
                "mask": response.get("mask").and_then(|v| v.as_bool()).unwrap_or(false),
                "payload": response.get("payloadData").and_then(|v| v.as_str()).unwrap_or(""),
                "timestamp_ms": now_ms,
            });
            inner.ws_frames.entry(req_id).or_default().push(frame);
        }
        "Network.webSocketFrameReceived" => {
            let req_id = string_field(&ev.params, "requestId");
            if req_id.is_empty() {
                return;
            }
            let response = ev.params.get("response").cloned().unwrap_or_default();
            let frame = serde_json::json!({
                "type": "received",
                "opcode": response.get("opcode").and_then(|v| v.as_u64()).unwrap_or(1),
                "mask": response.get("mask").and_then(|v| v.as_bool()).unwrap_or(false),
                "payload": response.get("payloadData").and_then(|v| v.as_str()).unwrap_or(""),
                "timestamp_ms": now_ms,
            });
            inner.ws_frames.entry(req_id).or_default().push(frame);
        }
        "Network.webSocketFrameError" => {
            let req_id = string_field(&ev.params, "requestId");
            if req_id.is_empty() {
                return;
            }
            let error = ev
                .params
                .get("errorMessage")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let frame = serde_json::json!({
                "type": "error",
                "error": error,
                "timestamp_ms": now_ms,
            });
            inner.ws_frames.entry(req_id).or_default().push(frame);
        }
        _ => {}
    }
}

fn handle_sse_event(inner: &mut NetworkInner, ev: &crate::sdk::cdp::ws_client::CdpEvent) {
    if ev.method != "Network.eventSourceMessageReceived" {
        return;
    }
    let req_id = string_field(&ev.params, "requestId");
    if req_id.is_empty() {
        return;
    }
    let event = serde_json::json!({
        "event_name": ev.params.get("eventName").and_then(|v| v.as_str()).unwrap_or(""),
        "data": ev.params.get("data").and_then(|v| v.as_str()).unwrap_or(""),
        "event_id": ev.params.get("eventId").and_then(|v| v.as_str()).unwrap_or(""),
        "timestamp_ms": ev.params.get("timestamp")
            .and_then(|v| v.as_f64())
            .map(|t| (t * 1000.0) as u64)
            .unwrap_or(0),
    });
    inner.sse_events.entry(req_id).or_default().push(event);
}

fn map_console_event(ev: &crate::sdk::cdp::ws_client::CdpEvent) -> Option<ConsoleEvent> {
    match ev.method.as_str() {
        "Runtime.consoleAPICalled" => {
            let level = match ev
                .params
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("log")
            {
                "debug" => ConsoleLevel::Debug,
                "info" => ConsoleLevel::Info,
                "warning" | "warn" => ConsoleLevel::Warn,
                "error" => ConsoleLevel::Error,
                _ => ConsoleLevel::Log,
            };
            let timestamp_ms = ev
                .params
                .get("timestamp")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let text = format_args_array(ev.params.get("args"));
            let (url, line_number) = stack_frame_origin(ev.params.get("stackTrace"));
            Some(ConsoleEvent {
                level,
                timestamp_ms,
                text,
                url,
                line_number,
            })
        }
        "Runtime.exceptionThrown" => {
            let details = ev.params.get("exceptionDetails")?;
            let text = details
                .get("exception")
                .and_then(|e| e.get("description").and_then(|v| v.as_str()))
                .map(str::to_string)
                .or_else(|| {
                    details
                        .get("text")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| "exception".to_string());
            let timestamp_ms = ev
                .params
                .get("timestamp")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let url = details
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let line_number = details
                .get("lineNumber")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32);
            Some(ConsoleEvent {
                level: ConsoleLevel::Exception,
                timestamp_ms,
                text,
                url,
                line_number,
            })
        }
        _ => None,
    }
}

fn format_args_array(args: Option<&Value>) -> String {
    let Some(arr) = args.and_then(|v| v.as_array()) else {
        return String::new();
    };
    let mut parts = Vec::with_capacity(arr.len());
    for a in arr {
        if let Some(s) = a.get("value").and_then(|v| v.as_str()) {
            parts.push(s.to_string());
        } else if let Some(s) = a.get("description").and_then(|v| v.as_str()) {
            parts.push(s.to_string());
        } else if let Some(v) = a.get("value") {
            parts.push(v.to_string());
        }
    }
    parts.join(" ")
}

fn stack_frame_origin(stack: Option<&Value>) -> (Option<String>, Option<u32>) {
    let Some(frames) = stack
        .and_then(|s| s.get("callFrames"))
        .and_then(|v| v.as_array())
    else {
        return (None, None);
    };
    let Some(first) = frames.first() else {
        return (None, None);
    };
    let url = first
        .get("url")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let line = first
        .get("lineNumber")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    (url, line)
}
