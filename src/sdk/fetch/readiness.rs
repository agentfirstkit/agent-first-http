//! Mechanical readiness, separate from HTTP success and business-data validity.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessState {
    Ready,
    Incomplete,
    Unknown,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestScope {
    MainDocument,
    Target,
    Ancillary,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingRequest {
    pub request_id: String,
    pub request_url: String,
    pub resource_type: String,
    pub frame_id: Option<String>,
    pub frame_url: Option<String>,
    pub scope: RequestScope,
    pub blocks_wait: bool,
    /// Elapsed local monotonic time since the driver observed this request.
    pub waited_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Readiness {
    pub wait_satisfied: bool,
    pub timed_out: bool,
    pub main_document: ReadinessState,
    pub target_content: ReadinessState,
    pub ancillary: ReadinessState,
    /// `body` for strict auto; caller-declared CSS for selector-stable.
    pub target_selector: String,
    pub target_dom_stable: bool,
    pub target_text_stable: bool,
    /// False if frame inspection or network event capture was incomplete.
    pub observation_complete: bool,
    pub unobserved_frame_ids: Vec<String>,
    pub pending_requests: Vec<PendingRequest>,
    pub pending_requests_truncated: bool,
}
