//! Client helpers for short-lived human-takeover handoff URLs.

use serde::{Deserialize, Serialize};

use crate::sdk::client::Client;
use crate::shared::error::{Error, ErrorCode};

#[derive(Debug, Clone, Serialize)]
struct TakeoverHandoffRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl_s: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tab_id: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TakeoverHandoffResponse {
    pub takeover_url: String,
    pub takeover_url_expires_at_rfc3339: String,
    pub takeover_url_ttl_s: u64,
    pub takeover_url_scope: String,
}

impl Client {
    /// Mint a short-lived URL capability for `/takeover/*`.
    pub async fn takeover_handoff(
        &self,
        ttl_s: Option<u64>,
        tab_id: Option<&str>,
    ) -> Result<TakeoverHandoffResponse, Error> {
        let endpoint = self.effective_endpoint().await?;
        let base = endpoint.http_base();
        let url = format!("{base}/takeover/handoff");
        let body = TakeoverHandoffRequest { ttl_s, tab_id };
        let mut req = self.http().post(&url).json(&body);
        if let Some(token) = self.token() {
            req = req.bearer_auth(token);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::new(ErrorCode::HostUnreachable, format!("POST {url}: {e}")))?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(|e| {
            Error::new(
                ErrorCode::InternalError,
                format!("takeover_handoff: read response: {e}"),
            )
        })?;
        if !status.is_success() {
            if let Ok(err) = serde_json::from_slice::<Error>(&bytes) {
                return Err(err);
            }
            return Err(Error::new(
                ErrorCode::InternalError,
                format!(
                    "takeover_handoff: status {status}; failed to decode error envelope: {}",
                    String::from_utf8_lossy(&bytes)
                ),
            ));
        }
        serde_json::from_slice::<TakeoverHandoffResponse>(&bytes).map_err(|e| {
            Error::new(
                ErrorCode::InternalError,
                format!("takeover_handoff: decode response: {e}"),
            )
        })
    }
}
