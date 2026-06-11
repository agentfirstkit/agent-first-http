//! Deep network capture (`network.json`). Aggregated from CDP `Network.*`
//! events. Headers are redacted by default per [`crate::shared::redact`].
//! This module defines the on-wire schema and the redaction helper used by tests.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::sdk::fetch::writer;
use crate::shared::artifacts::{Artifact, ArtifactPaths};
use crate::shared::error::{Error, ErrorCode};
use crate::shared::redact;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkLog {
    pub schema_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_request_id: Option<String>,
    pub entries: Vec<NetworkEntry>,
    pub summary: NetworkSummary,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkSummary {
    pub requests_total: usize,
    pub responses_total: usize,
    pub finished_total: usize,
    pub failed_total: usize,
    pub incomplete_total: usize,
    pub inflight_total_at_capture: usize,
    pub pending_by_resource_type: BTreeMap<String, usize>,
    pub captured_body_files: usize,
    pub redacted: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkEntryState {
    #[default]
    Pending,
    Responded,
    Finished,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkEntry {
    pub request_id: String,
    pub state: NetworkEntryState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_from_request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loader_id: Option<String>,
    pub resource_type: String,
    pub url: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initiator: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub request_headers: BTreeMap<String, String>,
    pub response_headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub request_post_data_present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_post_data_size_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_file: Option<PathBuf>,
    pub timing: NetworkTiming,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    pub hints: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkTiming {
    pub start_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<u64>,
}

pub async fn write(paths: &ArtifactPaths, log: &NetworkLog) -> Result<PathBuf, Error> {
    let target = paths.file_for(Artifact::Network);
    let bytes = serde_json::to_vec_pretty(log).map_err(|e| {
        Error::new(
            ErrorCode::InternalError,
            format!("serialize network log: {e}"),
        )
    })?;
    writer::write_bytes(&target, &bytes).await?;
    Ok(target)
}

/// Apply the default redaction policy to a header map in-place. Pass
/// `enabled = false` for `--no-network-redact`.
pub fn redact_headers(map: &mut BTreeMap<String, String>, enabled: bool) {
    if !enabled {
        return;
    }
    for (name, value) in map.iter_mut() {
        if redact::should_redact(name) {
            *value = redact::REDACTED_VALUE.to_string();
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_replaces_credentials_only() {
        let mut map = BTreeMap::new();
        map.insert("Authorization".to_string(), "Bearer xyz".to_string());
        map.insert("Cookie".to_string(), "session=abc".to_string());
        map.insert("X-Api-Token".to_string(), "tok".to_string());
        map.insert("Content-Type".to_string(), "application/json".to_string());
        redact_headers(&mut map, true);
        assert_eq!(map["Authorization"], "[redacted]");
        assert_eq!(map["Cookie"], "[redacted]");
        assert_eq!(map["X-Api-Token"], "[redacted]");
        assert_eq!(map["Content-Type"], "application/json");
    }

    #[test]
    fn redact_disabled_passes_everything_through() {
        let mut map = BTreeMap::new();
        map.insert("Authorization".to_string(), "Bearer xyz".to_string());
        redact_headers(&mut map, false);
        assert_eq!(map["Authorization"], "Bearer xyz");
    }
}
