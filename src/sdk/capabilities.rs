//! `/capabilities` client. Shape from `architecture.md §6`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::sdk::client::Client;
use crate::shared::error::{Error, ErrorCode};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilitiesResponse {
    pub code: String,
    pub backend: BackendFamily,
    pub artifacts: BTreeMap<String, ArtifactSupport>,
    pub wait_modes: Vec<String>,
    /// Whether this backend can expose a real display takeover when the host
    /// is started with `--takeover kasmvnc`.
    pub display_takeover: bool,
    pub ops_panel: OpsPanelSupport,
    pub profile: ProfileSupport,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub features: BTreeMap<String, FeatureSupport>,
    pub limits: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendFamily {
    pub family: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactSupport {
    pub supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub body_capture: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpsPanelSupport {
    pub supported: bool,
    pub screencast: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSupport {
    pub persistent: bool,
    pub ephemeral: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureSupport {
    pub supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
}

impl Client {
    pub async fn capabilities(&self) -> Result<CapabilitiesResponse, Error> {
        let endpoint = self.effective_endpoint().await?;
        let base = endpoint.http_base();
        let url = format!("{base}/capabilities");
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
                format!("capabilities: read response: {e}"),
            )
        })?;
        if !status.is_success() {
            if let Ok(err) = serde_json::from_slice::<Error>(&bytes) {
                return Err(err);
            }
            return Err(Error::new(
                ErrorCode::InternalError,
                format!(
                    "capabilities: status {status}; failed to decode error envelope: {}",
                    String::from_utf8_lossy(&bytes)
                ),
            ));
        }
        serde_json::from_slice::<CapabilitiesResponse>(&bytes).map_err(|e| {
            Error::new(
                ErrorCode::InternalError,
                format!("capabilities: decode response: {e}"),
            )
        })
    }

    /// Build a raw CDP request. Returns a [`crate::sdk::cdp::CdpBuilder`].
    pub fn cdp(&self, method: impl Into<String>) -> crate::sdk::cdp::CdpBuilder {
        crate::sdk::cdp::CdpBuilder::new(self.clone(), method)
    }
}
