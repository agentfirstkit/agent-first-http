//! Idle-tab reaper: closes page targets that nobody is driving.
//!
//! Drivers close the targets they open, but a driver that is killed
//! mid-fetch, an agent that walks away from a takeover, or a raw CDP client
//! that created targets and never returned all leave tabs behind — and a
//! leaked tab is not idle for the browser: challenge pages poll, SPAs run
//! timers, and the host launches Chromium with background throttling
//! disabled (a live fetch needs that), so an unowned tab burns CPU forever.
//! The host is the only party that sees every target, so it is the one that
//! reclaims them.
//!
//! A target is reaped when the host has been continuously idle for the
//! timeout: no `/cdp` proxy client connected and no takeover credential
//! live. The connection count covers every driver (all CDP traffic flows
//! through the proxy); credential liveness covers the human on the real
//! display, who drives the browser without a CDP connection. The backend's
//! per-target `attached` flag is deliberately not consulted: on a
//! chromium-family host the launcher itself keeps every page attached, so
//! the flag says nothing about drivers. Only `http(s)` pages are considered
//! — `about:blank` and browser-internal pages cost nothing and keeping them
//! means a headful browser (which exits with its last window) always has a
//! window left.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::host::listener::AppState;
use crate::sdk::cdp::ws_client::Connection;

pub(super) struct ReaperGuard {
    task: tokio::task::JoinHandle<()>,
}

impl Drop for ReaperGuard {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Matches the default takeover-handoff TTL: a tab parked for a human gets
/// the same patience as the URL that would have sent one.
pub const DEFAULT_TAB_IDLE_TIMEOUT_S: u64 = 900;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(3);

/// Spawn the reaper as a background task. `idle_timeout` must be non-zero;
/// callers gate on the `--tab-idle-timeout-s 0` opt-out before spawning.
pub fn spawn(state: AppState, idle_timeout: Duration) {
    spawn_with_interval(state, idle_timeout, sweep_interval(idle_timeout));
}

pub(super) fn spawn_scoped(state: AppState, idle_timeout: Duration) -> ReaperGuard {
    ReaperGuard {
        task: spawn_task(state, idle_timeout, sweep_interval(idle_timeout)),
    }
}

fn sweep_interval(idle_timeout: Duration) -> Duration {
    (idle_timeout / 3).clamp(Duration::from_millis(500), Duration::from_secs(60))
}

/// [`spawn`] with an explicit sweep interval; split out so tests can sweep
/// fast without shrinking the timeout below what they mean to assert.
pub fn spawn_with_interval(state: AppState, idle_timeout: Duration, interval: Duration) {
    drop(spawn_task(state, idle_timeout, interval));
}

fn spawn_task(
    state: AppState,
    idle_timeout: Duration,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tracker = Tracker::default();
        loop {
            tokio::time::sleep(interval).await;
            sweep(&state, idle_timeout, &mut tracker).await;
        }
    })
}

/// Per-target idle clocks. Keyed by the backend websocket URL so a runtime
/// profile switch (new browser, new targets) starts everything fresh.
#[derive(Default)]
struct Tracker {
    ws_url: String,
    idle_since: HashMap<String, Instant>,
}

async fn sweep(state: &AppState, idle_timeout: Duration, tracker: &mut Tracker) {
    let Some(entry) = state.get_profile() else {
        tracker.idle_since.clear();
        return;
    };
    // A connected driver or a live credential means someone is using (or on
    // their way to) this browser. Clearing the clocks rather than pausing
    // them gives every tab a full timeout of grace after the activity ends,
    // instead of reaping the instant the last connection or credential
    // lapses.
    if state.cdp_client_count() > 0 || state.has_live_takeover_credentials() {
        tracker.idle_since.clear();
        return;
    }
    if tracker.ws_url != entry.ws_url() {
        tracker.idle_since.clear();
        tracker.ws_url = entry.ws_url().to_string();
    }
    let Ok(Ok(conn)) =
        tokio::time::timeout(CONNECT_TIMEOUT, Connection::connect(entry.ws_url(), None)).await
    else {
        return;
    };
    let Ok(targets) = conn
        .send_timeout(
            "Target.getTargets",
            &serde_json::json!({}),
            None,
            COMMAND_TIMEOUT,
        )
        .await
    else {
        conn.close();
        return;
    };

    let now = Instant::now();
    let mut seen: HashSet<String> = HashSet::new();
    let mut expired: Vec<String> = Vec::new();
    let mut surviving_pages = 0usize;
    for target in page_targets(&targets) {
        seen.insert(target.id.clone());
        if !is_reapable_url(&target.url) {
            tracker.idle_since.remove(&target.id);
            surviving_pages += 1;
            continue;
        }
        let since = *tracker.idle_since.entry(target.id.clone()).or_insert(now);
        if now.duration_since(since) >= idle_timeout {
            expired.push(target.id);
        } else {
            surviving_pages += 1;
        }
    }
    tracker.idle_since.retain(|id, _| seen.contains(id));

    if expired.is_empty() {
        conn.close();
        return;
    }
    // A headful browser exits when its last window closes; make sure one
    // survives the sweep.
    if surviving_pages == 0 {
        let _ = conn
            .send_timeout(
                "Target.createTarget",
                &serde_json::json!({ "url": "about:blank" }),
                None,
                COMMAND_TIMEOUT,
            )
            .await;
    }
    for id in expired {
        let closed = conn
            .send_timeout(
                "Target.closeTarget",
                &serde_json::json!({ "targetId": id }),
                None,
                COMMAND_TIMEOUT,
            )
            .await
            .is_ok();
        if closed {
            tracker.idle_since.remove(&id);
            // The URL stays out of this event: page URLs can carry session
            // secrets, and the target id is what a driver would have logged.
            let _ = crate::shared::afdata::emit_process(
                "tab_reaped",
                &serde_json::json!({
                    "target_id": id,
                    "idle_timeout_s": idle_timeout.as_secs(),
                }),
            );
        }
    }
    conn.close();
}

struct PageTarget {
    id: String,
    url: String,
}

fn page_targets(response: &Value) -> Vec<PageTarget> {
    response
        .get("targetInfos")
        .and_then(Value::as_array)
        .map(|targets| {
            targets
                .iter()
                .filter(|t| t.get("type").and_then(Value::as_str) == Some("page"))
                .map(|t| PageTarget {
                    id: t
                        .get("targetId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    url: t
                        .get("url")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                })
                .filter(|t| !t.id.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Only real web pages are worth reaping. Blank and browser-internal targets
/// cost nothing to keep, and skipping them is what guarantees a headful
/// browser keeps a window even before the zero-survivor guard.
fn is_reapable_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_pages_are_reapable() {
        assert!(is_reapable_url("https://example.test/challenge"));
        assert!(is_reapable_url("http://127.0.0.1:8080/"));
        assert!(!is_reapable_url("about:blank"));
        assert!(!is_reapable_url("chrome://newtab/"));
        assert!(!is_reapable_url("brave://settings/"));
        assert!(!is_reapable_url(
            "devtools://devtools/bundled/inspector.html"
        ));
        assert!(!is_reapable_url(""));
    }

    #[test]
    fn page_targets_keeps_only_pages_with_ids() {
        let response = serde_json::json!({
            "targetInfos": [
                {"targetId": "t1", "type": "page", "url": "https://a.test/", "attached": true},
                {"targetId": "t2", "type": "page", "url": "about:blank"},
                {"targetId": "t3", "type": "service_worker", "url": "https://a.test/sw.js"},
                {"type": "page", "url": "https://no-id.test/"},
            ]
        });
        let pages = page_targets(&response);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].id, "t1");
        assert_eq!(pages[0].url, "https://a.test/");
        assert_eq!(pages[1].id, "t2");
        assert_eq!(pages[1].url, "about:blank");
    }
}
