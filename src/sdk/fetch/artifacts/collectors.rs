//! Async event subscribers for the browser fetch path.
//!
//! Each collector spawns a tokio task that drains a CDP event stream
//! (`Connection::subscribe`) into a shared in-memory aggregate. A connection
//! carries every session's events, so a collector keeps only those of the
//! fetch's own session. The task is aborted on finish or drop;
//! whatever has been observed up to that point is returned.

use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::{Mutex, Notify, broadcast};
use tokio::task::JoinHandle;

use crate::sdk::cdp::ws_client::{CdpEvent, Connection};
use crate::sdk::fetch::artifacts::{
    console::{ConsoleEvent, ConsoleLevel, ConsoleLog},
    network::{NetworkEntry, NetworkEntryState, NetworkLog, NetworkSummary, NetworkTiming},
};
use crate::shared::redact;

const NETWORK_ENTRY_LIMIT: usize = 4096;
const CONSOLE_EVENT_LIMIT: usize = 4096;
const STREAM_EVENT_LIMIT_PER_REQUEST: usize = 1024;
/// Payload bytes kept per WebSocket/SSE request. The event that crosses the
/// budget keeps a prefix and is marked `payload_truncated`; later events are
/// dropped and the network summary reports `truncated`.
const STREAM_PAYLOAD_BYTES_PER_REQUEST: usize = 1024 * 1024;

#[derive(Default)]
struct StreamEvents {
    events: Vec<Value>,
    /// Retained after a drain so repeated drains cannot reset the limit.
    captured_total: usize,
    /// Payload bytes kept so far; survives drains like `captured_total`.
    captured_bytes: usize,
}

/// Per-request slot. Held until `loadingFinished` / `loadingFailed` lands.
struct NetworkSlot {
    entry: NetworkEntry,
    started_at: Instant,
    updated_at: Instant,
}

pub(crate) struct ObservedRequest {
    pub(crate) entry: NetworkEntry,
    pub(crate) waited: Duration,
    pub(crate) idle_for: Duration,
}

pub(crate) struct ReadinessNetwork {
    pub(crate) requests: Vec<ObservedRequest>,
    pub(crate) complete: bool,
}

fn default_entry() -> NetworkEntry {
    NetworkEntry {
        request_id: String::new(),
        state: NetworkEntryState::Pending,
        redirect_from_request_id: None,
        frame_id: None,
        loader_id: None,
        resource_type: "Other".into(),
        request_url: String::new(),
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
            start_monotonic_ms: 0,
            end_monotonic_ms: None,
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
    finished: BTreeSet<String>,
    /// WebSocket frames keyed by requestId. Populated when capture_ws=true.
    ws_frames: HashMap<String, StreamEvents>,
    /// SSE events keyed by requestId. Populated when capture_sse=true.
    sse_events: HashMap<String, StreamEvents>,
    /// Redirect hops already moved to their own entry, per CDP request id.
    redirect_hops: HashMap<String, usize>,
    /// Last time any network event changed the aggregate. Used for the
    /// default readiness wait instead of Chrome's lifecycle-only networkIdle.
    last_activity: Option<Instant>,
    events_dropped: u64,
    truncated: bool,
}

#[derive(Debug, Clone)]
pub struct NetworkQuietSnapshot {
    pub quiet: bool,
    pub idle_for: Duration,
    pub inflight_total: usize,
    pub pending_by_resource_type: BTreeMap<String, usize>,
}

impl NetworkCollector {
    pub fn start(
        conn: &Connection,
        session_id: &str,
        redact_headers: bool,
        capture_ws: bool,
        capture_sse: bool,
    ) -> Self {
        Self::from_receiver(
            conn.subscribe(),
            session_id.to_string(),
            redact_headers,
            capture_ws,
            capture_sse,
        )
    }

    fn from_receiver(
        mut rx: broadcast::Receiver<CdpEvent>,
        session_id: String,
        redact_headers: bool,
        capture_ws: bool,
        capture_sse: bool,
    ) -> Self {
        let inner = Arc::new(Mutex::new(NetworkInner::default()));
        let main_notify = Arc::new(Notify::new());
        let inner_w = inner.clone();
        let notify_w = main_notify.clone();
        let task = tokio::spawn(async move {
            loop {
                let ev = match rx.recv().await {
                    Ok(ev) => ev,
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        let mut guard = inner_w.lock().await;
                        guard.events_dropped = guard.events_dropped.saturating_add(count);
                        guard.last_activity = Some(Instant::now());
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                if ev.session_id.as_deref() != Some(session_id.as_str()) {
                    continue;
                }
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
        take_stream_events(&mut guard.ws_frames)
    }

    /// Return all collected SSE events, keyed by requestId.
    pub async fn take_sse_events(&self) -> HashMap<String, Vec<Value>> {
        let mut guard = self.inner.lock().await;
        take_stream_events(&mut guard.sse_events)
    }

    /// Wait until the main-document entry has a status or failure, or
    /// until `timeout` elapses. Returns whatever main_entry is at that
    /// point (possibly None if the navigation never produced a Document
    /// resource — e.g. data: URLs).
    ///
    /// With `main_frame_id`, the main entry is re-resolved on every check
    /// the way [`Self::readiness_snapshot`] does it — the latest root-frame
    /// Document — so a script redirect does not pair the first page's
    /// status and body with the second page's URL. Without it (the root
    /// frame could not be read) the first Document seen stays the main one.
    pub async fn wait_for_main_status(
        &self,
        timeout: Duration,
        main_frame_id: Option<&str>,
    ) -> Option<NetworkEntry> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Register the notify future BEFORE checking, so a notification
            // that arrives between check and await is not lost.
            let notified = self.main_notify.notified();
            tokio::pin!(notified);
            if let Some(frame_id) = main_frame_id {
                select_main_document(&mut *self.inner.lock().await, frame_id);
            }
            if let Some(entry) = self.main_entry().await
                && (entry.status.is_some() || entry.failure.is_some())
            {
                return Some(entry);
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
        std::mem::take(&mut guard.finished).into_iter().collect()
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

    pub(crate) async fn readiness_snapshot(&self, main_frame_id: &str) -> ReadinessNetwork {
        let mut guard = self.inner.lock().await;
        select_main_document(&mut guard, main_frame_id);
        ReadinessNetwork {
            requests: guard
                .slots
                .values()
                .map(|slot| ObservedRequest {
                    entry: slot.entry.clone(),
                    waited: slot.started_at.elapsed(),
                    idle_for: slot.updated_at.elapsed(),
                })
                .collect(),
            complete: guard.events_dropped == 0 && !guard.truncated,
        }
    }

    /// Mechanical network quiet signal for readiness: no pending/responded
    /// requests and no Network.* activity for at least `idle`. Event loss and
    /// capture truncation do not block the signal: every Network.* event still
    /// stamps `last_activity` whether or not its request was retained, so a
    /// request hidden by loss keeps the page busy for as long as it produces
    /// events, and the diagnostics report the loss separately.
    pub async fn quiet_snapshot(&self, idle: Duration) -> NetworkQuietSnapshot {
        let guard = self.inner.lock().await;
        let pending_by_resource_type =
            pending_by_resource_type(guard.slots.values().map(|slot| &slot.entry));
        let inflight_total: usize = pending_by_resource_type.values().copied().sum();
        let idle_for = guard
            .last_activity
            .map(|last| last.elapsed())
            .unwrap_or_else(|| Duration::from_secs(u64::MAX / 2));
        NetworkQuietSnapshot {
            quiet: inflight_total == 0 && idle_for >= idle,
            idle_for,
            inflight_total,
            pending_by_resource_type,
        }
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
        let mut summary = summarize_entries(&entries, self.redact_headers);
        summary.events_dropped = guard.events_dropped;
        summary.truncated = guard.truncated;
        NetworkLog {
            schema_version: super::network::NETWORK_SCHEMA_VERSION,
            main_request_id: guard.main_request_id.clone(),
            entries,
            summary,
        }
    }

    #[allow(dead_code)]
    pub fn redact_headers(&self) -> bool {
        self.redact_headers
    }
}

impl Drop for NetworkCollector {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A subframe Document is never the main document, even if it was observed
/// first, and neither is a redirect hop that already ended. The latest
/// root-frame navigation owns main status. Leaves the current choice alone
/// when no root-frame Document has been seen.
fn select_main_document(inner: &mut NetworkInner, main_frame_id: &str) {
    if let Some(main) = inner
        .slots
        .values()
        .filter(|slot| {
            slot.entry.resource_type == "Document"
                && slot.entry.frame_id.as_deref() == Some(main_frame_id)
                && !is_redirect_hop(&slot.entry)
        })
        .max_by_key(|slot| slot.entry.timing.start_monotonic_ms)
    {
        inner.main_request_id = Some(main.entry.request_id.clone());
    }
}

/// True for the entry of a redirect hop that ended with the redirect.
pub(crate) fn is_redirect_hop(entry: &NetworkEntry) -> bool {
    entry.hints.get("redirected") == Some(&Value::Bool(true))
}

/// Returns `true` when the event mutated the main-document entry (set
/// the main_request_id, updated its status, finished, or failed it) so
/// the caller can wake `main_notify` waiters.
fn handle_event(
    inner: &mut NetworkInner,
    ev: &crate::sdk::cdp::ws_client::CdpEvent,
    redact_headers: bool,
) -> bool {
    if let Some(request_id) = ev.params.get("requestId").and_then(Value::as_str)
        && let Some(slot) = inner.slots.get_mut(request_id)
        && ev.method.starts_with("Network.")
    {
        slot.updated_at = Instant::now();
    }
    // Preserve the metadata activity signal. Events rejected by the request
    // limit still count; uncaptured streams and unrelated CDP events do not.
    if matches!(
        ev.method.as_str(),
        "Network.requestWillBeSent"
            | "Network.responseReceived"
            | "Network.loadingFinished"
            | "Network.loadingFailed"
            | "Network.requestServedFromCache"
    ) {
        inner.last_activity = Some(Instant::now());
    }
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
            // Reserve one slot for the main document if subresources arrive
            // first. Existing requests keep receiving their state updates.
            let limit = if inner.main_request_id.is_none() && resource_type != "Document" {
                NETWORK_ENTRY_LIMIT - 1
            } else {
                NETWORK_ENTRY_LIMIT
            };
            if req_id.is_empty()
                || (!inner.slots.contains_key(&req_id) && inner.slots.len() >= limit)
            {
                inner.truncated = true;
                return false;
            }
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
            let request_url = request
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_default();
            let method = request
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("GET")
                .to_string();
            let mut request_headers = headers_map(request.get("headers"));
            if redact_headers {
                redact_inplace(&mut request_headers);
            }
            // Redirect chain: CDP reuses one requestId for every hop. The
            // hop that just ended moves to its own entry, keyed
            // `{requestId}#redirect{n}`, and the new hop links back to it.
            let mut redirect_from = None;
            if let Some(redirect) = ev.params.get("redirectResponse")
                && let Some(mut hop) = inner.slots.remove(&req_id)
            {
                // The new hop takes the requestId's slot back; the ended hop
                // needs one more, if the limit leaves room.
                if inner.slots.len() + 2 > NETWORK_ENTRY_LIMIT {
                    inner.truncated = true;
                } else {
                    let count = inner.redirect_hops.entry(req_id.clone()).or_insert(0);
                    let hop_id = format!("{req_id}#redirect{count}");
                    *count += 1;
                    hop.entry.request_id = hop_id.clone();
                    hop.entry.status = redirect
                        .get("status")
                        .and_then(|v| v.as_u64())
                        .map(|s| s as u16);
                    hop.entry.state = NetworkEntryState::Finished;
                    hop.entry.timing.end_monotonic_ms = Some((timestamp * 1000.0) as u64);
                    let mut h = headers_map(redirect.get("headers"));
                    if redact_headers {
                        redact_inplace(&mut h);
                    }
                    hop.entry.response_headers = h;
                    hop.entry
                        .hints
                        .insert("redirected".into(), Value::Bool(true));
                    inner.slots.insert(hop_id.clone(), hop);
                    redirect_from = Some(hop_id);
                }
            }
            let mut entry = default_entry();
            entry.request_id = req_id.clone();
            entry.redirect_from_request_id = redirect_from;
            entry.frame_id = frame_id;
            entry.loader_id = loader_id;
            entry.resource_type = resource_type;
            entry.request_url = request_url;
            entry.method = method;
            entry.initiator = initiator;
            entry.request_headers = request_headers;
            entry.timing.start_monotonic_ms = (timestamp * 1000.0) as u64;
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
            let now = Instant::now();
            inner.slots.insert(
                req_id.clone(),
                NetworkSlot {
                    entry,
                    started_at: now,
                    updated_at: now,
                },
            );
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
            // What has arrived by the time the response is reported: the
            // header block. loadingFinished's total includes it, so the body
            // size is the difference.
            let header_length = response
                .get("encodedDataLength")
                .filter(|v| v.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0))
                .cloned();
            if let Some(s) = inner.slots.get_mut(&req_id) {
                s.entry.status = status;
                s.entry.state = NetworkEntryState::Responded;
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
                if let Some(n) = header_length {
                    s.entry.hints.insert("encoded_header_length".into(), n);
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
            let encoded = ev
                .params
                .get("encodedDataLength")
                .filter(|v| v.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0));
            if let Some(s) = inner.slots.get_mut(&req_id) {
                s.entry.timing.end_monotonic_ms = Some((timestamp * 1000.0) as u64);
                s.entry.state = NetworkEntryState::Finished;
                if let Some(n) = encoded {
                    s.entry
                        .hints
                        .insert("encoded_data_length".into(), n.clone());
                }
                inner.finished.insert(req_id.clone());
            }
            let is_main = inner.main_request_id.as_deref() == Some(req_id.as_str());
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
                s.entry.state = NetworkEntryState::Failed;
                inner.finished.insert(req_id.clone());
            }
            let is_main = inner.main_request_id.as_deref() == Some(req_id.as_str());
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

fn summarize_entries(entries: &[NetworkEntry], redacted: bool) -> NetworkSummary {
    let requests_total = entries.len();
    let responses_total = entries.iter().filter(|e| e.status.is_some()).count();
    let finished_total = entries
        .iter()
        .filter(|e| e.state == NetworkEntryState::Finished)
        .count();
    let failed_total = entries
        .iter()
        .filter(|e| e.state == NetworkEntryState::Failed || e.failure.is_some())
        .count();
    let pending_by_resource_type = pending_by_resource_type(entries.iter());
    let inflight_total_at_capture = pending_by_resource_type.values().copied().sum();
    let captured_body_files = entries.iter().filter(|e| e.body_file.is_some()).count();
    NetworkSummary {
        requests_total,
        responses_total,
        finished_total,
        failed_total,
        incomplete_total: inflight_total_at_capture,
        inflight_total_at_capture,
        pending_by_resource_type,
        captured_body_files,
        redacted,
        events_dropped: 0,
        truncated: false,
    }
}

fn pending_by_resource_type<'a, I>(entries: I) -> BTreeMap<String, usize>
where
    I: IntoIterator<Item = &'a NetworkEntry>,
{
    let mut out = BTreeMap::new();
    for entry in entries {
        if matches!(
            entry.state,
            NetworkEntryState::Pending | NetworkEntryState::Responded
        ) {
            *out.entry(entry.resource_type.clone()).or_insert(0) += 1;
        }
    }
    out
}

// -- Console collector -------------------------------------------------------

pub struct ConsoleCollector {
    inner: Arc<Mutex<ConsoleInner>>,
    task: JoinHandle<()>,
}

#[derive(Default)]
struct ConsoleInner {
    events: Vec<ConsoleEvent>,
    events_dropped: u64,
    truncated: bool,
}

impl ConsoleCollector {
    pub fn start(conn: &Connection, session_id: &str) -> Self {
        Self::from_receiver(conn.subscribe(), session_id.to_string())
    }

    fn from_receiver(mut rx: broadcast::Receiver<CdpEvent>, session_id: String) -> Self {
        let inner = Arc::new(Mutex::new(ConsoleInner::default()));
        let inner_w = inner.clone();
        let task = tokio::spawn(async move {
            loop {
                let ev = match rx.recv().await {
                    Ok(ev) => ev,
                    Err(broadcast::error::RecvError::Lagged(count)) => {
                        let mut guard = inner_w.lock().await;
                        guard.events_dropped = guard.events_dropped.saturating_add(count);
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                if !matches!(
                    ev.method.as_str(),
                    "Runtime.consoleAPICalled" | "Runtime.exceptionThrown"
                ) || ev.session_id.as_deref() != Some(session_id.as_str())
                {
                    continue;
                }
                let mut guard = inner_w.lock().await;
                if guard.events.len() >= CONSOLE_EVENT_LIMIT {
                    guard.truncated = true;
                    continue;
                }
                if let Some(console_event) = map_console_event(&ev) {
                    guard.events.push(console_event);
                }
            }
        });
        Self { inner, task }
    }

    pub async fn finish(self) -> ConsoleLog {
        self.task.abort();
        let mut guard = self.inner.lock().await;
        let events = std::mem::take(&mut guard.events);
        ConsoleLog {
            schema_version: super::console::CONSOLE_SCHEMA_VERSION,
            events,
            events_dropped: guard.events_dropped,
            truncated: guard.truncated,
        }
    }
}

impl Drop for ConsoleCollector {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn take_stream_events(map: &mut HashMap<String, StreamEvents>) -> HashMap<String, Vec<Value>> {
    map.iter_mut()
        .filter_map(|(request_id, bucket)| {
            if bucket.events.is_empty() {
                None
            } else {
                Some((request_id.clone(), std::mem::take(&mut bucket.events)))
            }
        })
        .collect()
}

/// Keep `event` under the per-request count and byte limits. `payload_key`
/// names the event's payload string; the event that crosses the byte budget
/// keeps a prefix and gains `payload_truncated: true` and the original
/// `payload_bytes`. Returns false when anything was dropped or cut.
fn push_stream_event(
    map: &mut HashMap<String, StreamEvents>,
    request_id: String,
    mut event: Value,
    payload_key: &str,
) -> bool {
    if !map.contains_key(&request_id) && map.len() >= NETWORK_ENTRY_LIMIT {
        return false;
    }
    let bucket = map.entry(request_id).or_default();
    if bucket.captured_total >= STREAM_EVENT_LIMIT_PER_REQUEST {
        return false;
    }
    let budget = STREAM_PAYLOAD_BYTES_PER_REQUEST - bucket.captured_bytes;
    let payload_len = event
        .get(payload_key)
        .and_then(Value::as_str)
        .map_or(0, str::len);
    let kept = if payload_len > budget {
        if budget == 0 {
            return false;
        }
        let payload = event[payload_key].as_str().unwrap_or("");
        let prefix = payload[..payload.floor_char_boundary(budget)].to_string();
        event[payload_key] = Value::String(prefix);
        event["payload_truncated"] = Value::Bool(true);
        event["payload_bytes"] = Value::from(payload_len);
        // The budget is spent: a later event would be cut to nothing.
        budget
    } else {
        payload_len
    };
    bucket.captured_total += 1;
    bucket.captured_bytes += kept;
    bucket.events.push(event);
    payload_len <= budget
}

fn handle_ws_event(inner: &mut NetworkInner, ev: &crate::sdk::cdp::ws_client::CdpEvent) {
    if !matches!(
        ev.method.as_str(),
        "Network.webSocketFrameSent"
            | "Network.webSocketFrameReceived"
            | "Network.webSocketFrameError"
    ) {
        return;
    }
    // Called only when WS capture is enabled. Activity precedes cap rejection.
    inner.last_activity = Some(Instant::now());
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
            let response = &ev.params["response"];
            let frame = serde_json::json!({
                "type": "sent",
                "opcode": response.get("opcode").and_then(|v| v.as_u64()).unwrap_or(1),
                "mask": response.get("mask").and_then(|v| v.as_bool()).unwrap_or(false),
                "payload": response.get("payloadData").and_then(|v| v.as_str()).unwrap_or(""),
                "timestamp_epoch_ms": now_ms,
            });
            if !push_stream_event(&mut inner.ws_frames, req_id, frame, "payload") {
                inner.truncated = true;
            }
        }
        "Network.webSocketFrameReceived" => {
            let req_id = string_field(&ev.params, "requestId");
            if req_id.is_empty() {
                return;
            }
            let response = &ev.params["response"];
            let frame = serde_json::json!({
                "type": "received",
                "opcode": response.get("opcode").and_then(|v| v.as_u64()).unwrap_or(1),
                "mask": response.get("mask").and_then(|v| v.as_bool()).unwrap_or(false),
                "payload": response.get("payloadData").and_then(|v| v.as_str()).unwrap_or(""),
                "timestamp_epoch_ms": now_ms,
            });
            if !push_stream_event(&mut inner.ws_frames, req_id, frame, "payload") {
                inner.truncated = true;
            }
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
                "timestamp_epoch_ms": now_ms,
            });
            if !push_stream_event(&mut inner.ws_frames, req_id, frame, "payload") {
                inner.truncated = true;
            }
        }
        _ => {}
    }
}

fn handle_sse_event(inner: &mut NetworkInner, ev: &crate::sdk::cdp::ws_client::CdpEvent) {
    if ev.method != "Network.eventSourceMessageReceived" {
        return;
    }
    // Called only when SSE capture is enabled. Activity precedes cap rejection.
    inner.last_activity = Some(Instant::now());
    let req_id = string_field(&ev.params, "requestId");
    if req_id.is_empty() {
        return;
    }
    let event = serde_json::json!({
        "event_name": ev.params.get("eventName").and_then(|v| v.as_str()).unwrap_or(""),
        "data": ev.params.get("data").and_then(|v| v.as_str()).unwrap_or(""),
        "event_id": ev.params.get("eventId").and_then(|v| v.as_str()).unwrap_or(""),
        "timestamp_monotonic_ms": ev.params.get("timestamp")
            .and_then(|v| v.as_f64())
            .map(|t| (t * 1000.0) as u64)
            .unwrap_or(0),
    });
    if !push_stream_event(&mut inner.sse_events, req_id, event, "data") {
        inner.truncated = true;
    }
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
            let timestamp_epoch_ms = ev
                .params
                .get("timestamp")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let text = format_args_array(ev.params.get("args"));
            let (url, line_number) = stack_frame_origin(ev.params.get("stackTrace"));
            Some(ConsoleEvent {
                level,
                timestamp_epoch_ms,
                text,
                source_url: url,
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
            let timestamp_epoch_ms = ev
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
                timestamp_epoch_ms,
                text,
                source_url: url,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(method: &str, params: Value) -> CdpEvent {
        CdpEvent {
            method: method.into(),
            session_id: Some("capture".into()),
            params,
        }
    }

    fn request(request_id: &str, resource_type: &str) -> CdpEvent {
        event(
            "Network.requestWillBeSent",
            json!({
                "requestId": request_id,
                "type": resource_type,
                "request": {"url": "https://example.test/data", "method": "GET"},
            }),
        )
    }

    fn console(text: &str) -> CdpEvent {
        event(
            "Runtime.consoleAPICalled",
            json!({"args": [{"value": text}]}),
        )
    }

    #[tokio::test]
    async fn collectors_continue_after_a_lagged_burst_and_report_loss() {
        let (tx, rx) = broadcast::channel(256);
        let mut network = NetworkCollector::from_receiver(rx, "capture".into(), true, false, false);
        let mut console_collector =
            ConsoleCollector::from_receiver(tx.subscribe(), "capture".into());
        let network_guard = network.inner.lock().await;
        let console_guard = console_collector.inner.lock().await;
        tx.send(console("before burst")).unwrap();
        // Both collectors are held at their aggregate lock while CDP bursts.
        tokio::task::yield_now().await;
        for index in 0..600 {
            tx.send(event("Test.burst", json!({"index": index})))
                .unwrap();
        }
        tx.send(request("late", "Document")).unwrap();
        tx.send(event(
            "Network.responseReceived",
            json!({"requestId": "late", "response": {"status": 200}}),
        ))
        .unwrap();
        tx.send(event(
            "Network.loadingFinished",
            json!({"requestId": "late", "encodedDataLength": 1000.0}),
        ))
        .unwrap();
        tx.send(console("after burst")).unwrap();
        drop(tx);
        drop(network_guard);
        drop(console_guard);
        tokio::time::timeout(Duration::from_secs(1), &mut network.task)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), &mut console_collector.task)
            .await
            .unwrap()
            .unwrap();

        // Loss is reported, not turned into an endless wait: the retained
        // late request finished and nothing else is active.
        assert!(network.quiet_snapshot(Duration::ZERO).await.quiet);
        let log = network.finish().await;
        assert!(log.summary.events_dropped > 0);
        assert_eq!(log.entries.len(), 1);
        assert_eq!(log.entries[0].state, NetworkEntryState::Finished);
        assert_eq!(log.entries[0].hints["encoded_data_length"], json!(1000.0));
        let log = console_collector.finish().await;
        assert!(log.events_dropped > 0);
        assert!(log.events.iter().any(|entry| entry.text == "after burst"));
    }

    #[tokio::test]
    async fn dropping_collectors_aborts_their_receivers() {
        let (tx, rx) = broadcast::channel(256);
        let network = NetworkCollector::from_receiver(rx, "capture".into(), true, true, true);
        let console_collector = ConsoleCollector::from_receiver(tx.subscribe(), "capture".into());
        let network_inner = Arc::downgrade(&network.inner);
        let console_inner = Arc::downgrade(&console_collector.inner);
        assert_eq!(tx.receiver_count(), 2);
        drop(network);
        drop(console_collector);
        tokio::time::timeout(Duration::from_secs(1), async {
            while tx.receiver_count() > 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(network_inner.upgrade().is_none());
        assert!(console_inner.upgrade().is_none());
    }

    #[tokio::test]
    async fn uncaptured_stream_events_do_not_delay_network_quiet() {
        for capture_streams in [false, true] {
            let (tx, rx) = broadcast::channel(16);
            let mut collector = NetworkCollector::from_receiver(
                rx,
                "capture".into(),
                true,
                capture_streams,
                capture_streams,
            );
            tx.send(event(
                "Network.webSocketFrameReceived",
                json!({"requestId": "background"}),
            ))
            .unwrap();
            tx.send(event(
                "Network.eventSourceMessageReceived",
                json!({"requestId": "background"}),
            ))
            .unwrap();
            tx.send(event(
                "Network.dataReceived",
                json!({"requestId": "background"}),
            ))
            .unwrap();
            drop(tx);
            tokio::time::timeout(Duration::from_secs(1), &mut collector.task)
                .await
                .unwrap()
                .unwrap();
            let inner = collector.inner.lock().await;
            assert_eq!(inner.last_activity.is_some(), capture_streams);
            assert_eq!(inner.ws_frames.len(), usize::from(capture_streams));
            assert_eq!(inner.sse_events.len(), usize::from(capture_streams));
            drop(inner);
            assert_eq!(
                collector
                    .quiet_snapshot(Duration::from_secs(60))
                    .await
                    .quiet,
                !capture_streams
            );
        }
    }

    #[tokio::test]
    async fn request_and_finished_limits_preserve_the_main_document_and_updates() {
        let mut inner = NetworkInner::default();
        for index in 0..NETWORK_ENTRY_LIMIT + 8 {
            handle_event(&mut inner, &request(&format!("xhr-{index}"), "XHR"), true);
        }
        assert_eq!(inner.slots.len(), NETWORK_ENTRY_LIMIT - 1);
        assert!(inner.truncated);
        assert!(handle_event(&mut inner, &request("main", "Document"), true));
        assert_eq!(inner.slots.len(), NETWORK_ENTRY_LIMIT);
        assert_eq!(inner.main_request_id.as_deref(), Some("main"));
        handle_event(
            &mut inner,
            &event(
                "Network.responseReceived",
                json!({"requestId": "xhr-0", "response": {"status": 201}}),
            ),
            true,
        );
        for index in 0..NETWORK_ENTRY_LIMIT * 2 {
            handle_event(
                &mut inner,
                &event("Network.loadingFinished", json!({"requestId": "xhr-0"})),
                true,
            );
            handle_event(
                &mut inner,
                &event(
                    "Network.loadingFinished",
                    json!({"requestId": format!("unknown-{index}")}),
                ),
                true,
            );
        }
        assert_eq!(inner.finished.len(), 1);
        assert_eq!(inner.slots["xhr-0"].entry.status, Some(201));
        assert_eq!(
            inner.slots["xhr-0"].entry.state,
            NetworkEntryState::Finished
        );
        inner.last_activity = Some(Instant::now() - Duration::from_secs(60));
        handle_event(&mut inner, &request("rejected", "XHR"), true);
        assert!(inner.last_activity.unwrap().elapsed() < Duration::from_secs(1));

        // With every retained request finished, truncation is reported in the
        // summary but does not hold readiness: rejected requests still stamp
        // activity, so the idle window covers them.
        for slot in inner.slots.values_mut() {
            slot.entry.state = NetworkEntryState::Finished;
        }
        let (_tx, rx) = broadcast::channel(1);
        let collector = NetworkCollector::from_receiver(rx, "capture".into(), true, false, false);
        *collector.inner.lock().await = inner;
        assert!(
            !collector
                .quiet_snapshot(Duration::from_secs(60))
                .await
                .quiet
        );
        assert!(collector.quiet_snapshot(Duration::ZERO).await.quiet);
        assert!(collector.finish().await.summary.truncated);
    }

    #[test]
    fn stream_limits_bound_unknown_keys_and_survive_drains() {
        let mut inner = NetworkInner::default();
        for index in 0..NETWORK_ENTRY_LIMIT + 8 {
            handle_ws_event(
                &mut inner,
                &event(
                    "Network.webSocketFrameReceived",
                    json!({"requestId": format!("stream-{index}"), "response": {"payloadData": "frame"}}),
                ),
            );
            handle_sse_event(
                &mut inner,
                &event(
                    "Network.eventSourceMessageReceived",
                    json!({"requestId": format!("stream-{index}"), "data": "event"}),
                ),
            );
        }
        assert!(inner.slots.is_empty());
        assert_eq!(inner.ws_frames.len(), NETWORK_ENTRY_LIMIT);
        assert_eq!(inner.sse_events.len(), NETWORK_ENTRY_LIMIT);
        for _ in 0..STREAM_EVENT_LIMIT_PER_REQUEST + 8 {
            handle_ws_event(
                &mut inner,
                &event(
                    "Network.webSocketFrameSent",
                    json!({"requestId": "stream-0"}),
                ),
            );
            handle_sse_event(
                &mut inner,
                &event(
                    "Network.eventSourceMessageReceived",
                    json!({"requestId": "stream-0"}),
                ),
            );
        }
        let ws = take_stream_events(&mut inner.ws_frames);
        let sse = take_stream_events(&mut inner.sse_events);
        assert_eq!(ws["stream-0"].len(), STREAM_EVENT_LIMIT_PER_REQUEST);
        assert_eq!(sse["stream-0"].len(), STREAM_EVENT_LIMIT_PER_REQUEST);
        assert!(inner.truncated);
        assert!(!push_stream_event(
            &mut inner.ws_frames,
            "stream-0".into(),
            json!({}),
            "payload"
        ));
        assert!(!push_stream_event(
            &mut inner.sse_events,
            "stream-0".into(),
            json!({}),
            "payload"
        ));
        assert!(!push_stream_event(
            &mut inner.ws_frames,
            "new-after-drain".into(),
            json!({}),
            "payload"
        ));
        assert!(!push_stream_event(
            &mut inner.sse_events,
            "new-after-drain".into(),
            json!({}),
            "payload"
        ));
        assert!(take_stream_events(&mut inner.ws_frames).is_empty());
        assert!(take_stream_events(&mut inner.sse_events).is_empty());
        inner.last_activity = Some(Instant::now() - Duration::from_secs(60));
        handle_ws_event(
            &mut inner,
            &event(
                "Network.webSocketFrameError",
                json!({"requestId": "stream-0"}),
            ),
        );
        assert!(inner.last_activity.unwrap().elapsed() < Duration::from_secs(1));
        inner.last_activity = Some(Instant::now() - Duration::from_secs(60));
        handle_sse_event(
            &mut inner,
            &event(
                "Network.eventSourceMessageReceived",
                json!({"requestId": "stream-0"}),
            ),
        );
        assert!(inner.last_activity.unwrap().elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn console_capture_has_a_fixed_event_limit() {
        let (tx, rx) = broadcast::channel(CONSOLE_EVENT_LIMIT * 2);
        let mut collector = ConsoleCollector::from_receiver(rx, "capture".into());
        for _ in 0..CONSOLE_EVENT_LIMIT + 8 {
            tx.send(console("entry")).unwrap();
        }
        drop(tx);
        tokio::time::timeout(Duration::from_secs(1), &mut collector.task)
            .await
            .unwrap()
            .unwrap();
        let log = collector.finish().await;
        assert_eq!(log.events.len(), CONSOLE_EVENT_LIMIT);
        assert!(log.truncated);
        assert_eq!(log.events_dropped, 0);
    }

    #[test]
    fn capture_stats_are_optional_for_legacy_artifacts() {
        let console: ConsoleLog =
            serde_json::from_value(json!({"schema_version": 2, "events": []})).unwrap();
        assert_eq!(console.events_dropped, 0);
        assert!(!console.truncated);
        let value = serde_json::to_value(console).unwrap();
        assert!(value.get("events_dropped").is_none());
        assert!(value.get("truncated").is_none());
        let value = serde_json::to_value(NetworkSummary::default()).unwrap();
        assert!(value.get("events_dropped").is_none());
        assert!(value.get("truncated").is_none());
        let summary: NetworkSummary = serde_json::from_value(value).unwrap();
        assert_eq!(summary.events_dropped, 0);
        assert!(!summary.truncated);
    }

    #[tokio::test]
    async fn collectors_keep_only_their_own_session() {
        let (tx, rx) = broadcast::channel(64);
        let mut network = NetworkCollector::from_receiver(rx, "capture".into(), true, false, false);
        let mut console_collector =
            ConsoleCollector::from_receiver(tx.subscribe(), "capture".into());
        let mut other = request("other-tab", "Document");
        other.session_id = Some("other".into());
        let mut unscoped = request("browser-level", "XHR");
        unscoped.session_id = None;
        let mut other_console = console("other tab");
        other_console.session_id = Some("other".into());
        for ev in [
            other,
            unscoped,
            other_console,
            request("mine", "Document"),
            console("mine"),
        ] {
            tx.send(ev).unwrap();
        }
        drop(tx);
        tokio::time::timeout(Duration::from_secs(1), &mut network.task)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), &mut console_collector.task)
            .await
            .unwrap()
            .unwrap();
        let log = network.finish().await;
        let ids: Vec<_> = log.entries.iter().map(|e| e.request_id.as_str()).collect();
        assert_eq!(ids, ["mine"]);
        assert_eq!(log.main_request_id.as_deref(), Some("mine"));
        let log = console_collector.finish().await;
        let texts: Vec<_> = log.events.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["mine"]);
    }

    #[test]
    fn redirect_hops_are_kept_as_a_linked_chain() {
        let mut inner = NetworkInner::default();
        let hop = |url: &str, ts: f64, redirect: Option<u16>| {
            let mut params = json!({
                "requestId": "main",
                "type": "Document",
                "frameId": "root",
                "timestamp": ts,
                "request": {"url": url, "method": "GET"},
            });
            if let Some(status) = redirect {
                params["redirectResponse"] =
                    json!({"status": status, "headers": {"Location": url}});
            }
            event("Network.requestWillBeSent", params)
        };
        handle_event(&mut inner, &hop("https://a.test/", 1.0, None), true);
        handle_event(&mut inner, &hop("https://b.test/", 2.0, Some(302)), true);
        handle_event(&mut inner, &hop("https://c.test/", 3.0, Some(301)), true);
        handle_event(
            &mut inner,
            &event(
                "Network.responseReceived",
                json!({"requestId": "main", "response": {"status": 200}}),
            ),
            true,
        );
        assert_eq!(inner.slots.len(), 3);
        let first = &inner.slots["main#redirect0"].entry;
        assert_eq!(first.request_url, "https://a.test/");
        assert_eq!(first.status, Some(302));
        assert_eq!(first.state, NetworkEntryState::Finished);
        assert_eq!(first.redirect_from_request_id, None);
        assert_eq!(first.response_headers["Location"], "https://b.test/");
        let second = &inner.slots["main#redirect1"].entry;
        assert_eq!(second.request_url, "https://b.test/");
        assert_eq!(second.status, Some(301));
        assert_eq!(
            second.redirect_from_request_id.as_deref(),
            Some("main#redirect0")
        );
        let last = &inner.slots["main"].entry;
        assert_eq!(last.request_url, "https://c.test/");
        assert_eq!(last.status, Some(200));
        assert_eq!(
            last.redirect_from_request_id.as_deref(),
            Some("main#redirect1")
        );
        assert_eq!(inner.main_request_id.as_deref(), Some("main"));
        // An ended hop is never chosen as the main document, even when its
        // timestamp ties the live request's.
        inner
            .slots
            .get_mut("main")
            .unwrap()
            .entry
            .timing
            .start_monotonic_ms = 0;
        select_main_document(&mut inner, "root");
        assert_eq!(inner.main_request_id.as_deref(), Some("main"));
        assert!(inner.finished.is_empty());
    }

    #[tokio::test]
    async fn main_status_follows_the_latest_root_frame_document() {
        let (tx, rx) = broadcast::channel(64);
        let mut collector =
            NetworkCollector::from_receiver(rx, "capture".into(), true, false, false);
        for (id, frame, ts, status) in [
            ("page-a", "root", 1.0, 200),
            ("subframe", "child", 3.0, 500),
            ("page-b", "root", 2.0, 404),
        ] {
            tx.send(event(
                "Network.requestWillBeSent",
                json!({"requestId": id, "type": "Document", "frameId": frame, "timestamp": ts,
                       "request": {"url": format!("https://example.test/{id}")}}),
            ))
            .unwrap();
            tx.send(event(
                "Network.responseReceived",
                json!({"requestId": id, "response": {"status": status}}),
            ))
            .unwrap();
        }
        drop(tx);
        tokio::time::timeout(Duration::from_secs(1), &mut collector.task)
            .await
            .unwrap()
            .unwrap();
        // Without the root frame, the first Document seen stays the main one.
        let first = collector
            .wait_for_main_status(Duration::ZERO, None)
            .await
            .unwrap();
        assert_eq!(first.request_id, "page-a");
        let latest = collector
            .wait_for_main_status(Duration::ZERO, Some("root"))
            .await
            .unwrap();
        assert_eq!(latest.request_id, "page-b");
        assert_eq!(latest.status, Some(404));
        assert_eq!(collector.main_request_id().await.as_deref(), Some("page-b"));
    }

    #[test]
    fn stream_payloads_have_a_per_request_byte_budget() {
        let mut inner = NetworkInner::default();
        let half = "x".repeat(STREAM_PAYLOAD_BYTES_PER_REQUEST / 2);
        for _ in 0..2 {
            handle_ws_event(
                &mut inner,
                &event(
                    "Network.webSocketFrameReceived",
                    json!({"requestId": "ws", "response": {"payloadData": half}}),
                ),
            );
        }
        assert!(!inner.truncated);
        // Multi-byte text is cut on a character boundary.
        let sse = format!("x{}", "é".repeat(STREAM_PAYLOAD_BYTES_PER_REQUEST / 2));
        handle_sse_event(
            &mut inner,
            &event(
                "Network.eventSourceMessageReceived",
                json!({"requestId": "sse", "data": sse}),
            ),
        );
        assert!(inner.truncated);
        let cut = &inner.sse_events["sse"].events[0];
        assert_eq!(cut["payload_truncated"], true);
        assert_eq!(cut["payload_bytes"], sse.len());
        assert_eq!(
            cut["data"].as_str().unwrap().len(),
            STREAM_PAYLOAD_BYTES_PER_REQUEST - 1
        );
        inner.truncated = false;
        handle_ws_event(
            &mut inner,
            &event(
                "Network.webSocketFrameSent",
                json!({"requestId": "ws", "response": {"payloadData": "one more"}}),
            ),
        );
        assert!(inner.truncated);
        let frames = take_stream_events(&mut inner.ws_frames);
        assert_eq!(frames["ws"].len(), 2);
        assert!(
            frames["ws"]
                .iter()
                .all(|f| f.get("payload_truncated").is_none())
        );
    }
}
