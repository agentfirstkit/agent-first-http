//! Client helpers for human-takeover URLs and process-bound UI leases.

use std::time::Duration;

use reqwest::StatusCode;
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
    #[serde(alias = "takeover_url")]
    pub takeover_url_secret: String,
    pub takeover_url_expires_at_rfc3339: String,
    pub takeover_url_ttl_s: u64,
    pub takeover_url_scope: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct TakeoverUiSessionResponse {
    takeover_url_secret: String,
    takeover_session_idle_timeout_s: u64,
    takeover_url_scope: String,
}

/// The private upstream credential behind one AFUI delivery.
///
/// AFUI owns the window, public Link page, attention policy, and registered
/// session. This lease only keeps the separately hosted takeover panel alive
/// while that delivery exists. Its short idle timeout is crash cleanup, not a
/// user-visible UI expiry.
pub struct TakeoverUiSession {
    client: reqwest::Client,
    maintenance_url: url::Url,
    handoff_secret: HandoffSecret,
    takeover_url_secret: String,
    keep_alive_every: Duration,
}

impl Client {
    // Also protects drivers connected to older hosts that only report backend
    // connectivity. New hosts independently enforce display/process checks.
    async fn check_takeover_browser(&self) -> Result<(), Error> {
        let health = match self.health().await {
            Ok(health) => health,
            Err(health_error) => {
                // Hosts may explicitly disable /health. Probe CDP directly
                // in that case; the host still gates its display endpoints.
                let probe = async {
                    let conn = self.cdp_connection().await?;
                    conn.send_timeout(
                        "Target.getTargets",
                        &serde_json::json!({}),
                        None,
                        Duration::from_secs(3),
                    )
                    .await?;
                    Ok::<(), Error>(())
                };
                return tokio::time::timeout(Duration::from_secs(5), probe)
                    .await
                    .map_err(|_| {
                        Error::new(
                            ErrorCode::CdpTimeout,
                            format!(
                                "takeover browser probe timed out; health unavailable: {}",
                                health_error.detail
                            ),
                        )
                    })?
                    .map_err(|error| {
                        Error::new(
                            error.error_code,
                            format!(
                                "takeover browser unavailable: {}; health unavailable: {}",
                                error.detail, health_error.detail
                            ),
                        )
                    });
            }
        };
        if let Some(takeover) = health.takeover {
            if !takeover.ready {
                return Err(Error::new(
                    ErrorCode::CdpUnavailable,
                    takeover
                        .reason
                        .unwrap_or_else(|| "takeover is unavailable".into()),
                ));
            }
        } else if !health.backend.is_some_and(|backend| backend.connected) {
            return Err(Error::new(
                ErrorCode::CdpUnavailable,
                "takeover cannot open: browser CDP is unavailable; a running host/VNC does not imply a live browser; preserve the profile",
            ));
        }
        Ok(())
    }

    /// Mint a short-lived URL capability for `/takeover/*`.
    pub async fn takeover_handoff(
        &self,
        ttl_s: Option<u64>,
        tab_id: Option<&str>,
    ) -> Result<TakeoverHandoffResponse, Error> {
        self.check_takeover_browser().await?;
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
            if let Ok(err) = crate::shared::events::decode_error(&bytes) {
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
        crate::shared::events::decode_result(&bytes)
    }

    /// Mint a process-bound upstream credential for an AFUI delivery.
    pub async fn takeover_ui_session(&self) -> Result<TakeoverUiSession, Error> {
        self.check_takeover_browser().await?;
        let endpoint = self.effective_endpoint().await?;
        let url = format!("{}/takeover/ui-session", endpoint.http_base());
        let mut request = self.http().post(&url);
        if let Some(token) = self.token() {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.map_err(|error| {
            Error::new(ErrorCode::HostUnreachable, format!("POST {url}: {error}"))
        })?;
        let response = decode_ui_session_response(response, "create takeover UI session").await?;
        TakeoverUiSession::from_response(self.http().clone(), response)
    }
}

impl TakeoverUiSession {
    /// Exchange an already-minted panel handoff for a process-bound UI lease.
    ///
    /// The existing handoff authorizes only this exchange; callers do not need
    /// the host's long-lived API bearer merely to put a URL they already hold
    /// into AFUI's delivery lifecycle.
    pub async fn exchange(takeover_url_secret: &str) -> Result<Self, Error> {
        crate::sdk::client::ensure_rustls_provider();
        let (maintenance_url, handoff_secret) = maintenance_url(takeover_url_secret)?;
        // Bounded: a renewal stuck on a host that accepted and went quiet
        // would outlive the lease it was meant to keep, and a revoke on the
        // way out would hold shutdown.
        let client = reqwest::Client::builder()
            .no_proxy()
            .user_agent(concat!("afhttp/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|error| {
                Error::new(
                    ErrorCode::InternalError,
                    format!("build takeover UI session client: {error}"),
                )
            })?;
        let response = client
            .post(maintenance_url.clone())
            .header(
                reqwest::header::AUTHORIZATION,
                handoff_authorization(&handoff_secret),
            )
            .send()
            .await
            .map_err(|error| {
                Error::new(
                    ErrorCode::HostUnreachable,
                    format!("exchange takeover handoff for UI session: {error}"),
                )
            })?;
        let response =
            decode_ui_session_response(response, "exchange takeover handoff for UI session")
                .await?;
        Self::from_response(client, response)
    }

    fn from_response(
        client: reqwest::Client,
        response: TakeoverUiSessionResponse,
    ) -> Result<Self, Error> {
        if response.takeover_url_scope != "takeover_ui_session" {
            return Err(Error::new(
                ErrorCode::InternalError,
                format!(
                    "takeover UI session: host returned scope {:?}, expected \"takeover_ui_session\"",
                    response.takeover_url_scope
                ),
            ));
        }
        if response.takeover_session_idle_timeout_s < 3 {
            return Err(Error::new(
                ErrorCode::InternalError,
                "takeover UI session: host returned an idle timeout shorter than 3 seconds",
            ));
        }
        let (maintenance_url, handoff_secret) = maintenance_url(&response.takeover_url_secret)?;
        Ok(Self {
            client,
            maintenance_url,
            handoff_secret,
            takeover_url_secret: response.takeover_url_secret,
            keep_alive_every: Duration::from_secs(response.takeover_session_idle_timeout_s / 3),
        })
    }

    /// The upstream URL AFUI keeps private behind its own delivery.
    #[must_use]
    pub fn takeover_url_secret(&self) -> &str {
        &self.takeover_url_secret
    }

    /// Renew until this future is cancelled or the host can no longer do so.
    pub async fn keep_alive(&self) -> Result<(), Error> {
        loop {
            tokio::time::sleep(self.keep_alive_every).await;
            let response = self
                .client
                .put(self.maintenance_url.clone())
                .header(
                    reqwest::header::AUTHORIZATION,
                    handoff_authorization(&self.handoff_secret),
                )
                .send()
                .await
                .map_err(|error| {
                    Error::new(
                        ErrorCode::HostUnreachable,
                        format!("renew takeover UI session: {error}"),
                    )
                })?;
            if !response.status().is_success() {
                return Err(response_error(response, "renew takeover UI session").await);
            }
        }
    }

    /// Revoke the upstream credential. Already gone is the same outcome.
    pub async fn revoke(&self) -> Result<bool, Error> {
        let response = self
            .client
            .delete(self.maintenance_url.clone())
            .header(
                reqwest::header::AUTHORIZATION,
                handoff_authorization(&self.handoff_secret),
            )
            .send()
            .await
            .map_err(|error| {
                Error::new(
                    ErrorCode::HostUnreachable,
                    format!("revoke takeover UI session: {error}"),
                )
            })?;
        match response.status() {
            StatusCode::NO_CONTENT => Ok(true),
            StatusCode::NOT_FOUND | StatusCode::UNAUTHORIZED => Ok(false),
            _ => Err(response_error(response, "revoke takeover UI session").await),
        }
    }
}

fn maintenance_url(takeover_url_secret: &str) -> Result<(url::Url, HandoffSecret), Error> {
    let mut url = url::Url::parse(takeover_url_secret).map_err(|error| {
        Error::new(
            ErrorCode::InvalidEndpoint,
            format!("takeover UI session: invalid takeover URL: {error}"),
        )
    })?;
    let secret = url
        .query_pairs()
        .find(|(key, _)| key == "handoff_secret")
        .map(|(_, value)| value.into_owned())
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| {
            Error::new(
                ErrorCode::InvalidEndpoint,
                "takeover UI session: URL has no valid handoff_secret",
            )
        })?;
    url.set_path("/takeover/ui-session");
    url.set_query(None);
    url.set_fragment(None);
    Ok((url, HandoffSecret(secret)))
}

/// The handoff credential, kept out of `Debug` so a logged session never
/// carries it.
struct HandoffSecret(String);

fn handoff_authorization(secret: &HandoffSecret) -> String {
    format!("Handoff {}", secret.0)
}

async fn decode_ui_session_response(
    response: reqwest::Response,
    operation: &str,
) -> Result<TakeoverUiSessionResponse, Error> {
    let status = response.status();
    let bytes = response.bytes().await.map_err(|error| {
        Error::new(
            ErrorCode::InternalError,
            format!("{operation}: read response: {error}"),
        )
    })?;
    if !status.is_success() {
        if let Ok(error) = crate::shared::events::decode_error(&bytes) {
            return Err(error);
        }
        return Err(Error::new(
            ErrorCode::InternalError,
            format!(
                "{operation}: status {status}; failed to decode error envelope: {}",
                String::from_utf8_lossy(&bytes)
            ),
        ));
    }
    crate::shared::events::decode_result(&bytes)
}

async fn response_error(response: reqwest::Response, operation: &str) -> Error {
    let status = response.status();
    match response.bytes().await {
        Ok(bytes) => crate::shared::events::decode_error(&bytes).unwrap_or_else(|_| {
            Error::new(
                ErrorCode::InternalError,
                format!("{operation}: host returned {status}"),
            )
        }),
        Err(error) => Error::new(
            ErrorCode::InternalError,
            format!("{operation}: host returned {status}; read response: {error}"),
        ),
    }
}
