//! GET /capabilities response builder.

use std::collections::BTreeMap;

use crate::host::listener::AppState;
use crate::sdk::capabilities::{
    ArtifactSupport, BackendFamily, CapabilitiesResponse, FeatureSupport, OpsPanelSupport,
    ProfileSupport,
};

pub fn build(state: &AppState) -> CapabilitiesResponse {
    let default_entry = state.get_profile();
    let backend_ready = default_entry.is_some();
    let (family, version) = match default_entry {
        Some(e) => (e.handle.family.clone(), e.handle.version.clone()),
        None => ("unknown".into(), "".into()),
    };
    // Both lightpanda and the camoufox-via-foxbridge bridge speak a CDP
    // subset that lacks the chromium screenshot / screencast surface and
    // does not back the ops panel's live screen-grab flow. Group them so
    // capability gating stays mechanical.
    let is_subset_backend = family == "lightpanda" || family == "camoufox";
    let display_takeover = backend_ready && family != "lightpanda";
    let screencast_supported = backend_ready && state.ops_enabled && !is_subset_backend;
    let display_enabled = state.display_takeover.is_some();
    let display_provider = state
        .display_takeover
        .as_ref()
        .map(|display| display.provider.as_str().to_string());

    let mut artifacts: BTreeMap<String, ArtifactSupport> = BTreeMap::new();
    artifacts.insert(
        "body".into(),
        ArtifactSupport {
            supported: backend_ready,
            source: None,
            body_capture: Vec::new(),
        },
    );
    for token in ["rendered_html", "text", "console"] {
        artifacts.insert(
            token.into(),
            ArtifactSupport {
                supported: backend_ready,
                source: None,
                body_capture: Vec::new(),
            },
        );
    }
    artifacts.insert(
        "screenshot".into(),
        ArtifactSupport {
            supported: backend_ready && !is_subset_backend,
            source: None,
            body_capture: Vec::new(),
        },
    );
    artifacts.insert(
        "network".into(),
        ArtifactSupport {
            supported: backend_ready,
            source: None,
            body_capture: if backend_ready {
                vec!["off".into(), "xhr".into(), "all".into()]
            } else {
                Vec::new()
            },
        },
    );
    artifacts.insert(
        "observation".into(),
        ArtifactSupport {
            supported: backend_ready,
            source: Some("accessibility+dom".into()),
            body_capture: Vec::new(),
        },
    );
    artifacts.insert(
        "storage".into(),
        ArtifactSupport {
            supported: backend_ready,
            source: Some("browser_storage".into()),
            body_capture: Vec::new(),
        },
    );

    let mut features: BTreeMap<String, FeatureSupport> = BTreeMap::new();
    features.insert(
        "selector_visible".into(),
        FeatureSupport {
            supported: true,
            detail: Some("--wait selector-visible:<css>".into()),
            risk: None,
        },
    );
    features.insert(
        "network_body_capture".into(),
        FeatureSupport {
            supported: backend_ready,
            detail: Some("--network-bodies xhr|all".into()),
            risk: None,
        },
    );
    features.insert(
        "capture_ws".into(),
        FeatureSupport {
            supported: backend_ready,
            detail: Some("--capture-ws".into()),
            risk: Some("may expose tokens or PII in frame payload artifacts".into()),
        },
    );
    features.insert(
        "capture_sse".into(),
        FeatureSupport {
            supported: backend_ready,
            detail: Some("--capture-sse".into()),
            risk: Some("may expose tokens or PII in event payload artifacts".into()),
        },
    );
    features.insert(
        "display_takeover".into(),
        FeatureSupport {
            supported: display_takeover,
            detail: Some("--takeover display --display-provider kasmvnc".into()),
            risk: None,
        },
    );
    features.insert(
        "ops_panel".into(),
        FeatureSupport {
            supported: screencast_supported || display_enabled,
            detail: Some("/ops/screencast".into()),
            risk: None,
        },
    );
    features.insert(
        "recent_requests".into(),
        FeatureSupport {
            supported: state.recent_requests.is_some(),
            detail: Some("--recent-requests-cap > 0".into()),
            risk: None,
        },
    );
    features.insert(
        "profile_persistence".into(),
        FeatureSupport {
            supported: true,
            detail: Some("persistent and ephemeral profiles".into()),
            risk: None,
        },
    );
    features.insert(
        "network_redact_off".into(),
        FeatureSupport {
            supported: true,
            detail: Some("--network-redact off".into()),
            risk: Some("writes raw credential headers and other PII into network artifacts".into()),
        },
    );

    CapabilitiesResponse {
        code: "capabilities".into(),
        backend: BackendFamily { family, version },
        artifacts,
        wait_modes: vec![
            "auto".into(),
            "load".into(),
            "idle".into(),
            "selector".into(),
            "selector_visible".into(),
            "ms".into(),
        ],
        display_takeover,
        ops_panel: OpsPanelSupport {
            supported: screencast_supported || display_enabled,
            screencast: screencast_supported,
            display: display_enabled,
            screencast_url: screencast_supported.then(|| "/ops/screencast".to_string()),
            display_url: display_enabled.then(|| "/ops/display".to_string()),
            display_provider,
        },
        profile: ProfileSupport {
            persistent: true,
            ephemeral: true,
        },
        features,
        limits: crate::host::listener::default_limits(),
    }
}
