//! Structured error contract for `afhttp`.
//!
//! Every failure surface — fetch, host, profile, CDP — flows through this
//! enum. The `ErrorCode` enum maps 1:1 to `architecture.md §11`; serialization
//! is stable across versions and is part of the public contract.

use serde::{Deserialize, Serialize};

/// Stable error-code enum from `architecture.md §11`.
///
/// Variants serialize to their snake_case form (e.g. `ErrorCode::NavigationTimeout`
/// → `"navigation_timeout"`). Agents match on this; they do **not** parse
/// `Error::detail`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NavigationTimeout,
    RenderUnavailable,
    HostUnreachable,
    DnsResolutionFailed,
    TargetUnreachable,
    TlsError,
    TabCrashed,
    ProfileLocked,
    BrowserLaunchFailed,
    CdpUnavailable,
    CdpError,
    CdpTimeout,
    WaitSelectorUnmatched,
    BackendUnsupported,
    ArtifactCaptureFailed,
    ArtifactCaptureTimeout,
    ArtifactEmpty,
    ArtifactTiny,
    BotWallDetected,
    SecurityChallengeDetected,
    NetworkNotIdle,
    NavigationStale,
    ObservationEmpty,
    PendingXhrAtCapture,
    ReadinessTimeout,
    NetworkBodyTruncated,
    ProfileNotFound,
    ProfileDeleteLocked,
    ProfileInvalidName,
    ProfileRootUnavailable,
    InvalidArgument,
    InvalidEndpoint,
    IoError,
    InternalError,
}

impl ErrorCode {
    /// Default retryability per `architecture.md §11`. Some codes (`cdp_error`,
    /// `artifact_capture_failed`, `browser_launch_failed`) are "depends on
    /// detail"; we report a conservative default and let callers override via
    /// [`Error::with_retryable`] when they know better.
    #[must_use]
    pub const fn retryable_default(self) -> bool {
        match self {
            Self::NavigationTimeout
            | Self::HostUnreachable
            | Self::DnsResolutionFailed
            | Self::TargetUnreachable
            | Self::TabCrashed
            | Self::CdpTimeout
            | Self::IoError => true,

            Self::RenderUnavailable
            | Self::TlsError
            | Self::ProfileLocked
            | Self::CdpUnavailable
            | Self::WaitSelectorUnmatched
            | Self::BackendUnsupported
            | Self::ArtifactCaptureTimeout
            | Self::ArtifactEmpty
            | Self::ArtifactTiny
            | Self::BotWallDetected
            | Self::SecurityChallengeDetected
            | Self::NetworkNotIdle
            | Self::NavigationStale
            | Self::ObservationEmpty
            | Self::PendingXhrAtCapture
            | Self::ReadinessTimeout
            | Self::NetworkBodyTruncated
            | Self::ProfileNotFound
            | Self::ProfileDeleteLocked
            | Self::ProfileInvalidName
            | Self::InvalidArgument
            | Self::InvalidEndpoint => false,

            // "Depends" cases — conservative default is false; callers tune.
            Self::BrowserLaunchFailed
            | Self::CdpError
            | Self::ArtifactCaptureFailed
            | Self::ProfileRootUnavailable
            | Self::InternalError => false,
        }
    }

    /// Stable snake_case string. Mirrors serde output; useful when emitting
    /// trace logs from the protocol writer without re-serializing.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NavigationTimeout => "navigation_timeout",
            Self::RenderUnavailable => "render_unavailable",
            Self::HostUnreachable => "host_unreachable",
            Self::DnsResolutionFailed => "dns_resolution_failed",
            Self::TargetUnreachable => "target_unreachable",
            Self::TlsError => "tls_error",
            Self::TabCrashed => "tab_crashed",
            Self::ProfileLocked => "profile_locked",
            Self::BrowserLaunchFailed => "browser_launch_failed",
            Self::CdpUnavailable => "cdp_unavailable",
            Self::CdpError => "cdp_error",
            Self::CdpTimeout => "cdp_timeout",
            Self::WaitSelectorUnmatched => "wait_selector_unmatched",
            Self::BackendUnsupported => "backend_unsupported",
            Self::ArtifactCaptureFailed => "artifact_capture_failed",
            Self::ArtifactCaptureTimeout => "artifact_capture_timeout",
            Self::ArtifactEmpty => "artifact_empty",
            Self::ArtifactTiny => "artifact_tiny",
            Self::BotWallDetected => "bot_wall_detected",
            Self::SecurityChallengeDetected => "security_challenge_detected",
            Self::NetworkNotIdle => "network_not_idle",
            Self::NavigationStale => "navigation_stale",
            Self::ObservationEmpty => "observation_empty",
            Self::PendingXhrAtCapture => "pending_xhr_at_capture",
            Self::ReadinessTimeout => "readiness_timeout",
            Self::NetworkBodyTruncated => "network_body_truncated",
            Self::ProfileNotFound => "profile_not_found",
            Self::ProfileDeleteLocked => "profile_delete_locked",
            Self::ProfileInvalidName => "profile_invalid_name",
            Self::ProfileRootUnavailable => "profile_root_unavailable",
            Self::InvalidArgument => "invalid_argument",
            Self::InvalidEndpoint => "invalid_endpoint",
            Self::IoError => "io_error",
            Self::InternalError => "internal_error",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The structured error every `afhttp` failure path produces.
///
/// Serializes as `{"code":"error","error_code":"<snake>","error":"...","retryable":bool}`
/// when wrapped by the protocol envelope; the envelope adds the outer `code`
/// discriminator. `Error` itself only carries the inner fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Error {
    pub error_code: ErrorCode,
    #[serde(rename = "error")]
    pub detail: String,
    pub retryable: bool,
}

impl Error {
    #[must_use]
    pub fn new(error_code: ErrorCode, detail: impl Into<String>) -> Self {
        Self {
            error_code,
            detail: detail.into(),
            retryable: error_code.retryable_default(),
        }
    }

    #[must_use]
    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.error_code, self.detail)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::new(ErrorCode::IoError, err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_serializes_to_snake_case() {
        let cases = [
            (ErrorCode::NavigationTimeout, "navigation_timeout"),
            (ErrorCode::RenderUnavailable, "render_unavailable"),
            (ErrorCode::HostUnreachable, "host_unreachable"),
            (ErrorCode::DnsResolutionFailed, "dns_resolution_failed"),
            (ErrorCode::TargetUnreachable, "target_unreachable"),
            (ErrorCode::TlsError, "tls_error"),
            (ErrorCode::TabCrashed, "tab_crashed"),
            (ErrorCode::ProfileLocked, "profile_locked"),
            (ErrorCode::BrowserLaunchFailed, "browser_launch_failed"),
            (ErrorCode::CdpUnavailable, "cdp_unavailable"),
            (ErrorCode::CdpError, "cdp_error"),
            (ErrorCode::CdpTimeout, "cdp_timeout"),
            (ErrorCode::WaitSelectorUnmatched, "wait_selector_unmatched"),
            (ErrorCode::BackendUnsupported, "backend_unsupported"),
            (ErrorCode::ArtifactCaptureFailed, "artifact_capture_failed"),
            (
                ErrorCode::ArtifactCaptureTimeout,
                "artifact_capture_timeout",
            ),
            (ErrorCode::ArtifactEmpty, "artifact_empty"),
            (ErrorCode::ArtifactTiny, "artifact_tiny"),
            (ErrorCode::BotWallDetected, "bot_wall_detected"),
            (
                ErrorCode::SecurityChallengeDetected,
                "security_challenge_detected",
            ),
            (ErrorCode::NetworkNotIdle, "network_not_idle"),
            (ErrorCode::NavigationStale, "navigation_stale"),
            (ErrorCode::ObservationEmpty, "observation_empty"),
            (ErrorCode::PendingXhrAtCapture, "pending_xhr_at_capture"),
            (ErrorCode::ReadinessTimeout, "readiness_timeout"),
            (ErrorCode::NetworkBodyTruncated, "network_body_truncated"),
            (ErrorCode::ProfileNotFound, "profile_not_found"),
            (ErrorCode::ProfileDeleteLocked, "profile_delete_locked"),
            (ErrorCode::ProfileInvalidName, "profile_invalid_name"),
            (
                ErrorCode::ProfileRootUnavailable,
                "profile_root_unavailable",
            ),
            (ErrorCode::InvalidArgument, "invalid_argument"),
            (ErrorCode::InvalidEndpoint, "invalid_endpoint"),
            (ErrorCode::IoError, "io_error"),
            (ErrorCode::InternalError, "internal_error"),
        ];
        for (code, expected) in cases {
            assert_eq!(code.as_str(), expected);
            let json = serde_json::to_string(&code).unwrap_or_default();
            assert_eq!(json, format!("\"{expected}\""), "{code:?}");
        }
    }

    #[test]
    fn retryable_defaults_match_spec_table() {
        assert!(ErrorCode::NavigationTimeout.retryable_default());
        assert!(ErrorCode::HostUnreachable.retryable_default());
        assert!(ErrorCode::DnsResolutionFailed.retryable_default());
        assert!(ErrorCode::TargetUnreachable.retryable_default());
        assert!(ErrorCode::TabCrashed.retryable_default());
        assert!(ErrorCode::CdpTimeout.retryable_default());

        assert!(!ErrorCode::TlsError.retryable_default());
        assert!(!ErrorCode::RenderUnavailable.retryable_default());
        assert!(!ErrorCode::ProfileLocked.retryable_default());
        assert!(!ErrorCode::CdpUnavailable.retryable_default());
        assert!(!ErrorCode::BackendUnsupported.retryable_default());
        assert!(!ErrorCode::WaitSelectorUnmatched.retryable_default());
        assert!(!ErrorCode::ProfileNotFound.retryable_default());
        assert!(!ErrorCode::ProfileDeleteLocked.retryable_default());
        assert!(!ErrorCode::ProfileInvalidName.retryable_default());
    }

    #[test]
    fn with_retryable_overrides_default() {
        let err = Error::new(ErrorCode::CdpError, "boom").with_retryable(true);
        assert!(err.retryable);
        assert_eq!(err.error_code, ErrorCode::CdpError);
    }

    #[test]
    fn error_serializes_with_renamed_detail_field() {
        let err = Error::new(ErrorCode::NavigationTimeout, "page never loaded");
        let json = serde_json::to_value(&err).unwrap_or(serde_json::Value::Null);
        assert_eq!(json["error_code"], "navigation_timeout");
        assert_eq!(json["error"], "page never loaded");
        assert_eq!(json["retryable"], true);
    }

    #[test]
    fn io_error_maps_to_io_error_code() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "nope");
        let err: Error = io_err.into();
        assert_eq!(err.error_code, ErrorCode::IoError);
        assert!(err.retryable);
    }
}
