//! Local profile lifecycle: list / info / lock-status / downloads / delete / prune.
//!
//! Operates on disk only; never reaches over the network. Used by both
//! the SDK consumer and the `afhttp profile` CLI subcommand.

pub mod cookie_jar;
pub mod info;
pub mod lock;
pub mod meta;
pub mod paths;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::shared::error::{Error, ErrorCode};

/// Top-level profile inventory entry from `afhttp profile list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileEntry {
    pub name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub metadata_present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used_at_rfc3339: Option<String>,
    pub locked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadEntry {
    pub filename: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub state: String,
}

fn root_or_default(root: Option<&Path>) -> PathBuf {
    root.map(Path::to_path_buf)
        .unwrap_or_else(paths::default_root)
}

pub fn list(profile_root: Option<&Path>) -> Result<Vec<ProfileEntry>, Error> {
    let root = root_or_default(profile_root);
    if !root.exists() {
        return Ok(Vec::new());
    }
    let read = std::fs::read_dir(&root).map_err(|e| {
        Error::new(
            ErrorCode::ProfileRootUnavailable,
            format!("read_dir({}): {e}", root.display()),
        )
    })?;
    let mut out = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        out.push(info_at(&path, &name)?);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

pub fn info(name: &str, profile_root: Option<&Path>) -> Result<ProfileEntry, Error> {
    paths::validate_name(name)?;
    let root = root_or_default(profile_root);
    let dir = root.join(name);
    if !dir.exists() {
        return Err(Error::new(
            ErrorCode::ProfileNotFound,
            format!("profile {name:?} not found at {}", dir.display()),
        ));
    }
    info_at(&dir, name)
}

fn info_at(dir: &Path, name: &str) -> Result<ProfileEntry, Error> {
    let meta_path = dir.join("afhttp-profile.json");
    let (metadata_present, last_used_at) = if meta_path.exists() {
        match std::fs::read_to_string(&meta_path) {
            Ok(s) => match serde_json::from_str::<meta::ProfileMeta>(&s) {
                Ok(m) => (true, Some(m.last_used_at_rfc3339)),
                Err(_) => (false, None),
            },
            Err(_) => (false, None),
        }
    } else {
        (false, None)
    };
    let size_bytes = dir_size(dir).unwrap_or(0);
    let locked = lock::probe(dir);
    Ok(ProfileEntry {
        name: name.to_string(),
        path: dir.to_path_buf(),
        size_bytes,
        metadata_present,
        last_used_at_rfc3339: last_used_at,
        locked,
    })
}

pub fn lock_status(name: &str, profile_root: Option<&Path>) -> Result<lock::LockStatus, Error> {
    paths::validate_name(name)?;
    let root = root_or_default(profile_root);
    let dir = root.join(name);
    if !dir.exists() {
        return Err(Error::new(
            ErrorCode::ProfileNotFound,
            format!("profile {name:?} not found"),
        ));
    }
    Ok(lock::status(&dir))
}

pub fn downloads(name: &str, profile_root: Option<&Path>) -> Result<Vec<DownloadEntry>, Error> {
    let entry = info(name, profile_root)?;
    downloads_at(&entry.path)
}

fn downloads_at(profile_dir: &Path) -> Result<Vec<DownloadEntry>, Error> {
    let dir = profile_dir.join("downloads");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let read = std::fs::read_dir(&dir).map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("read_dir({}): {e}", dir.display()),
        )
    })?;
    let mut out = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let filename = filename.to_string();
        let state = if path.extension().and_then(|ext| ext.to_str()) == Some("crdownload") {
            "in_progress"
        } else {
            "completed"
        };
        let path = path.canonicalize().unwrap_or(path);
        out.push(DownloadEntry {
            filename,
            path,
            size_bytes: meta.len(),
            state: state.to_string(),
        });
    }
    out.sort_by(|a, b| a.filename.cmp(&b.filename));
    Ok(out)
}

pub fn delete(name: &str, confirm: &str, profile_root: Option<&Path>) -> Result<(), Error> {
    paths::validate_name(name)?;
    if name != confirm {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            format!("profile delete: --confirm must match name (got {confirm:?})"),
        ));
    }
    let root = root_or_default(profile_root);
    let dir = root.join(name);
    if !dir.exists() {
        return Err(Error::new(
            ErrorCode::ProfileNotFound,
            format!("profile {name:?} not found"),
        ));
    }
    if lock::probe(&dir) {
        return Err(Error::new(
            ErrorCode::ProfileDeleteLocked,
            format!("profile {name:?} is locked; cannot delete"),
        ));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("remove_dir_all({}): {e}", dir.display()),
        )
    })
}

pub fn prune(
    older_than: Duration,
    dry_run: bool,
    profile_root: Option<&Path>,
) -> Result<Vec<ProfileEntry>, Error> {
    let entries = list(profile_root)?;
    let now = SystemTime::now();
    let mut removed = Vec::new();
    for entry in entries {
        if entry.locked {
            continue;
        }
        let too_old = match std::fs::metadata(&entry.path) {
            Ok(m) => match m.modified() {
                Ok(t) => match now.duration_since(t) {
                    Ok(age) => age >= older_than,
                    Err(_) => false,
                },
                Err(_) => false,
            },
            Err(_) => false,
        };
        if !too_old {
            continue;
        }
        if !dry_run {
            std::fs::remove_dir_all(&entry.path).map_err(|e| {
                Error::new(
                    ErrorCode::IoError,
                    format!("remove_dir_all({}): {e}", entry.path.display()),
                )
            })?;
        }
        removed.push(entry);
    }
    Ok(removed)
}

fn dir_size(dir: &Path) -> std::io::Result<u64> {
    let mut total: u64 = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(p) = stack.pop() {
        for entry in std::fs::read_dir(&p)?.flatten() {
            let path = entry.path();
            let md = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if md.is_file() {
                total = total.saturating_add(md.len());
            } else if md.is_dir() {
                stack.push(path);
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch_profile(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let meta = meta::ProfileMeta::new(name);
        std::fs::write(
            dir.join("afhttp-profile.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
        dir
    }

    #[test]
    fn lists_existing_profiles() {
        let tmp = tempfile::tempdir().unwrap();
        touch_profile(tmp.path(), "work");
        touch_profile(tmp.path(), "alpha");
        let entries = list(Some(tmp.path())).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "alpha"); // sorted
        assert_eq!(entries[1].name, "work");
        assert!(entries[0].metadata_present);
    }

    #[test]
    fn info_returns_profile_details() {
        let tmp = tempfile::tempdir().unwrap();
        touch_profile(tmp.path(), "work");
        let entry = info("work", Some(tmp.path())).unwrap();
        assert_eq!(entry.name, "work");
        assert!(entry.metadata_present);
        assert!(!entry.locked);
    }

    #[test]
    fn downloads_lists_profile_download_artifacts() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = touch_profile(tmp.path(), "work");
        let download_dir = profile.join("downloads");
        std::fs::create_dir_all(&download_dir).unwrap();
        std::fs::write(download_dir.join("report.csv"), "abc").unwrap();
        std::fs::write(download_dir.join("pending.bin.crdownload"), "partial").unwrap();

        let entries = downloads("work", Some(tmp.path())).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].filename, "pending.bin.crdownload");
        assert_eq!(entries[0].state, "in_progress");
        assert_eq!(entries[1].filename, "report.csv");
        assert_eq!(entries[1].size_bytes, 3);
        assert_eq!(entries[1].state, "completed");
    }

    #[test]
    fn info_missing_profile_returns_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let err = info("nope", Some(tmp.path())).err().unwrap();
        assert_eq!(err.error_code, ErrorCode::ProfileNotFound);
    }

    #[test]
    fn delete_requires_matching_confirm() {
        let tmp = tempfile::tempdir().unwrap();
        touch_profile(tmp.path(), "work");
        let err = delete("work", "different", Some(tmp.path())).err().unwrap();
        assert_eq!(err.error_code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn delete_removes_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = touch_profile(tmp.path(), "work");
        delete("work", "work", Some(tmp.path())).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn delete_missing_returns_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let err = delete("nope", "nope", Some(tmp.path())).err().unwrap();
        assert_eq!(err.error_code, ErrorCode::ProfileNotFound);
    }

    #[test]
    fn delete_locked_refuses() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = touch_profile(tmp.path(), "work");
        // Hold the lock for the duration of the assertion.
        let _g = lock::Guard::acquire(&dir).unwrap();
        let err = delete("work", "work", Some(tmp.path())).err().unwrap();
        assert_eq!(err.error_code, ErrorCode::ProfileDeleteLocked);
    }

    #[test]
    fn prune_dry_run_does_not_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = touch_profile(tmp.path(), "work");
        // Set mtime far in the past.
        let two_hours = SystemTime::now() - Duration::from_secs(7200);
        filetime::set_file_mtime(&dir, filetime::FileTime::from_system_time(two_hours)).ok();
        let removed = prune(Duration::from_secs(3600), true, Some(tmp.path())).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(dir.exists());
    }

    #[test]
    fn prune_removes_old_profiles() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = touch_profile(tmp.path(), "work");
        let two_hours = SystemTime::now() - Duration::from_secs(7200);
        filetime::set_file_mtime(&dir, filetime::FileTime::from_system_time(two_hours)).ok();
        let removed = prune(Duration::from_secs(3600), false, Some(tmp.path())).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(!dir.exists());
    }
}
