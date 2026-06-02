//! `document.body.innerText` artifact. Mechanical projection; not an
//! interpretation, via CDP `Runtime.evaluate`.

use std::path::PathBuf;

use crate::sdk::fetch::writer;
use crate::shared::artifacts::{Artifact, ArtifactPaths};
use crate::shared::error::Error;

pub async fn write(paths: &ArtifactPaths, text: &str) -> Result<PathBuf, Error> {
    let target = paths.file_for(Artifact::Text);
    writer::write_bytes(&target, text.as_bytes()).await?;
    Ok(target)
}
