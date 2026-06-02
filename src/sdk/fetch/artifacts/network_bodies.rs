//! Opt-in network response body capture under `network-bodies/`.

use std::path::PathBuf;

use crate::sdk::fetch::writer;
use crate::shared::artifacts::ArtifactPaths;
use crate::shared::error::Error;

/// Write a single response body. `request_id` is the CDP-assigned id used
/// as the filename stem; callers pick the extension from the mime type.
pub async fn write(
    paths: &ArtifactPaths,
    request_id: &str,
    ext: &str,
    bytes: &[u8],
) -> Result<PathBuf, Error> {
    let dir = paths.network_bodies_dir();
    writer::ensure_dir(&dir).await?;
    let target = dir.join(format!("{request_id}.{ext}"));
    writer::write_bytes(&target, bytes).await?;
    Ok(target)
}
