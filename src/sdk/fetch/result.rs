//! Fetch result envelope (the JSON the agent reads on success).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::shared::artifacts::Artifact;
use crate::shared::error::{Error, ErrorCode};
use crate::shared::ids::{RequestId, TabId};

/// Result of a successful fetch. Serialized verbatim into the response
/// envelope; the protocol writer adds the outer `code: "fetch"` tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchResult {
    pub request_id: RequestId,
    pub url: String,
    pub final_url: String,
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_kind: Option<PageKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<TabId>,
    pub trace: Trace,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<Warning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rendered_html_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub console_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_state: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Trace {
    pub render_decision: RenderDecision,
    /// The `--render` mode the agent requested. Distinct from
    /// `render_decision`: a fetch with `render_mode: "auto"` that escalates
    /// to the browser reports `render_decision: "browser"` here. Agents
    /// branching on retry logic can match on this to know whether they
    /// asked for the browser explicitly or auto-mode chose it.
    #[serde(default)]
    pub render_mode: TraceRenderMode,
    /// `true` when the browser actually ran (i.e. `render_decision ==
    /// Browser`). Convenience boolean so agents don't have to compare
    /// enum strings; redundant with `render_decision`, intentionally so.
    #[serde(default)]
    pub render_used: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escalation_reason: Option<String>,
    pub main_request_observed: bool,
    pub duration_ms: u64,
    pub timeout_ms: u64,
    pub current_stage: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigation_duration_ms: Option<u64>,
    /// Browser wait mode requested for this fetch (`auto`, `load`, `idle`,
    /// `selector`, `selector_visible`, or `ms`). HTTP-only results omit it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_mode: Option<String>,
    /// Mechanical condition that allowed artifact capture to proceed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_satisfied_by: Option<String>,
    /// Whether the fetch's own Network collector observed a quiet page at
    /// capture time. Browser-path only; `None` for HTTP-only or explicit waits
    /// that do not inspect network quietness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_quiet: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dom_stable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_stable: Option<bool>,
    /// Why capture proceeded (`wait_satisfied`, `readiness_timeout`, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_reason: Option<String>,
    /// Absolute path of the cookie jar used for this fetch, if any. `None`
    /// when `--no-cookie-jar` was set or the jar could not be resolved.
    /// Exposes the implicit "GET /profile → default jar" behaviour that was
    /// previously invisible to the agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cookie_jar_file: Option<std::path::PathBuf>,
    /// Structured trace note when the cookie jar could not be resolved from
    /// `/profile` and the fetch continued without implicit profile cookies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cookie_jar_warning: Option<String>,
    /// Capture knobs that can expose secrets or PII in artifacts. Empty when
    /// the default redaction posture is in effect.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub sensitive_capture: Vec<String>,
    pub stages: Vec<TraceStage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceStage {
    pub name: String,
    pub status: TraceStageStatus,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceStageStatus {
    Ok,
    Error,
    Timeout,
    Started,
}

/// Wire-stable serialization of the `--render` mode for `Trace.render_mode`.
/// Kept separate from `pipeline::RenderMode` so the SDK exposes the trace
/// shape without callers having to depend on the pipeline module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TraceRenderMode {
    None,
    #[default]
    Auto,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RenderDecision {
    #[default]
    HttpOnly,
    Browser,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Warning {
    pub artifact: Artifact,
    pub code: crate::shared::error::ErrorCode,
    pub detail: String,
}

/// Machine-readable classification for pages that are mechanically loaded but
/// not trustworthy target content (for example Cloudflare/Turnstile walls).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageKind {
    BotWallDetected,
    SecurityChallengeDetected,
}

/// Fetch-only failure envelope data. This keeps the global `Error` contract
/// unchanged while allowing `afhttp fetch` to include the in-progress trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchError {
    pub error_code: ErrorCode,
    #[serde(rename = "error")]
    pub detail: String,
    pub retryable: bool,
    pub trace: Trace,
}

impl FetchError {
    #[must_use]
    pub fn new(error: Error, trace: Trace) -> Self {
        Self {
            error_code: error.error_code,
            detail: error.detail,
            retryable: error.retryable,
            trace,
        }
    }

    #[must_use]
    pub fn into_error(self) -> Error {
        Error {
            error_code: self.error_code,
            detail: self.detail,
            retryable: self.retryable,
        }
    }

    #[must_use]
    pub fn as_error(&self) -> Error {
        Error {
            error_code: self.error_code,
            detail: self.detail.clone(),
            retryable: self.retryable,
        }
    }
}

/// Canonical `escalation_reason` strings emitted in `Trace.escalation_reason`.
/// The wire format is `Option<String>`; these constants and constructors are
/// the single source of values so agents can match without string-parsing
/// surprise.
///
/// | Value | Meaning |
/// |---|---|
/// | `"empty_html_shell"` | HTTP returned HTML with no visible text — SPA bootstrap |
/// | `"http_status_NNN"` | HTTP returned status code NNN |
/// | `"http_failed_<code>"` | Transport-level failure, `<code>` is the ErrorCode |
pub struct EscalationReason;

impl EscalationReason {
    /// HTTP response was an empty SPA shell.
    pub const EMPTY_HTML_SHELL: &'static str = "empty_html_shell";

    /// HTTP returned status ≥ 400. Produces `"http_status_NNN"`.
    #[must_use]
    pub fn http_status(status: u16) -> String {
        format!("http_status_{status}")
    }

    /// HTTP transport error. Produces `"http_failed_<error_code>"`.
    #[must_use]
    pub fn http_failed(error_code: &str) -> String {
        format!("http_failed_{error_code}")
    }
}

impl FetchResult {
    /// Base result for `url` carrying `trace`. `final_url` defaults to `url`,
    /// `status` to 0, and every artifact/download field to empty; the pipeline
    /// fills those in as captures complete. Avoids repeating the ~15-field
    /// `None` initializer at each pipeline exit (HTTP, browser, download).
    pub(crate) fn new(request_id: RequestId, url: String, trace: Trace) -> Self {
        Self {
            request_id,
            final_url: url.clone(),
            url,
            status: 0,
            page_kind: None,
            tab_id: None,
            trace,
            warnings: Vec::new(),
            body_file: None,
            rendered_html_file: None,
            text_file: None,
            screenshot_file: None,
            network_file: None,
            console_file: None,
            observation_file: None,
            storage_file: None,
            download_file: None,
            download_bytes: None,
            download_filename: None,
            download_url: None,
            download_state: None,
        }
    }

    /// Convenience for setting an artifact path in its top-level `*_file`
    /// field. The JSON contract intentionally does not nest these paths.
    pub fn set_artifact_file(&mut self, artifact: Artifact, path: PathBuf) {
        match artifact {
            Artifact::Body => self.body_file = Some(path),
            Artifact::RenderedHtml => self.rendered_html_file = Some(path),
            Artifact::Text => self.text_file = Some(path),
            Artifact::Screenshot => self.screenshot_file = Some(path),
            Artifact::Network => self.network_file = Some(path),
            Artifact::Console => self.console_file = Some(path),
            Artifact::Observation => self.observation_file = Some(path),
            Artifact::Storage => self.storage_file = Some(path),
        }
    }

    #[must_use]
    pub fn artifact_file(&self, artifact: Artifact) -> Option<&PathBuf> {
        match artifact {
            Artifact::Body => self.body_file.as_ref(),
            Artifact::RenderedHtml => self.rendered_html_file.as_ref(),
            Artifact::Text => self.text_file.as_ref(),
            Artifact::Screenshot => self.screenshot_file.as_ref(),
            Artifact::Network => self.network_file.as_ref(),
            Artifact::Console => self.console_file.as_ref(),
            Artifact::Observation => self.observation_file.as_ref(),
            Artifact::Storage => self.storage_file.as_ref(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_success_json_is_flat_golden() {
        let mut result = FetchResult::new(
            RequestId("req-1".into()),
            "https://example.com/".into(),
            Trace {
                render_decision: RenderDecision::Browser,
                render_mode: TraceRenderMode::Always,
                render_used: true,
                escalation_reason: None,
                main_request_observed: true,
                duration_ms: 12,
                timeout_ms: 30000,
                current_stage: "complete".into(),
                navigation_duration_ms: Some(8),
                wait_mode: Some("load".into()),
                wait_satisfied_by: Some("load".into()),
                network_quiet: None,
                dom_stable: None,
                text_stable: None,
                capture_reason: Some("wait_satisfied".into()),
                cookie_jar_file: None,
                cookie_jar_warning: None,
                sensitive_capture: Vec::new(),
                stages: vec![
                    TraceStage {
                        name: "navigate".into(),
                        status: TraceStageStatus::Ok,
                        duration_ms: 8,
                    },
                    TraceStage {
                        name: "capture_text".into(),
                        status: TraceStageStatus::Ok,
                        duration_ms: 4,
                    },
                ],
            },
        );
        result.status = 200;
        for (artifact, file) in [
            (Artifact::Body, "body.html"),
            (Artifact::RenderedHtml, "rendered.html"),
            (Artifact::Text, "text.txt"),
            (Artifact::Screenshot, "page.png"),
            (Artifact::Network, "network.json"),
            (Artifact::Console, "console.json"),
            (Artifact::Observation, "observation.json"),
            (Artifact::Storage, "storage.json"),
        ] {
            result.set_artifact_file(artifact, PathBuf::from("/tmp/afhttp-out/req-1").join(file));
        }
        let mut buf = Vec::new();
        crate::shared::envelope::emit(&mut buf, "fetch", &result).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert!(
            json.get("artifacts").is_none(),
            "artifacts map must be gone"
        );
        let canonical = serde_json::to_string(&json).unwrap();
        let expected = include_str!("../../../tests/golden/fetch-success.json").trim();
        assert_eq!(canonical, expected);
    }
}
