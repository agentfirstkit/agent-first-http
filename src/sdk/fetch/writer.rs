//! Atomic artifact writes into `--out/<request_id>/`.
//!
//! `write_bytes` creates the destination directory lazily and writes to a
//! `.tmp` sibling first, then renames into place so a crashed fetch never
//! leaves a half-written artifact that a later run would mistake for
//! complete.

use std::path::{Path, PathBuf};

use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::shared::error::{Error, ErrorCode};

pub async fn ensure_dir(dir: &Path) -> Result<(), Error> {
    fs::create_dir_all(dir).await.map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("create_dir_all({}): {e}", dir.display()),
        )
    })
}

pub async fn write_bytes(target: &Path, bytes: &[u8]) -> Result<(), Error> {
    if let Some(parent) = target.parent() {
        ensure_dir(parent).await?;
    }
    let tmp = sibling_tmp(target);
    let mut file = fs::File::create(&tmp).await.map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("create({}): {e}", tmp.display()),
        )
    })?;
    file.write_all(bytes)
        .await
        .map_err(|e| Error::new(ErrorCode::IoError, format!("write({}): {e}", tmp.display())))?;
    file.flush()
        .await
        .map_err(|e| Error::new(ErrorCode::IoError, format!("flush({}): {e}", tmp.display())))?;
    drop(file);
    fs::rename(&tmp, target).await.map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("rename({} -> {}): {e}", tmp.display(), target.display()),
        )
    })
}

fn sibling_tmp(target: &Path) -> PathBuf {
    let mut name = target
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    target.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writes_and_renames_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nested/body.txt");
        write_bytes(&target, b"hello").await.unwrap();
        let read = fs::read(&target).await.unwrap();
        assert_eq!(read, b"hello");
        // No leftover .tmp
        let tmp = sibling_tmp(&target);
        assert!(!tmp.exists());
    }
}
