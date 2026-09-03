//! GET /health response builder.

use std::time::Duration;

use crate::host::bootstrap::HealthPublic;
use crate::host::listener::{AppState, ProfileEntry};
use crate::sdk::cdp::ws_client::Connection;
use crate::sdk::health::{BackendError, BackendInfo, HealthResponse};
use crate::shared::error::{Error, ErrorCode};
use crate::shared::profile_snapshot::ProfileSnapshot;

pub async fn build(state: &AppState, authenticated: bool) -> HealthResponse {
    let uptime_s = state.started_at.elapsed().as_secs();
    let public_minimal = matches!(state.health_public, HealthPublic::Minimal);
    if public_minimal && state.token.is_some() && !authenticated {
        // Without the token, only minimal status is exposed.
        return HealthResponse {
            code: "health".into(),
            status: status_string(state, false),
            version: env!("CARGO_PKG_VERSION").into(),
            uptime_s,
            backend: None,
            backend_error: None,
            profile: None,
            tabs_active: 0,
            capabilities_url: None,
        };
    }

    let default_entry = state.get_profile();
    let mut backend_error = None;
    let mut tabs_active = 0;
    let backend_connected = if let Some(entry) = &default_entry {
        match probe_tabs_active(entry).await {
            Ok(count) => {
                tabs_active = count;
                true
            }
            Err(e) => {
                backend_error = Some(BackendError {
                    error_code: e.error_code,
                    error: e.detail,
                });
                false
            }
        }
    } else {
        false
    };
    let backend = default_entry.as_ref().map(|e| BackendInfo {
        family: e.handle.family.clone(),
        version: e.handle.version.clone(),
        connected: backend_connected,
    });
    let profile_snapshot = default_entry.as_ref().map(|e| ProfileSnapshot {
        kind: e.kind.clone(),
        name: if e.kind == "persistent" {
            Some(e.name.clone())
        } else {
            None
        },
        path: None,
        locked: e.kind == "persistent",
    });

    HealthResponse {
        code: "health".into(),
        status: status_string(state, backend_error.is_some()),
        version: env!("CARGO_PKG_VERSION").into(),
        uptime_s,
        backend,
        backend_error,
        profile: profile_snapshot,
        tabs_active,
        capabilities_url: Some("/capabilities".into()),
    }
}

/// A backend under load is not a backend that is gone, and this probe is the
/// only thing every reader of `/health` has to tell those two apart. Several
/// heavy pages rendering at once park CDP replies for seconds, so a budget
/// tight enough to feel like a liveness check is also tight enough to report a
/// busy browser as a broken one. These are sized for the slow case on purpose:
/// a probe that answers late costs one `/health` call, while a probe that
/// answers wrong costs the request that trusted it.
const PROBE_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const PROBE_COMMAND_TIMEOUT: Duration = Duration::from_secs(3);

/// Connect and command are timed apart so the failure names the phase that
/// actually stalled: a backend that will not accept a websocket is a different
/// problem from one that accepts it and then never answers.
async fn probe_tabs_active(entry: &ProfileEntry) -> Result<u32, Error> {
    if entry.ws_url().is_empty() {
        return Err(Error::new(
            ErrorCode::CdpUnavailable,
            "backend CDP websocket URL is empty",
        ));
    }
    let conn = tokio::time::timeout(
        PROBE_CONNECT_TIMEOUT,
        Connection::connect(entry.ws_url(), None),
    )
    .await
    .map_err(|_| {
        Error::new(
            ErrorCode::CdpTimeout,
            format!(
                "connecting to the backend CDP websocket timed out after {}ms",
                PROBE_CONNECT_TIMEOUT.as_millis()
            ),
        )
    })??;
    let result = conn
        .send_timeout(
            "Target.getTargets",
            &serde_json::json!({}),
            None,
            PROBE_COMMAND_TIMEOUT,
        )
        .await;
    conn.close();
    let value = result?;
    let count = value
        .get("targetInfos")
        .and_then(|v| v.as_array())
        .map(|targets| {
            targets
                .iter()
                .filter(|target| target.get("type").and_then(|v| v.as_str()) == Some("page"))
                .count() as u32
        })
        .unwrap_or(0);
    Ok(count)
}

fn status_string(state: &AppState, degraded: bool) -> String {
    if state.get_profile().is_none() {
        "starting".into()
    } else if degraded {
        "degraded".into()
    } else {
        "ok".into()
    }
}
