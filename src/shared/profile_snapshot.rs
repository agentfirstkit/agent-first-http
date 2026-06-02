//! Canonical profile-state snapshot shared by the SDK and the wire layer.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Snapshot of the host's active profile, shared by `GET /profile` (full),
/// `GET /health` (summary without path), and the SDK's `Client::profile_info`.
///
/// Fields that are not meaningful for a given endpoint are omitted during
/// serialization — `path` in the health response, `locked` when false.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSnapshot {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Absolute profile directory on the host filesystem. Present in
    /// `GET /profile`; absent in `GET /health`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Whether the profile lock is held. `false` for ephemeral hosts.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
}

impl ProfileSnapshot {
    /// `true` when the host is running with `--profile <name>`.
    #[must_use]
    pub fn is_persistent(&self) -> bool {
        self.kind == "persistent"
    }

    /// Default cookie-jar path for this profile. `None` when `path` is
    /// absent (e.g. a health-response snapshot with no profile path).
    #[must_use]
    pub fn canonical_cookie_jar(&self) -> Option<PathBuf> {
        self.path.as_ref().map(|p| p.join("cookies.jar.json"))
    }
}
