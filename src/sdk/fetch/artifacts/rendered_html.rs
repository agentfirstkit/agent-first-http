//! Post-JS DOM serialized to HTML, emitted via CDP
//! `DOM.getOuterHTML` on the document node when a browser was used.

use std::path::PathBuf;

use crate::sdk::fetch::writer;
use crate::shared::artifacts::{Artifact, ArtifactPaths};
use crate::shared::error::Error;

pub async fn write(paths: &ArtifactPaths, html: &str) -> Result<PathBuf, Error> {
    let target = paths.file_for(Artifact::RenderedHtml);
    writer::write_bytes(&target, html.as_bytes()).await?;
    Ok(target)
}
