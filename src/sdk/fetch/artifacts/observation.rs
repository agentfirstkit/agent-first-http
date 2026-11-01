//! Agent-readable page snapshot (`observation.json`).
//!
//! Mechanical projection of the accessibility tree + DOM geometry per
//! `architecture.md §8`. Disallowed: intent labels like "login", "captcha",
//! importance ranking. This module owns the
//! schema and the no-intent-label invariant.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::sdk::fetch::writer;
use crate::shared::artifacts::{Artifact, ArtifactPaths};
use crate::shared::error::{Error, ErrorCode};

pub const OBSERVATION_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub schema_version: u32,
    #[serde(alias = "url")]
    pub page_url: String,
    pub title: String,
    pub viewport: Viewport,
    pub frames: Vec<Frame>,
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub forms: Vec<Form>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focused_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<ObservationTruncation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
    pub device_scale_factor: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub frame_id: String,
    #[serde(alias = "url")]
    pub frame_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    /// Per-snapshot ref; not a durable selector.
    pub r#ref: String,
    pub frame_id: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub visible: bool,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<BBox>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_redacted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector_hint_unique: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Form {
    pub r#ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationTruncation {
    pub reason: String,
    pub node_limit: usize,
    pub scan_limit: usize,
    pub scanned: usize,
    pub emitted_nodes: usize,
}

/// Capture an `Observation` from a CDP session. Projects the accessibility
/// tree + page title + viewport into the mechanical schema; never adds
/// intent labels or importance ranks (see `DISALLOWED_LABELS`).
pub async fn capture(
    conn: &crate::sdk::cdp::ws_client::Connection,
    session_id: &str,
    url: &str,
) -> Result<Observation, Error> {
    // Page metadata.
    let meta = conn
        .send(
            "Runtime.evaluate",
            &serde_json::json!({
                "expression": "JSON.stringify({title: document.title, w: window.innerWidth, h: window.innerHeight, dpr: window.devicePixelRatio})",
                "returnByValue": true,
            }),
            Some(session_id),
        )
        .await?;
    let title;
    let viewport;
    if let Some(s) = meta["result"]["value"].as_str() {
        let v: serde_json::Value = serde_json::from_str(s).unwrap_or(serde_json::Value::Null);
        title = v["title"].as_str().unwrap_or("").to_string();
        viewport = Viewport {
            width: v["w"].as_u64().unwrap_or(0) as u32,
            height: v["h"].as_u64().unwrap_or(0) as u32,
            device_scale_factor: v["dpr"].as_f64().unwrap_or(1.0) as f32,
        };
    } else {
        title = String::new();
        viewport = Viewport {
            width: 0,
            height: 0,
            device_scale_factor: 1.0,
        };
    }

    // Mechanical DOM projection for agent planning. This intentionally
    // captures roles/states/geometry/actions without ranking or intent labels.
    let dom = conn
        .send(
            "Runtime.evaluate",
            &serde_json::json!({
                "expression": OBSERVATION_JS,
                "returnByValue": true,
            }),
            Some(session_id),
        )
        .await?;
    let raw = dom["result"]["value"].as_str().ok_or_else(|| {
        Error::new(
            ErrorCode::ArtifactCaptureFailed,
            "observation snapshot did not return a JSON string",
        )
    })?;
    let snapshot = parse_snapshot(raw)?;

    Ok(Observation {
        schema_version: OBSERVATION_SCHEMA_VERSION,
        page_url: url.to_string(),
        title,
        viewport,
        frames: snapshot.frames,
        nodes: snapshot.nodes,
        forms: snapshot.forms,
        focused_ref: snapshot.focused_ref,
        truncated: snapshot.truncated,
    })
}

#[derive(Debug, Deserialize)]
struct Snapshot {
    nodes: Vec<Node>,
    forms: Vec<Form>,
    frames: Vec<Frame>,
    focused_ref: Option<String>,
    truncated: Option<ObservationTruncation>,
}

fn parse_snapshot(raw: &str) -> Result<Snapshot, Error> {
    let mut deserializer = serde_json::Deserializer::from_str(raw);
    let snapshot = serde_path_to_error::deserialize(&mut deserializer).map_err(|error| {
        Error::new(
            ErrorCode::ArtifactCaptureFailed,
            format!(
                "decode observation snapshot at {}: {}",
                error.path(),
                error.inner()
            ),
        )
    })?;
    deserializer.end().map_err(|error| {
        Error::new(
            ErrorCode::ArtifactCaptureFailed,
            format!("decode observation snapshot: {error}"),
        )
    })?;
    Ok(snapshot)
}

const OBSERVATION_JS: &str = include_str!("../../../../assets/observation/snapshot.js");

pub async fn write(paths: &ArtifactPaths, obs: &Observation) -> Result<PathBuf, Error> {
    let target = paths.file_for(Artifact::Observation);
    let bytes = serde_json::to_vec_pretty(obs).map_err(|e| {
        Error::new(
            ErrorCode::InternalError,
            format!("serialize observation: {e}"),
        )
    })?;
    writer::write_bytes(&target, &bytes).await?;
    Ok(target)
}

/// Disallowed substrings in any string field of an `Observation`. Tests
/// use this to enforce `design.md §"Observation is mechanical, not
/// interpretive"`.
pub const DISALLOWED_LABELS: &[&str] =
    &["login", "captcha", "paywall", "important", "best", "likely"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_round_trips() {
        let obs = Observation {
            schema_version: OBSERVATION_SCHEMA_VERSION,
            page_url: "https://example.com/".into(),
            title: "Example".into(),
            viewport: Viewport {
                width: 1280,
                height: 720,
                device_scale_factor: 1.0,
            },
            frames: vec![Frame {
                frame_id: "main".into(),
                frame_url: "https://example.com/".into(),
            }],
            nodes: vec![Node {
                r#ref: "obs-1".into(),
                frame_id: "main".into(),
                role: "button".into(),
                name: Some("Submit".into()),
                text: Some("Submit".into()),
                visible: true,
                enabled: true,
                bbox: Some(BBox {
                    x: 0.0,
                    y: 0.0,
                    width: 80.0,
                    height: 32.0,
                }),
                actions: vec!["click".into()],
                href: None,
                src: None,
                frame_ref: None,
                input_type: None,
                checked: None,
                selected: None,
                focused: None,
                value_redacted: None,
                selector_hint: None,
                selector_hint_unique: None,
            }],
            forms: vec![],
            focused_ref: None,
            truncated: None,
        };
        let json = serde_json::to_string(&obs).unwrap_or_default();
        let parsed: Observation = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.nodes.len(), 1);
        assert_eq!(parsed.nodes[0].role, "button");
    }

    #[test]
    fn invalid_snapshot_reports_node_field_instead_of_empty_nodes() {
        let error = parse_snapshot(r#"{"nodes":[{"ref":"obs-0","frame_id":"main","role":"link","visible":true,"enabled":true,"href":{}}],"forms":[],"frames":[]}"#).unwrap_err();
        assert_eq!(error.error_code, ErrorCode::ArtifactCaptureFailed);
        assert!(error.detail.contains("nodes[0].href"), "{}", error.detail);
        assert!(parse_snapshot("undefined").is_err());
        assert!(parse_snapshot(r#"{"forms":[],"frames":[]}"#).is_err());
        let empty = parse_snapshot(r#"{"nodes":[],"forms":[],"frames":[]}"#).unwrap();
        assert!(empty.nodes.is_empty());
    }

    #[test]
    fn disallowed_labels_present_in_constant() {
        for label in DISALLOWED_LABELS {
            assert!(!label.is_empty());
        }
    }
}
