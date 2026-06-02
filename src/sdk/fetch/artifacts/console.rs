//! Console event capture (`console.json`) via CDP
//! `Runtime.consoleAPICalled` + `Runtime.exceptionThrown`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::sdk::fetch::writer;
use crate::shared::artifacts::{Artifact, ArtifactPaths};
use crate::shared::error::{Error, ErrorCode};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleLog {
    pub schema_version: u32,
    pub events: Vec<ConsoleEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleEvent {
    pub level: ConsoleLevel,
    pub timestamp_ms: f64,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
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
