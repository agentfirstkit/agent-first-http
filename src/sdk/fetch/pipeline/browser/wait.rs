//! Wait/readiness logic for the browser path: `--wait` mode evaluation and
//! the `auto` network-quiet + DOM/text-stability readiness loop.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::sdk::cdp::ws_client::Connection;
use crate::sdk::fetch::artifacts::collectors::{NetworkCollector, NetworkQuietSnapshot};
use crate::sdk::fetch::deadline::FetchDeadline;
use crate::sdk::fetch::wait::Wait;
use crate::shared::error::{Error, ErrorCode};

use super::{cdp_send, sleep_until_or_deadline};

#[derive(Debug, Clone)]
pub(super) struct WaitOutcome {
    pub(super) wait_mode: String,
    pub(super) wait_satisfied_by: Option<String>,
    pub(super) network_quiet: Option<bool>,
    pub(super) dom_stable: Option<bool>,
    pub(super) text_stable: Option<bool>,
    pub(super) capture_reason: String,
    pub(super) readiness_timed_out: bool,
}

impl WaitOutcome {
    fn satisfied(wait: &Wait, by: impl Into<String>) -> Self {
        Self {
            wait_mode: wait.mode_name().to_string(),
            wait_satisfied_by: Some(by.into()),
            network_quiet: None,
            dom_stable: None,
            text_stable: None,
            capture_reason: "wait_satisfied".into(),
            readiness_timed_out: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PageMetrics {
    text_len: usize,
    text_hash: u32,
    node_count: usize,
    html_hash: u32,
}

#[derive(Debug, Clone)]
struct StabilityTracker {
    last: Option<PageMetrics>,
    unchanged_since: Instant,
    dom_stable: bool,
    text_stable: bool,
}

impl StabilityTracker {
    fn new(now: Instant) -> Self {
        Self {
            last: None,
            unchanged_since: now,
            dom_stable: false,
            text_stable: false,
        }
    }

    fn observe(&mut self, metrics: PageMetrics, stable_for: Duration, now: Instant) {
        let same_dom = self.last.as_ref().is_some_and(|last| {
            last.node_count == metrics.node_count && last.html_hash == metrics.html_hash
        });
        let same_text = self.last.as_ref().is_some_and(|last| {
            last.text_len == metrics.text_len && last.text_hash == metrics.text_hash
        });
        if !same_dom || !same_text {
            self.unchanged_since = now;
            self.dom_stable = false;
            self.text_stable = false;
        } else if now.saturating_duration_since(self.unchanged_since) >= stable_for {
            self.dom_stable = true;
            self.text_stable = true;
        }
        self.last = Some(metrics);
    }
}

pub(super) struct WaitContext<'a> {
    pub(super) conn: &'a Connection,
    pub(super) session_id: &'a str,
    pub(super) collector: &'a NetworkCollector,
    pub(super) deadline: &'a FetchDeadline,
}

pub(super) struct WaitTuning {
    pub(super) timeout: Duration,
    pub(super) readiness_idle_ms: u64,
    pub(super) readiness_stable_ms: u64,
}

pub(super) async fn wait_for_condition(
    ctx: WaitContext<'_>,
    wait: &Wait,
    tuning: WaitTuning,
) -> Result<WaitOutcome, Error> {
    let conn = ctx.conn;
    let session_id = ctx.session_id;
    let deadline = ctx.deadline;
    let sid = session_id.to_string();
    match wait {
        Wait::Auto => {
            wait_for_auto_readiness(
                conn,
                session_id,
                ctx.collector,
                Duration::from_millis(tuning.readiness_idle_ms),
                Duration::from_millis(tuning.readiness_stable_ms),
                tuning.timeout,
                deadline,
            )
            .await
        }
        Wait::Load => {
            wait_for_load(
                conn,
                session_id,
                &sid,
                explicit_wait_budget(deadline.remaining("wait_readiness")?, tuning.timeout),
                deadline,
            )
            .await?;
            Ok(WaitOutcome::satisfied(wait, "load"))
        }
        Wait::Idle => {
            let _ = cdp_send(
                conn,
                session_id,
                "Page.setLifecycleEventsEnabled",
                &serde_json::json!({"enabled": true}),
                "wait_readiness",
                deadline,
            )
            .await;
            conn.wait_event(
                explicit_wait_budget(deadline.remaining("wait_readiness")?, tuning.timeout),
                |ev| {
                    ev.method == "Page.lifecycleEvent"
                        && ev.params["name"].as_str() == Some("networkIdle")
                        && ev.session_id.as_deref() == Some(&sid)
                },
            )
            .await?;
            Ok(WaitOutcome::satisfied(wait, "network_idle_event"))
        }
        Wait::Selector(sel) => {
            let deadline_at = Instant::now()
                + explicit_wait_budget(deadline.remaining("wait_readiness")?, tuning.timeout);
            while Instant::now() < deadline_at {
                let expr = format!(
                    "!!document.querySelector({})",
                    serde_json::to_string(sel).unwrap_or_else(|_| "null".into())
                );
                let r = cdp_send(
                    conn,
                    session_id,
                    "Runtime.evaluate",
                    &serde_json::json!({
                        "expression": expr,
                        "returnByValue": true,
                    }),
                    "wait_readiness",
                    deadline,
                )
                .await?;
                if r["result"]["value"].as_bool() == Some(true) {
                    return Ok(WaitOutcome::satisfied(wait, "selector"));
                }
                sleep_until_or_deadline(deadline_at, Duration::from_millis(50)).await;
            }
            Err(Error::new(
                ErrorCode::WaitSelectorUnmatched,
                format!("selector {sel:?} did not appear before --timeout-ms"),
            ))
        }
        Wait::SelectorVisible(sel) => {
            let deadline_at = Instant::now()
                + explicit_wait_budget(deadline.remaining("wait_readiness")?, tuning.timeout);
            let json_sel = serde_json::to_string(sel).unwrap_or_else(|_| "null".into());
            let expr = format!(
                "(function(){{\
                   const el = document.querySelector({json_sel});\
                   if (!el) return false;\
                   const r = el.getBoundingClientRect();\
                   if (r.width === 0 || r.height === 0) return false;\
                   const style = window.getComputedStyle(el);\
                   if (style.visibility === 'hidden') return false;\
                   if (style.display === 'none') return false;\
                   if (style.opacity === '0') return false;\
                   if (style.position !== 'fixed' && el.offsetParent === null) return false;\
                   return true;\
                 }})()"
            );
            while Instant::now() < deadline_at {
                let r = cdp_send(
                    conn,
                    session_id,
                    "Runtime.evaluate",
                    &serde_json::json!({
                        "expression": expr,
                        "returnByValue": true,
                    }),
                    "wait_readiness",
                    deadline,
                )
                .await?;
                if r["result"]["value"].as_bool() == Some(true) {
                    return Ok(WaitOutcome::satisfied(wait, "selector_visible"));
                }
                sleep_until_or_deadline(deadline_at, Duration::from_millis(50)).await;
            }
            Err(Error::new(
                ErrorCode::WaitSelectorUnmatched,
                format!("selector-visible {sel:?} did not appear visible before --timeout-ms"),
            ))
        }
        Wait::Ms(n) => {
            let sleep = Duration::from_millis(*n).min(explicit_wait_budget(
                deadline.remaining("wait_readiness")?,
                tuning.timeout,
            ));
            tokio::time::sleep(sleep).await;
            Ok(WaitOutcome::satisfied(wait, "ms"))
        }
    }
}

fn explicit_wait_budget(remaining: Duration, requested: Duration) -> Duration {
    remaining
        .min(requested)
        .checked_sub(Duration::from_millis(100))
        .unwrap_or_else(|| remaining.min(requested))
}

async fn wait_for_auto_readiness(
    conn: &Connection,
    session_id: &str,
    collector: &NetworkCollector,
    idle_for: Duration,
    stable_for: Duration,
    timeout: Duration,
    deadline: &FetchDeadline,
) -> Result<WaitOutcome, Error> {
    let sid = session_id.to_string();
    let wait_budget = auto_readiness_budget(deadline.remaining("wait_readiness")?, timeout);
    let wait_deadline = Instant::now() + wait_budget;
    let _ = wait_for_load(conn, session_id, &sid, wait_budget, deadline).await;

    let mut tracker = StabilityTracker::new(Instant::now());
    let mut last_network = NetworkQuietSnapshot {
        quiet: false,
        idle_for: Duration::ZERO,
        inflight_total: 0,
        pending_by_resource_type: Default::default(),
    };

    loop {
        let now = Instant::now();
        if now >= wait_deadline {
            return Ok(WaitOutcome {
                wait_mode: "auto".into(),
                wait_satisfied_by: None,
                network_quiet: Some(last_network.quiet),
                dom_stable: Some(tracker.dom_stable),
                text_stable: Some(tracker.text_stable),
                capture_reason: "readiness_timeout".into(),
                readiness_timed_out: true,
            });
        }

        last_network = collector.quiet_snapshot(idle_for).await;
        if let Ok(metrics) = capture_page_metrics(conn, session_id, deadline).await {
            tracker.observe(metrics, stable_for, now);
        }
        if last_network.quiet && tracker.dom_stable && tracker.text_stable {
            return Ok(WaitOutcome {
                wait_mode: "auto".into(),
                wait_satisfied_by: Some("network_quiet_dom_text_stable".into()),
                network_quiet: Some(true),
                dom_stable: Some(true),
                text_stable: Some(true),
                capture_reason: "wait_satisfied".into(),
                readiness_timed_out: false,
            });
        }
        sleep_until_or_deadline(wait_deadline, Duration::from_millis(100)).await;
    }
}

fn auto_readiness_budget(remaining: Duration, timeout: Duration) -> Duration {
    let reserve = Duration::from_millis(750).min(timeout / 3);
    remaining
        .checked_sub(reserve)
        .unwrap_or_else(|| remaining.min(Duration::from_millis(100)))
}

async fn wait_for_load(
    conn: &Connection,
    session_id: &str,
    sid: &str,
    timeout: Duration,
    deadline: &FetchDeadline,
) -> Result<(), Error> {
    if let Ok(r) = cdp_send(
        conn,
        session_id,
        "Runtime.evaluate",
        &serde_json::json!({
            "expression": "document.readyState",
            "returnByValue": true,
        }),
        "wait_readiness",
        deadline,
    )
    .await
    {
        if r["result"]["value"].as_str() == Some("complete") {
            return Ok(());
        }
    }
    let event_wait = async {
        conn.wait_event(timeout, |ev| {
            ev.method == "Page.loadEventFired" && ev.session_id.as_deref() == Some(sid)
        })
        .await
        .map(|_| ())
    };
    let poll = async {
        let deadline_at = Instant::now() + timeout;
        while Instant::now() < deadline_at {
            if let Ok(r) = cdp_send(
                conn,
                session_id,
                "Runtime.evaluate",
                &serde_json::json!({
                    "expression": "document.readyState",
                    "returnByValue": true,
                }),
                "wait_readiness",
                deadline,
            )
            .await
            {
                if r["result"]["value"].as_str() == Some("complete") {
                    return Ok::<(), Error>(());
                }
            }
            sleep_until_or_deadline(deadline_at, Duration::from_millis(50)).await;
        }
        Err(Error::new(
            ErrorCode::CdpTimeout,
            "Wait::Load: readyState never became complete",
        ))
    };
    tokio::select! {
        r = event_wait => r,
        r = poll => r,
    }
}

async fn capture_page_metrics(
    conn: &Connection,
    session_id: &str,
    deadline: &FetchDeadline,
) -> Result<PageMetrics, Error> {
    let r = cdp_send(
        conn,
        session_id,
        "Runtime.evaluate",
        &serde_json::json!({
            "expression": PAGE_METRICS_JS,
            "returnByValue": true,
        }),
        "wait_readiness",
        deadline,
    )
    .await?;
    let s = r["result"]["value"].as_str().unwrap_or("{}");
    let v: Value = serde_json::from_str(s).unwrap_or(Value::Null);
    Ok(PageMetrics {
        text_len: v["text_len"].as_u64().unwrap_or(0) as usize,
        text_hash: v["text_hash"].as_u64().unwrap_or(0) as u32,
        node_count: v["node_count"].as_u64().unwrap_or(0) as usize,
        html_hash: v["html_hash"].as_u64().unwrap_or(0) as u32,
    })
}

const PAGE_METRICS_JS: &str = r#"(() => {
  const text = document.body ? document.body.innerText : '';
  const html = document.documentElement ? document.documentElement.outerHTML : '';
  function hash(s) {
    let h = 2166136261;
    for (let i = 0; i < s.length; i++) {
      h ^= s.charCodeAt(i);
      h = Math.imul(h, 16777619);
    }
    return h >>> 0;
  }
  return JSON.stringify({
    text_len: text.length,
    text_hash: hash(text),
    node_count: document.getElementsByTagName('*').length,
    html_hash: hash(html)
  });
})()"#;
