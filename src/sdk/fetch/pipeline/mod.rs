//! Render-mode state machine.
//!
//! `RenderMode::None` uses the HTTP fast path (no browser). `Auto` tries
//! HTTP first and escalates to the browser on connect failure / 5xx;
//! `Always` skips the entire HTTP attempt.

mod browser;
mod cookie_jar_resolve;
mod http_only;
mod request_opts;

use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::sdk::fetch::deadline::FetchDeadline;
use crate::sdk::fetch::result::{EscalationReason, FetchResult, RenderDecision};
use crate::sdk::fetch::FetchBuilder;
use crate::shared::artifacts::{Artifact, ArtifactPaths};
use crate::shared::error::{Error, ErrorCode};
use crate::shared::ids::RequestId;

/// `--render` modes from `architecture.md §5`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderMode {
    /// HTTP only; never start a browser.
    None,
    /// HTTP first; escalate to browser on connect/5xx/non-HTML.
    #[default]
    Auto,
    /// Always use the browser.
    Always,
}

impl RenderMode {
    pub fn parse(s: &str) -> Result<Self, Error> {
        match s {
            "none" => Ok(Self::None),
            "auto" => Ok(Self::Auto),
            "always" => Ok(Self::Always),
            other => Err(Error::new(
                ErrorCode::InvalidArgument,
                format!("--render: unknown mode {other:?}; expected none|auto|always"),
            )),
        }
    }

    pub(crate) fn as_trace(self) -> crate::sdk::fetch::result::TraceRenderMode {
        match self {
            Self::None => crate::sdk::fetch::result::TraceRenderMode::None,
            Self::Auto => crate::sdk::fetch::result::TraceRenderMode::Auto,
            Self::Always => crate::sdk::fetch::result::TraceRenderMode::Always,
        }
    }
}

/// `--network-bodies` modes from `architecture.md §8`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkBodies {
    #[default]
    Off,
    Xhr,
    All,
}

impl NetworkBodies {
    pub fn parse(s: &str) -> Result<Self, Error> {
        match s {
            "off" => Ok(Self::Off),
            "xhr" => Ok(Self::Xhr),
            "all" => Ok(Self::All),
            other => Err(Error::new(
                ErrorCode::InvalidArgument,
                format!("--network-bodies: unknown {other:?}; expected off|xhr|all"),
            )),
        }
    }
}

pub(crate) async fn execute(
    mut builder: FetchBuilder,
    deadline: FetchDeadline,
) -> Result<FetchResult, Error> {
    deadline.update_trace(|trace| {
        trace.render_mode = builder.render.as_trace();
        trace.sensitive_capture = sensitive_capture(&builder);
    });
    deadline
        .run_result(
            "resolve_cookie_jar",
            ErrorCode::NavigationTimeout,
            cookie_jar_resolve::resolve_cookie_jar_path(&mut builder),
        )
        .await?;
    deadline.update_trace(|trace| {
        trace.cookie_jar_file = builder.cookie_jar.path.clone();
        trace.cookie_jar_warning = builder.cookie_jar.warning.clone();
    });
    let request_options = deadline
        .run_result(
            "prepare_request",
            ErrorCode::NavigationTimeout,
            std::future::ready(request_opts::PreparedRequestOptions::from_builder(&builder)),
        )
        .await?;
    let request_id = RequestId::new_v4();
    let out_root = builder.out_dir.clone().unwrap_or_else(default_out_dir);
    let paths = ArtifactPaths::new(out_root, &request_id);

    let start = Instant::now();
    match builder.render {
        RenderMode::None => {
            deadline.update_trace(|trace| {
                trace.render_decision = RenderDecision::HttpOnly;
                trace.render_used = false;
            });
            browser::reject_http_only_evaluate(&request_options)?;
            http_only::http_only(
                &builder,
                &request_options,
                request_id,
                &paths,
                start,
                None,
                &deadline,
            )
            .await
            .map(|o| o.result)
        }
        RenderMode::Auto => {
            deadline.update_trace(|trace| {
                trace.render_decision = RenderDecision::HttpOnly;
                trace.render_used = false;
            });
            match http_only::http_only(
                &builder,
                &request_options,
                request_id.clone(),
                &paths,
                start,
                None,
                &deadline,
            )
            .await
            {
                Ok(o) if o.result.status < 400 => {
                    if let Some(classification) =
                        http_only::classify_http_body(&o.body_bytes, o.content_type.as_deref())
                    {
                        let reason = classification.code.as_str().to_string();
                        deadline.update_trace(|trace| {
                            trace.render_decision = RenderDecision::Browser;
                            trace.render_used = true;
                            trace.escalation_reason = Some(reason.clone());
                        });
                        return browser::browser_path(
                            builder,
                            request_options,
                            request_id,
                            paths,
                            start,
                            Some(reason),
                            &deadline,
                        )
                        .await;
                    }
                    if http_only::looks_like_empty_html_shell(
                        &o.body_bytes,
                        o.content_type.as_deref(),
                    ) {
                        deadline.update_trace(|trace| {
                            trace.render_decision = RenderDecision::Browser;
                            trace.render_used = true;
                            trace.escalation_reason =
                                Some(EscalationReason::EMPTY_HTML_SHELL.to_string());
                        });
                        return browser::browser_path(
                            builder,
                            request_options,
                            request_id,
                            paths,
                            start,
                            Some(EscalationReason::EMPTY_HTML_SHELL.to_string()),
                            &deadline,
                        )
                        .await;
                    }
                    if builder.want.contains(&Artifact::Content)
                        || builder.want.contains(&Artifact::ContentJson)
                    {
                        let reason = "requested_content_artifact".to_string();
                        deadline.update_trace(|trace| {
                            trace.render_decision = RenderDecision::Browser;
                            trace.render_used = true;
                            trace.escalation_reason = Some(reason.clone());
                        });
                        return browser::browser_path(
                            builder,
                            request_options,
                            request_id,
                            paths,
                            start,
                            Some(reason),
                            &deadline,
                        )
                        .await;
                    }
                    browser::reject_http_only_evaluate(&request_options)?;
                    Ok(o.result)
                }
                Err(e) if e.error_code == ErrorCode::InvalidArgument => Err(e),
                outcome => {
                    let reason = match &outcome {
                        Ok(o) => EscalationReason::http_status(o.result.status),
                        Err(e) => EscalationReason::http_failed(e.error_code.as_str()),
                    };
                    deadline.update_trace(|trace| {
                        trace.render_decision = RenderDecision::Browser;
                        trace.render_used = true;
                        trace.escalation_reason = Some(reason.clone());
                    });
                    browser::browser_path(
                        builder,
                        request_options,
                        request_id,
                        paths,
                        start,
                        Some(reason),
                        &deadline,
                    )
                    .await
                }
            }
        }
        RenderMode::Always => {
            deadline.update_trace(|trace| {
                trace.render_decision = RenderDecision::Browser;
                trace.render_used = true;
            });
            browser::browser_path(
                builder,
                request_options,
                request_id,
                paths,
                start,
                None,
                &deadline,
            )
            .await
        }
    }
}

fn default_out_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("afhttp-out")
}

pub(super) fn sensitive_capture(builder: &FetchBuilder) -> Vec<String> {
    let mut risks = Vec::new();
    if !builder.network.redact {
        risks.push("network_redact_off_may_expose_tokens_or_pii".to_string());
    }
    if builder.network.capture_ws {
        risks.push("capture_ws_may_expose_tokens_or_pii".to_string());
    }
    if builder.network.capture_sse {
        risks.push("capture_sse_may_expose_tokens_or_pii".to_string());
    }
    risks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_mode_parses() {
        assert_eq!(RenderMode::parse("none").unwrap(), RenderMode::None);
        assert_eq!(RenderMode::parse("auto").unwrap(), RenderMode::Auto);
        assert_eq!(RenderMode::parse("always").unwrap(), RenderMode::Always);
        assert!(RenderMode::parse("rocket").is_err());
    }

    #[test]
    fn network_bodies_parses() {
        assert_eq!(NetworkBodies::parse("off").unwrap(), NetworkBodies::Off);
        assert_eq!(NetworkBodies::parse("xhr").unwrap(), NetworkBodies::Xhr);
        assert_eq!(NetworkBodies::parse("all").unwrap(), NetworkBodies::All);
        assert!(NetworkBodies::parse("some").is_err());
    }

    #[test]
    fn default_out_dir_is_under_system_temp() {
        let dir = default_out_dir();
        assert_eq!(dir, std::env::temp_dir().join("afhttp-out"));
    }
}
