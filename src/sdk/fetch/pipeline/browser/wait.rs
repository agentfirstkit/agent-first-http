//! Wait/readiness logic for the browser path: `--wait` mode evaluation and
//! the `auto` network-quiet + DOM/text-stability readiness loop.

use std::time::{Duration, Instant};

use super::readiness::Sampler;
use crate::sdk::fetch::readiness::{Readiness, ReadinessState};
use serde::Deserialize;

use crate::sdk::cdp::ws_client::Connection;
use crate::sdk::fetch::artifacts::collectors::NetworkCollector;
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
    pub(super) readiness: Option<Readiness>,
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
            readiness: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(super) struct PageMetrics {
    pub(super) text_len: usize,
    pub(super) text_hash: u32,
    pub(super) node_count: usize,
    pub(super) dom_hash: u32,
}

#[derive(Debug, Clone)]
struct StabilityTracker {
    last: Option<PageMetrics>,
    dom_unchanged_since: Instant,
    text_unchanged_since: Instant,
    dom_stable: bool,
    text_stable: bool,
}

impl StabilityTracker {
    fn new(now: Instant) -> Self {
        Self {
            last: None,
            dom_unchanged_since: now,
            text_unchanged_since: now,
            dom_stable: false,
            text_stable: false,
        }
    }

    fn observe(&mut self, metrics: PageMetrics, stable_for: Duration, now: Instant) {
        let same_dom = self.last.as_ref().is_some_and(|last| {
            last.node_count == metrics.node_count && last.dom_hash == metrics.dom_hash
        });
        let same_text = self.last.as_ref().is_some_and(|last| {
            last.text_len == metrics.text_len && last.text_hash == metrics.text_hash
        });
        if !same_dom {
            self.dom_unchanged_since = now;
        }
        if !same_text {
            self.text_unchanged_since = now;
        }
        self.dom_stable =
            same_dom && now.saturating_duration_since(self.dom_unchanged_since) >= stable_for;
        self.text_stable =
            same_text && now.saturating_duration_since(self.text_unchanged_since) >= stable_for;
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
        Wait::Auto | Wait::SelectorStable(_) => wait_for_readiness(ctx, wait, tuning).await,
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
            wait_for_network_idle(conn, session_id, tuning.timeout, deadline).await?;
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

/// Wait for the page's `networkIdle` lifecycle event.
///
/// The subscription must exist before lifecycle events are enabled: enabling
/// them makes the browser replay the lifecycle events the page has already
/// reached, `networkIdle` included, ahead of the command's response. A page
/// that went idle before this wait began is only ever reported by that replay.
async fn wait_for_network_idle(
    conn: &Connection,
    session_id: &str,
    requested: Duration,
    deadline: &FetchDeadline,
) -> Result<(), Error> {
    let events = conn.subscribe();
    let _ = cdp_send(
        conn,
        session_id,
        "Page.setLifecycleEventsEnabled",
        &serde_json::json!({"enabled": true}),
        "wait_readiness",
        deadline,
    )
    .await;
    Connection::wait_event_on(
        events,
        explicit_wait_budget(deadline.remaining("wait_readiness")?, requested),
        |ev| {
            ev.method == "Page.lifecycleEvent"
                && ev.params["name"].as_str() == Some("networkIdle")
                && ev.session_id.as_deref() == Some(session_id)
        },
    )
    .await?;
    Ok(())
}

fn explicit_wait_budget(remaining: Duration, requested: Duration) -> Duration {
    remaining
        .min(requested)
        .checked_sub(Duration::from_millis(100))
        .unwrap_or_else(|| remaining.min(requested))
}

async fn wait_for_readiness(
    ctx: WaitContext<'_>,
    wait: &Wait,
    tuning: WaitTuning,
) -> Result<WaitOutcome, Error> {
    let strict = matches!(wait, Wait::Auto);
    let selector = match wait {
        Wait::SelectorStable(selector) => selector.as_str(),
        _ => "body",
    };
    let idle_for = Duration::from_millis(tuning.readiness_idle_ms);
    let stable_for = Duration::from_millis(tuning.readiness_stable_ms);
    let wait_deadline = Instant::now()
        + auto_readiness_budget(ctx.deadline.remaining("wait_readiness")?, tuning.timeout);
    let mut sampler = Sampler::new(ctx.conn, ctx.session_id, ctx.deadline, selector, strict);
    let mut body_tracker = StabilityTracker::new(Instant::now());
    let mut target_tracker = StabilityTracker::new(Instant::now());
    let mut outcome = WaitOutcome {
        wait_mode: wait.mode_name().to_string(),
        wait_satisfied_by: None,
        network_quiet: Some(false),
        dom_stable: Some(false),
        text_stable: Some(false),
        capture_reason: "readiness_timeout".into(),
        readiness_timed_out: true,
        readiness: None,
    };
    loop {
        let now = Instant::now();
        if now >= wait_deadline {
            if outcome.readiness.is_none() {
                outcome.readiness =
                    Some(super::readiness::unknown_readiness(ctx.collector, selector).await);
            }
            if let Some(readiness) = outcome.readiness.as_mut() {
                readiness.timed_out = true;
            }
            return Ok(outcome);
        }
        let budget = wait_deadline
            .saturating_duration_since(now)
            .min(Duration::from_millis(500));
        match tokio::time::timeout(budget, sampler.sample(ctx.collector, idle_for)).await {
            Ok(Ok(mut sample)) => {
                body_tracker.observe(sample.body, stable_for, now);
                if sample.target_eligible {
                    target_tracker.observe(sample.target, stable_for, now);
                } else {
                    target_tracker = StabilityTracker::new(now);
                }
                let target_ready = sample.target_eligible
                    && sample.target_network_quiet
                    && target_tracker.dom_stable
                    && target_tracker.text_stable;
                sample.readiness.target_dom_stable = target_tracker.dom_stable;
                sample.readiness.target_text_stable = target_tracker.text_stable;
                if target_ready {
                    sample.readiness.target_content = ReadinessState::Ready;
                }
                let network = ctx.collector.quiet_snapshot(idle_for).await;
                let satisfied = target_ready && (!strict || network.quiet);
                sample.readiness.wait_satisfied = satisfied;
                outcome.network_quiet = Some(network.quiet);
                outcome.dom_stable = Some(body_tracker.dom_stable);
                outcome.text_stable = Some(body_tracker.text_stable);
                outcome.readiness = Some(sample.readiness);
                if satisfied {
                    outcome.wait_satisfied_by = Some(
                        if strict {
                            "network_quiet_dom_text_stable"
                        } else {
                            "selector_stable"
                        }
                        .into(),
                    );
                    outcome.capture_reason = "wait_satisfied".into();
                    outcome.readiness_timed_out = false;
                    return Ok(outcome);
                }
            }
            Ok(Err(error)) if error.error_code == ErrorCode::InvalidArgument => return Err(error),
            Err(_) if budget < Duration::from_millis(500) => {
                // The final sample ran out of wait budget, not evidence.
                // Preserve the last completed sample and its raw signals.
                if outcome.readiness.is_none() {
                    outcome.readiness =
                        Some(super::readiness::unknown_readiness(ctx.collector, selector).await);
                }
                if let Some(readiness) = outcome.readiness.as_mut() {
                    readiness.timed_out = true;
                }
                return Ok(outcome);
            }
            _ => {
                body_tracker = StabilityTracker::new(now);
                target_tracker = StabilityTracker::new(now);
                outcome.dom_stable = Some(false);
                outcome.text_stable = Some(false);
                if let Some(readiness) = outcome.readiness.as_mut() {
                    readiness.observation_complete = false;
                    readiness.target_content = ReadinessState::Unknown;
                    readiness.target_dom_stable = false;
                    readiness.target_text_stable = false;
                }
            }
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
        && r["result"]["value"].as_str() == Some("complete")
    {
        return Ok(());
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
                && r["result"]["value"].as_str() == Some("complete")
            {
                return Ok::<(), Error>(());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn idle_wait_sees_networkidle_replayed_before_the_enable_response() {
        use crate::sdk::fetch::result::TraceRenderMode;
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(message)) = ws.next().await {
                let Message::Text(text) = message else {
                    continue;
                };
                let request: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert_eq!(request["method"], "Page.setLifecycleEventsEnabled");
                // The page already went idle: the browser replays the event
                // ahead of the response, and sends nothing after it.
                let replay = serde_json::json!({
                    "method": "Page.lifecycleEvent",
                    "params": {"name": "networkIdle"},
                    "sessionId": "page",
                });
                let response = serde_json::json!({"id": request["id"], "result": {}});
                for frame in [replay, response] {
                    ws.send(Message::Text(frame.to_string().into()))
                        .await
                        .unwrap();
                }
            }
        });
        let conn = Connection::connect(&format!("ws://{addr}"), None)
            .await
            .unwrap();
        let deadline = FetchDeadline::new(Duration::from_secs(2), TraceRenderMode::Always);
        wait_for_network_idle(&conn, "page", Duration::from_secs(2), &deadline)
            .await
            .unwrap();
        conn.close();
        peer.await.unwrap();
    }

    fn metrics(dom_hash: u32, text_hash: u32) -> PageMetrics {
        PageMetrics {
            text_len: 10,
            text_hash,
            node_count: 10,
            dom_hash,
        }
    }

    #[test]
    fn dom_and_text_have_independent_stability_windows() {
        let now = Instant::now();
        let stable_for = Duration::from_millis(500);
        for changing_dom in [true, false] {
            let mut tracker = StabilityTracker::new(now);
            tracker.observe(metrics(1, 1), stable_for, now);
            tracker.observe(metrics(1, 1), stable_for, now + stable_for);
            assert!(tracker.dom_stable && tracker.text_stable);
            let changed = if changing_dom {
                metrics(2, 1)
            } else {
                metrics(1, 2)
            };
            tracker.observe(changed.clone(), stable_for, now + stable_for * 2);
            assert_eq!(tracker.dom_stable, !changing_dom);
            assert_eq!(tracker.text_stable, changing_dom);
            tracker.observe(
                changed.clone(),
                stable_for,
                now + stable_for * 2 + stable_for / 2,
            );
            assert_eq!(tracker.dom_stable, !changing_dom);
            assert_eq!(tracker.text_stable, changing_dom);
            tracker.observe(changed, stable_for, now + stable_for * 3);
            assert!(tracker.dom_stable && tracker.text_stable);
        }
    }
}
