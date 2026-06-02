//! Full-page PNG via CDP `Page.captureScreenshot { format: png, captureBeyondViewport: true }`.

use std::path::PathBuf;

use crate::sdk::fetch::writer;
use crate::shared::artifacts::{Artifact, ArtifactPaths};
use crate::shared::error::Error;

pub async fn write(paths: &ArtifactPaths, png: &[u8]) -> Result<PathBuf, Error> {
    let target = paths.file_for(Artifact::Screenshot);
    writer::write_bytes(&target, png).await?;
    Ok(target)
}
