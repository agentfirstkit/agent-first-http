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
    let backend_connected = if let Some(entry) = default_entry {
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
    let backend = default_entry.map(|e| BackendInfo {
        family: e.handle.family.clone(),
        version: e.handle.version.clone(),
        connected: backend_connected,
    });
    let profile_snapshot = default_entry.map(|e| ProfileSnapshot {
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

async fn probe_tabs_active(entry: &ProfileEntry) -> Result<u32, Error> {
    if entry.ws_url().is_empty() {
        return Err(Error::new(
            ErrorCode::CdpUnavailable,
            "backend CDP websocket URL is empty",
        ));
    }
    let fut = async {
        let conn = Connection::connect(entry.ws_url(), None).await?;
        let result = conn
            .send("Target.getTargets", &serde_json::json!({}), None)
            .await;
        conn.close();
        result
    };
    let value = tokio::time::timeout(Duration::from_millis(750), fut)
        .await
        .map_err(|_| Error::new(ErrorCode::CdpTimeout, "Target.getTargets timed out"))??;
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
    if state.profile.is_none() {
        "starting".into()
    } else if degraded {
        "degraded".into()
    } else {
        "ok".into()
    }
}
