//! `GET /profile` — surface the host's resolved profile path so the SDK
//! can place the cookie jar (and any future profile-internal artifacts)
//! inside the same sandbox the host is using.
//!
//! Per the isolation invariant in [design.md] and [architecture.md §7],
//! all persistent state for a session lives under the profile directory.
//! The SDK derives `<profile>/cookies.jar.json` from this endpoint's
//! response — it is the single point where the SDK learns "where am I
//! allowed to write" so cross-profile leaks are impossible without an
//! explicit override flag.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Serialize;

use crate::host::listener::AppState;
use crate::shared::profile_snapshot::ProfileSnapshot;

/// Wire envelope: `ProfileSnapshot` plus the `code` tag required by the
/// protocol (architecture.md §1). The `#[serde(flatten)]` ensures the
/// snapshot fields appear at the top level, not nested under `"data"`.
#[derive(Serialize)]
struct ProfileEnvelope {
    code: &'static str,
    #[serde(flatten)]
    snapshot: ProfileSnapshot,
}

pub async fn handler(State(state): State<AppState>) -> Response {
    let Some(entry) = state.get_profile() else {
        let body = serde_json::json!({
            "code": "error",
            "error_code": "internal_error",
            "error": "profile not yet resolved",
            "retryable": true,
        });
        return (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response();
    };
    let payload = ProfileEnvelope {
        code: "profile",
        snapshot: ProfileSnapshot {
            kind: entry.kind.clone(),
            name: if entry.kind == "persistent" {
                Some(entry.name.clone())
            } else {
                None
            },
            path: Some(entry.profile_path().clone()),
            locked: entry.kind == "persistent",
        },
    };
    Json(payload).into_response()
}
