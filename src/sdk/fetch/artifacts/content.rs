//! Agent-oriented composed content artifacts.
//!
//! `content.md` is the primary human/LLM-readable projection, while
//! `content.json` keeps links and page structure machine-readable.

use std::path::PathBuf;

use serde_json::Value;

use crate::sdk::fetch::writer;
use crate::shared::artifacts::{Artifact, ArtifactPaths};
use crate::shared::error::{Error, ErrorCode};

#[derive(Debug, Clone)]
pub struct ContentCapture {
    pub markdown: String,
    pub json: Value,
}

pub async fn write_markdown(paths: &ArtifactPaths, markdown: &str) -> Result<PathBuf, Error> {
    let target = paths.file_for(Artifact::Content);
    writer::write_bytes(&target, markdown.as_bytes()).await?;
    Ok(target)
}

pub async fn write_json(paths: &ArtifactPaths, json: &Value) -> Result<PathBuf, Error> {
    let target = paths.file_for(Artifact::ContentJson);
    let bytes = serde_json::to_vec_pretty(json).map_err(|e| {
        Error::new(
            ErrorCode::InternalError,
            format!("serialize content json: {e}"),
        )
    })?;
    writer::write_bytes(&target, &bytes).await?;
    Ok(target)
}
