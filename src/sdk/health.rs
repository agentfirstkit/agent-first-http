//! `/health` client. Hits the host's HTTP endpoint and returns the parsed
//! shape from `architecture.md §6`.

use serde::{Deserialize, Serialize};

use crate::sdk::client::Client;
use crate::shared::error::{Error, ErrorCode};
use crate::shared::profile_snapshot::ProfileSnapshot;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub code: String,
    pub status: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    /// Live takeover checks; absent on older hosts. Capabilities describe
    /// configuration support and must not be used as evidence of liveness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub takeover: Option<TakeoverHealth>,
    pub uptime_s: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<BackendInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_error: Option<BackendError>,
    /// Snapshot of the active/default profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<ProfileSnapshot>,
    #[serde(default)]
    pub tabs_active: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TakeoverHealth {
    pub configured: bool,
    /// None when the host does not supervise this process.
    pub browser_running: Option<bool>,
    pub cdp_connected: bool,
    pub display_connected: bool,
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendInfo {
    pub family: String,
    pub version: String,
    pub connected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendError {
    pub error_code: ErrorCode,
    pub error: String,
}

impl Client {
    /// Fetch `/health`. Returns an error wrapped with
    /// `ErrorCode::HostUnreachable` on transport failure.
    pub async fn health(&self) -> Result<HealthResponse, Error> {
        let endpoint = self.effective_endpoint().await?;
        let base = endpoint.http_base();
        let url = format!("{base}/health");
        let mut req = self.http().get(&url);
        if let Some(token) = self.token() {
            req = req.bearer_auth(token);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::new(ErrorCode::HostUnreachable, format!("GET {url}: {e}")))?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(|e| {
            Error::new(
                ErrorCode::InternalError,
                format!("health: read response: {e}"),
            )
        })?;
        if !status.is_success() {
            if let Ok(err) = crate::shared::events::decode_error(&bytes) {
                return Err(err);
            }
            return Err(Error::new(
                ErrorCode::InternalError,
                format!(
                    "health: status {status}; failed to decode error envelope: {}",
                    String::from_utf8_lossy(&bytes)
                ),
            ));
        }
        crate::shared::events::decode_result(&bytes)
    }
}
