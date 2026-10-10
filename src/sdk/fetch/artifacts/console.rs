//! Console event capture (`console.json`) via CDP
//! `Runtime.consoleAPICalled` + `Runtime.exceptionThrown`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::sdk::fetch::writer;
use crate::shared::artifacts::{Artifact, ArtifactPaths};
use crate::shared::error::{Error, ErrorCode};

pub const CONSOLE_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleLog {
    pub schema_version: u32,
    pub events: Vec<ConsoleEvent>,
    /// CDP events lost because the broadcast receiver fell behind.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub events_dropped: u64,
    /// The fixed console event limit omitted data.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleEvent {
    pub level: ConsoleLevel,
    #[serde(alias = "timestamp_ms")]
    pub timestamp_epoch_ms: f64,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(alias = "url")]
    pub source_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_number: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleLevel {
    Log,
    Debug,
    Info,
    Warn,
    Error,
    Exception,
}

pub async fn write(paths: &ArtifactPaths, log: &ConsoleLog) -> Result<PathBuf, Error> {
    let target = paths.file_for(Artifact::Console);
    let bytes = serde_json::to_vec_pretty(log).map_err(|e| {
        Error::new(
            ErrorCode::InternalError,
            format!("serialize console log: {e}"),
        )
    })?;
    writer::write_bytes(&target, &bytes).await?;
    Ok(target)
}
