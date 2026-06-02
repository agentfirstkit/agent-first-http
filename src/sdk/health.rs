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
            if let Ok(err) = serde_json::from_slice::<Error>(&bytes) {
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
        serde_json::from_slice::<HealthResponse>(&bytes).map_err(|e| {
            Error::new(
                ErrorCode::InternalError,
                format!("health: decode response: {e}"),
            )
        })
    }
}
