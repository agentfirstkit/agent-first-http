//! Per-fetch deadline and trace recording.
//!
//! `--timeout-ms` is the whole fetch budget, not just navigation. The recorder
//! tracks the active mechanical stage plus completed stage timings so success
//! and failure envelopes share the same trace contract.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::sdk::fetch::result::{
    RenderDecision, Trace, TraceRenderMode, TraceStage, TraceStageStatus,
};
use crate::shared::error::{Error, ErrorCode};
use crate::shared::time::duration_ms;

#[derive(Clone, Debug)]
pub(crate) struct FetchDeadline {
    started: Instant,
    timeout: Duration,
    state: Arc<Mutex<TraceState>>,
}

#[derive(Debug)]
struct TraceState {
    trace: Trace,
    active: Vec<ActiveStage>,
}

#[derive(Debug)]
struct ActiveStage {
    name: &'static str,
    started: Instant,
}

#[derive(Debug)]
struct StageToken {
    name: &'static str,
    nested: bool,
}

impl FetchDeadline {
    #[must_use]
    pub(crate) fn new(timeout: Duration, render_mode: TraceRenderMode) -> Self {
        let timeout_ms = duration_ms(timeout);
        Self {
            started: Instant::now(),
            timeout,
            state: Arc::new(Mutex::new(TraceState {
                trace: Trace {
                    render_decision: RenderDecision::HttpOnly,
                    render_mode,
                    render_used: false,
                    escalation_reason: None,
                    main_request_observed: false,
                    duration_ms: 0,
                    timeout_ms,
                    current_stage: "start".into(),
                    navigation_duration_ms: None,
                    wait_mode: None,
                    wait_satisfied_by: None,
                    network_quiet: None,
                    dom_stable: None,
                    text_stable: None,
                    capture_reason: None,
                    cookie_jar_file: None,
                    cookie_jar_warning: None,
                    sensitive_capture: Vec::new(),
                    stages: Vec::new(),
                },
                active: Vec::new(),
            })),
        }
    }

    pub(crate) fn set_stage(&self, stage: &'static str) {
        if let Ok(mut guard) = self.state.lock() {
            guard.trace.current_stage = stage.to_string();
        }
    }

    #[must_use]
    pub(crate) fn current_stage(&self) -> String {
        self.state
            .lock()
            .map(|guard| guard.trace.current_stage.clone())
            .unwrap_or_else(|_| "unknown".into())
    }

    pub(crate) fn update_trace(&self, f: impl FnOnce(&mut Trace)) {
        if let Ok(mut guard) = self.state.lock() {
            f(&mut guard.trace);
        }
    }

    #[must_use]
    pub(crate) fn snapshot(&self) -> Trace {
        self.snapshot_inner(false)
    }

    #[must_use]
    pub(crate) fn complete_trace(&self) -> Trace {
        self.snapshot_inner(true)
    }

    #[must_use]
    pub(crate) fn timeout_error(&self) -> Error {
        self.mark_current_stage(TraceStageStatus::Timeout);
        Error::new(
            ErrorCode::NavigationTimeout,
            format!(
                "fetch timed out after {}ms during {}",
                duration_ms(self.timeout),
                self.current_stage()
            ),
        )
    }

    pub(crate) fn remaining(&self, stage: &'static str) -> Result<Duration, Error> {
        self.set_stage(stage);
        self.remaining_without_stage_update()
    }

    pub(crate) fn remaining_without_stage_update(&self) -> Result<Duration, Error> {
        let elapsed = self.started.elapsed();
        if elapsed >= self.timeout {
            return Err(self.timeout_error());
        }
        Ok(self.timeout - elapsed)
    }

    pub(crate) async fn run_result<T, F>(
        &self,
        stage: &'static str,
        timeout_code: ErrorCode,
        future: F,
    ) -> Result<T, Error>
    where
        F: Future<Output = Result<T, Error>>,
    {
        let token = self.begin_stage(stage)?;
        let remaining = self.remaining_without_stage_update()?;
        match tokio::time::timeout(remaining, future).await {
            Ok(Ok(value)) => {
                self.finish_stage(token, TraceStageStatus::Ok);
                Ok(value)
            }
            Ok(Err(err)) => {
                let status = status_for_error(err.error_code);
                self.finish_stage(token, status);
                Err(err)
            }
            Err(_) => {
                self.finish_stage(token, TraceStageStatus::Timeout);
                Err(Error::new(
                    timeout_code,
                    format!(
                        "fetch timed out after {}ms during {stage}",
                        duration_ms(self.timeout)
                    ),
                ))
            }
        }
    }

    pub(crate) fn bounded_remaining(
        &self,
        stage: &'static str,
        cap: Duration,
    ) -> Result<Duration, Error> {
        Ok(self.remaining(stage)?.min(cap))
    }

    fn begin_stage(&self, stage: &'static str) -> Result<StageToken, Error> {
        let mut guard = self.state.lock().map_err(|_| {
            Error::new(
                ErrorCode::InternalError,
                "fetch trace recorder lock poisoned",
            )
        })?;
        guard.trace.current_stage = stage.to_string();
        let nested = guard
            .active
            .last()
            .is_some_and(|active| active.name == stage);
        if !nested {
            guard.active.push(ActiveStage {
                name: stage,
                started: Instant::now(),
            });
        }
        Ok(StageToken {
            name: stage,
            nested,
        })
    }

    fn finish_stage(&self, token: StageToken, status: TraceStageStatus) {
        if token.nested {
            return;
        }
        if let Ok(mut guard) = self.state.lock() {
            let Some(idx) = guard
                .active
                .iter()
                .rposition(|active| active.name == token.name)
            else {
                return;
            };
            let active = guard.active.remove(idx);
            guard.trace.current_stage = active.name.to_string();
            guard.trace.stages.push(TraceStage {
                name: active.name.to_string(),
                status,
                duration_ms: duration_ms(active.started.elapsed()),
            });
        }
    }

    fn mark_current_stage(&self, status: TraceStageStatus) {
        if let Ok(mut guard) = self.state.lock() {
            let Some(active) = guard.active.pop() else {
                return;
            };
            guard.trace.current_stage = active.name.to_string();
            guard.trace.stages.push(TraceStage {
                name: active.name.to_string(),
                status,
                duration_ms: duration_ms(active.started.elapsed()),
            });
        }
    }

    fn snapshot_inner(&self, complete: bool) -> Trace {
        let Ok(mut guard) = self.state.lock() else {
            let mut trace = fallback_trace(duration_ms(self.timeout));
            trace.duration_ms = duration_ms(self.started.elapsed());
            return trace;
        };
        guard.trace.duration_ms = duration_ms(self.started.elapsed());
        if complete {
            guard.trace.current_stage = "complete".into();
        }
        let mut trace = guard.trace.clone();
        for active in &guard.active {
            trace.stages.push(TraceStage {
                name: active.name.to_string(),
                status: TraceStageStatus::Started,
                duration_ms: duration_ms(active.started.elapsed()),
            });
        }
        trace
    }
}

fn fallback_trace(timeout_ms: u64) -> Trace {
    Trace {
        render_decision: RenderDecision::HttpOnly,
        render_mode: TraceRenderMode::Auto,
        render_used: false,
        escalation_reason: None,
        main_request_observed: false,
        duration_ms: 0,
        timeout_ms,
        current_stage: "unknown".into(),
        navigation_duration_ms: None,
        wait_mode: None,
        wait_satisfied_by: None,
        network_quiet: None,
        dom_stable: None,
        text_stable: None,
        capture_reason: None,
        cookie_jar_file: None,
        cookie_jar_warning: None,
        sensitive_capture: Vec::new(),
        stages: Vec::new(),
    }
}

fn status_for_error(code: ErrorCode) -> TraceStageStatus {
    match code {
        ErrorCode::NavigationTimeout
        | ErrorCode::CdpTimeout
        | ErrorCode::ArtifactCaptureTimeout
        | ErrorCode::ReadinessTimeout => TraceStageStatus::Timeout,
        _ => TraceStageStatus::Error,
    }
}
