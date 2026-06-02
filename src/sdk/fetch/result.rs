//! Fetch result envelope (the JSON the agent reads on success).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::shared::artifacts::Artifact;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigation_duration_ms: Option<u64>,
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
        let mut result = FetchResult {
            request_id: RequestId("req-1".into()),
            url: "https://example.com/".into(),
            final_url: "https://example.com/".into(),
            status: 200,
            tab_id: None,
            trace: Trace {
                render_decision: RenderDecision::Browser,
                render_mode: TraceRenderMode::Always,
                render_used: true,
                escalation_reason: None,
                main_request_observed: true,
                duration_ms: 12,
                navigation_duration_ms: Some(8),
                cookie_jar_file: None,
                cookie_jar_warning: None,
                sensitive_capture: Vec::new(),
            },
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
        };
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
