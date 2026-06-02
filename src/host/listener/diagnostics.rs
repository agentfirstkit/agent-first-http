//! `GET /diagnostics` — auth-required endpoint that returns host status and
//! per-profile browser stderr tails. Gives agents structured follow-up for
//! `tab_crashed` or `browser_launch_failed` without needing shell access.

use axum::extract::State;
use axum::response::{IntoResponse, Json, Response};

use crate::host::listener::AppState;

pub async fn handler(State(state): State<AppState>) -> Response {
    let uptime_s = state.started_at.elapsed().as_secs();

    let profile = if let Some(entry) = state.get_profile() {
        // Browser stderr can echo afhttp's own `--proxy-server=...:pass@...`
        // launch arg; mask the userinfo password (afhttp's credential, not
        // page data) before surfacing it.
        let stderr_lines: Vec<String> = {
            let guard = entry.handle.stderr_ring.lock().await;
            guard
                .iter()
                .map(|line| crate::shared::redact::redact_userinfo_passwords(line))
                .collect()
        };
        Some(serde_json::json!({
            "name": entry.name.clone(),
            "kind": entry.kind.clone(),
            "family": entry.handle.family.clone(),
            "version": entry.handle.version.clone(),
            "ws_url": entry.handle.ws_url.clone(),
            "stderr_lines": stderr_lines,
        }))
    } else {
        None
    };

    let body = serde_json::json!({
        "code": "diagnostics",
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_s": uptime_s,
        "profile": profile,
    });
    Json(body).into_response()
}
