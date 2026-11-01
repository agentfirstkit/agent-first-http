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
    pub backend: String,
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
    let mut out = Vec::new();
    for backend in paths::KNOWN_BACKENDS {
        let backend_dir = root.join(backend);
        if !backend_dir.is_dir() {
            continue;
        }
        let read = std::fs::read_dir(&backend_dir).map_err(|e| {
            Error::new(
                ErrorCode::ProfileRootUnavailable,
                format!("read_dir({}): {e}", backend_dir.display()),
            )
        })?;
        for entry in read.flatten() {
            let path = entry.path();
            // `file_type` does not follow links: a link planted in the root is
            // not a profile, and prune would otherwise empty whatever it names.
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let name = match path.file_name().and_then(|s| s.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            out.push(info_at(&path, backend, &name)?);
        }
    }
    out.sort_by(|a, b| a.backend.cmp(&b.backend).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

pub fn info(
    name: &str,
    backend: Option<&str>,
    profile_root: Option<&Path>,
) -> Result<ProfileEntry, Error> {
    paths::validate_name(name)?;
    let root = root_or_default(profile_root);
    let (backend, dir) = resolve_backend_scoped_profile(&root, name, backend)?;
    info_at(&dir, &backend, name)
}

fn info_at(dir: &Path, backend: &str, name: &str) -> Result<ProfileEntry, Error> {
    let meta_path = dir.join("afhttp-profile.json");
    let (metadata_present, last_used_at) = if meta_path.exists() {
        let m = read_profile_meta(&meta_path)?;
        validate_profile_meta(&m, backend, name, dir)?;
        (true, Some(m.last_used_at_rfc3339))
    } else {
        (false, None)
    };
    let size_bytes = dir_size(dir).unwrap_or(0);
    let locked = lock::probe(dir);
    Ok(ProfileEntry {
        backend: backend.to_string(),
        name: name.to_string(),
        path: dir.to_path_buf(),
        size_bytes,
        metadata_present,
        last_used_at_rfc3339: last_used_at,
        locked,
    })
}

pub fn lock_status(
    name: &str,
    backend: Option<&str>,
    profile_root: Option<&Path>,
) -> Result<lock::LockStatus, Error> {
    paths::validate_name(name)?;
    let root = root_or_default(profile_root);
    let (_, dir) = resolve_backend_scoped_profile(&root, name, backend)?;
    Ok(lock::status(&dir))
}

pub fn downloads(
    name: &str,
    backend: Option<&str>,
    profile_root: Option<&Path>,
) -> Result<Vec<DownloadEntry>, Error> {
    let entry = info(name, backend, profile_root)?;
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

pub fn delete(
    name: &str,
    confirm: &str,
    backend: Option<&str>,
    profile_root: Option<&Path>,
) -> Result<(), Error> {
    paths::validate_name(name)?;
    if name != confirm {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            format!("profile delete: --confirm must match name (got {confirm:?})"),
        ));
    }
    let root = root_or_default(profile_root);
    let (backend, dir) = resolve_backend_scoped_profile(&root, name, backend)?;
    remove_profile_dir(&dir).map_err(|e| match e.error_code {
        ErrorCode::ProfileLocked => Error::new(
            ErrorCode::ProfileDeleteLocked,
            format!("profile {backend}/{name:?} is locked; cannot delete"),
        ),
        _ => e,
    })
}

/// Remove a profile directory while holding its lock.
///
/// Checking the lock and then deleting left a window in which a host could
/// acquire the profile and have its browser data removed underneath it.
/// Holding the guard across the removal closes that: a host that tries to
/// acquire meanwhile gets `profile_locked`. A lock file that cannot be opened
/// is an error here, never "unlocked" — not knowing is not permission.
///
/// Everything but the lock file goes while the lock is held. The lock file
/// itself and the then-empty directory go after the guard drops, because
/// Windows will not delete a file that is open. A host that claims the profile
/// in that last moment finds an empty directory, and the `remove_dir` below
/// fails rather than delete what it has started writing.
///
/// Returns `profile_locked` when a host holds the profile.
fn remove_profile_dir(dir: &Path) -> Result<(), Error> {
    let io = |what: &str, path: &Path, e: std::io::Error| {
        Error::new(
            ErrorCode::IoError,
            format!("{what}({}): {e}", path.display()),
        )
    };
    // Never through a link (or a Windows junction, which std reports as one):
    // the lock and every removal below would land in the directory it names.
    let kind = std::fs::symlink_metadata(dir)
        .map_err(|e| io("symlink_metadata", dir, e))?
        .file_type();
    if !kind.is_dir() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            format!(
                "{} is not a profile directory (a link or a file); remove it by hand",
                dir.display()
            ),
        ));
    }
    let guard = lock::Guard::acquire(dir)?;
    let lockfile = lock::lockfile(dir);
    for entry in std::fs::read_dir(dir).map_err(|e| io("read_dir", dir, e))? {
        let entry = entry.map_err(|e| io("read_dir", dir, e))?;
        let path = entry.path();
        if path == lockfile {
            continue;
        }
        let is_dir = entry
            .file_type()
            .map_err(|e| io("file_type", &path, e))?
            .is_dir();
        if is_dir {
            std::fs::remove_dir_all(&path).map_err(|e| io("remove_dir_all", &path, e))?;
        } else {
            std::fs::remove_file(&path).map_err(|e| io("remove_file", &path, e))?;
        }
    }
    drop(guard);
    match std::fs::remove_file(&lockfile) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io("remove_file", &lockfile, e)),
    }
    std::fs::remove_dir(dir).map_err(|e| io("remove_dir", dir, e))
}

fn resolve_backend_scoped_profile(
    root: &Path,
    name: &str,
    backend: Option<&str>,
) -> Result<(String, PathBuf), Error> {
    if let Some(backend) = backend {
        paths::validate_backend_key(backend)?;
        let dir = paths::join_root_backend_name(root, backend, name)?;
        if dir.exists() {
            return Ok((backend.to_string(), dir));
        }
        return Err(Error::new(
            ErrorCode::ProfileNotFound,
            format!(
                "profile {name:?} not found for backend {backend:?} at {}",
                dir.display()
            ),
        ));
    }

    let mut candidates = Vec::new();
    for backend in paths::KNOWN_BACKENDS {
        let dir = root.join(backend).join(name);
        if dir.exists() {
            candidates.push(((*backend).to_string(), dir));
        }
    }
    match candidates.len() {
        0 => Err(Error::new(
            ErrorCode::ProfileNotFound,
            format!(
                "profile {name:?} not found under backend-scoped root {}",
                root.display()
            ),
        )),
        1 => Ok(candidates.remove(0)),
        _ => Err(Error::new(
            ErrorCode::InvalidArgument,
            format!(
                "profile {name:?} exists under multiple backends ({}); pass --backend <backend>",
                candidates
                    .iter()
                    .map(|(backend, _)| backend.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

pub fn read_profile_meta(path: &Path) -> Result<meta::ProfileMeta, Error> {
    let s = std::fs::read_to_string(path).map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("read profile metadata {}: {e}", path.display()),
        )
    })?;
    // The name is fine; the file beside it is not. `io_error`, not retryable:
    // reading it again gives the same bytes.
    serde_json::from_str::<meta::ProfileMeta>(&s).map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!(
                "profile metadata {} is not schema v{}: {e}; back up this profile and restore or repair only afhttp-profile.json. Browser data is separate; do not delete the profile directory",
                path.display(),
                meta::ProfileMeta::SCHEMA_VERSION
            ),
        )
        .with_retryable(false)
    })
}

/// Check the metadata against the path it was found at. A mismatch is a
/// metadata fault (`io_error`, not retryable), not an invalid name: the name
/// the caller gave is valid, and it is the file that disagrees with it.
pub fn validate_profile_meta(
    meta: &meta::ProfileMeta,
    backend: &str,
    name: &str,
    dir: &Path,
) -> Result<(), Error> {
    if meta.schema_version != meta::ProfileMeta::SCHEMA_VERSION {
        return Err(Error::new(
            ErrorCode::IoError,
            format!(
                "profile metadata at {} has schema_version {}; expected {}; back up this profile and restore compatible afhttp-profile.json metadata; preserve browser data",
                dir.join("afhttp-profile.json").display(),
                meta.schema_version,
                meta::ProfileMeta::SCHEMA_VERSION
            ),
        )
        .with_retryable(false));
    }
    if meta.backend != backend {
        return Err(Error::new(
            ErrorCode::IoError,
            format!(
                "profile metadata backend {:?} does not match path backend {:?} at {}; back up this profile and repair afhttp-profile.json or select the matching backend; preserve browser data",
                meta.backend,
                backend,
                dir.display()
            ),
        )
        .with_retryable(false));
    }
    if meta.name != name {
        return Err(Error::new(
            ErrorCode::IoError,
            format!(
                "profile metadata name {:?} does not match path name {:?} at {}; back up this profile and repair afhttp-profile.json or select the matching name; preserve browser data",
                meta.name,
                name,
                dir.display()
            ),
        )
        .with_retryable(false));
    }
    Ok(())
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
        // Advisory only: the lock is taken for real before anything is
        // removed, and a profile a host claimed since is skipped then.
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
            match remove_profile_dir(&entry.path) {
                Ok(()) => {}
                Err(e) if e.error_code == ErrorCode::ProfileLocked => continue,
                Err(e) => return Err(e),
            }
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

    fn touch_profile(root: &Path, backend: &str, name: &str) -> PathBuf {
        let dir = root.join(backend).join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let meta = meta::ProfileMeta::new(name, backend);
        std::fs::write(
            dir.join("afhttp-profile.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
        dir
    }

    #[test]
    fn empty_metadata_diagnoses_metadata_and_preserves_browser_data() {
        let dir = tempfile::tempdir().unwrap();
        let metadata = dir.path().join("afhttp-profile.json");
        let cookies = dir.path().join("Cookies");
        std::fs::write(&metadata, b"").unwrap();
        std::fs::write(&cookies, b"browser cookie data").unwrap();
        let error = read_profile_meta(&metadata).unwrap_err();
        assert!(error.detail.contains("EOF"));
        assert!(error.detail.contains("Browser data is separate"));
        assert!(error.detail.contains("do not delete the profile directory"));
        assert!(!error.detail.contains("delete/recreate"));
        assert_eq!(std::fs::read(cookies).unwrap(), b"browser cookie data");
    }

    #[test]
    fn lists_existing_profiles() {
        let tmp = tempfile::tempdir().unwrap();
        touch_profile(tmp.path(), "brave", "work");
        touch_profile(tmp.path(), "chromium", "alpha");
        std::fs::create_dir_all(tmp.path().join("old-layout")).unwrap();
        let entries = list(Some(tmp.path())).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].backend, "brave"); // sorted by backend, then name
        assert_eq!(entries[0].name, "work");
        assert_eq!(entries[1].backend, "chromium");
        assert_eq!(entries[1].name, "alpha");
        assert!(entries[0].metadata_present);
    }

    #[test]
    fn info_returns_profile_details() {
        let tmp = tempfile::tempdir().unwrap();
        touch_profile(tmp.path(), "brave", "work");
        let entry = info("work", Some("brave"), Some(tmp.path())).unwrap();
        assert_eq!(entry.name, "work");
        assert_eq!(entry.backend, "brave");
        assert!(entry.metadata_present);
        assert!(!entry.locked);
    }

    #[test]
    fn downloads_lists_profile_download_artifacts() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = touch_profile(tmp.path(), "brave", "work");
        let download_dir = profile.join("downloads");
        std::fs::create_dir_all(&download_dir).unwrap();
        std::fs::write(download_dir.join("report.csv"), "abc").unwrap();
        std::fs::write(download_dir.join("pending.bin.crdownload"), "partial").unwrap();

        let entries = downloads("work", Some("brave"), Some(tmp.path())).unwrap();
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
        let err = info("nope", Some("brave"), Some(tmp.path())).err().unwrap();
        assert_eq!(err.error_code, ErrorCode::ProfileNotFound);
    }

    #[test]
    fn delete_requires_matching_confirm() {
        let tmp = tempfile::tempdir().unwrap();
        touch_profile(tmp.path(), "brave", "work");
        let err = delete("work", "different", Some("brave"), Some(tmp.path()))
            .err()
            .unwrap();
        assert_eq!(err.error_code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn delete_removes_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = touch_profile(tmp.path(), "brave", "work");
        delete("work", "work", Some("brave"), Some(tmp.path())).unwrap();
        assert!(!dir.exists());
    }

    /// A link in the profile root is not a profile: listing skips it, so
    /// prune never sees it, and delete refuses it before taking a lock or
    /// removing anything in the directory it names.
    #[cfg(unix)]
    #[test]
    fn a_linked_profile_directory_is_never_emptied() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let keep = outside.path().join("keep.txt");
        std::fs::write(&keep, b"not a profile").unwrap();
        std::fs::create_dir_all(tmp.path().join("brave")).unwrap();
        std::os::unix::fs::symlink(outside.path(), tmp.path().join("brave").join("linked"))
            .unwrap();

        assert!(list(Some(tmp.path())).unwrap().is_empty());
        assert!(
            prune(Duration::ZERO, false, Some(tmp.path()))
                .unwrap()
                .is_empty()
        );
        let err = delete("linked", "linked", Some("brave"), Some(tmp.path()))
            .err()
            .unwrap();
        assert_eq!(err.error_code, ErrorCode::InvalidArgument);
        assert_eq!(std::fs::read(&keep).unwrap(), b"not a profile");
        assert!(!lock::lockfile(outside.path()).exists());
    }

    #[test]
    fn delete_missing_returns_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let err = delete("nope", "nope", Some("brave"), Some(tmp.path()))
            .err()
            .unwrap();
        assert_eq!(err.error_code, ErrorCode::ProfileNotFound);
    }

    #[test]
    fn delete_locked_refuses() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = touch_profile(tmp.path(), "brave", "work");
        // Hold the lock for the duration of the assertion.
        let _g = lock::Guard::acquire(&dir).unwrap();
        let err = delete("work", "work", Some("brave"), Some(tmp.path()))
            .err()
            .unwrap();
        assert_eq!(err.error_code, ErrorCode::ProfileDeleteLocked);
    }

    #[test]
    fn prune_dry_run_does_not_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = touch_profile(tmp.path(), "brave", "work");
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
        let dir = touch_profile(tmp.path(), "brave", "work");
        let two_hours = SystemTime::now() - Duration::from_secs(7200);
        filetime::set_file_mtime(&dir, filetime::FileTime::from_system_time(two_hours)).ok();
        let removed = prune(Duration::from_secs(3600), false, Some(tmp.path())).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(!dir.exists());
    }

    #[test]
    fn same_name_multiple_backends_requires_backend() {
        let tmp = tempfile::tempdir().unwrap();
        touch_profile(tmp.path(), "brave", "work");
        touch_profile(tmp.path(), "chromium", "work");
        let err = info("work", None, Some(tmp.path())).err().unwrap();
        assert_eq!(err.error_code, ErrorCode::InvalidArgument);
        assert!(err.detail.contains("--backend"));

        let chromium = info("work", Some("chromium"), Some(tmp.path())).unwrap();
        assert_eq!(chromium.backend, "chromium");
    }

    #[test]
    fn same_name_different_backends_have_independent_locks_and_downloads() {
        let tmp = tempfile::tempdir().unwrap();
        let brave = touch_profile(tmp.path(), "brave", "work");
        let chromium = touch_profile(tmp.path(), "chromium", "work");
        let _guard = lock::Guard::acquire(&brave).unwrap();

        assert!(
            lock_status("work", Some("brave"), Some(tmp.path()))
                .unwrap()
                .locked
        );
        assert!(
            !lock_status("work", Some("chromium"), Some(tmp.path()))
                .unwrap()
                .locked
        );

        std::fs::create_dir_all(brave.join("downloads")).unwrap();
        std::fs::create_dir_all(chromium.join("downloads")).unwrap();
        std::fs::write(brave.join("downloads").join("brave.txt"), "a").unwrap();
        std::fs::write(chromium.join("downloads").join("chromium.txt"), "b").unwrap();
        let brave_downloads = downloads("work", Some("brave"), Some(tmp.path())).unwrap();
        let chromium_downloads = downloads("work", Some("chromium"), Some(tmp.path())).unwrap();
        assert_eq!(brave_downloads[0].filename, "brave.txt");
        assert_eq!(chromium_downloads[0].filename, "chromium.txt");
    }

    #[test]
    fn metadata_backend_mismatch_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("brave").join("work");
        std::fs::create_dir_all(&dir).unwrap();
        let meta = meta::ProfileMeta::new("work", "chromium");
        std::fs::write(
            dir.join("afhttp-profile.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
        let err = info("work", Some("brave"), Some(tmp.path())).err().unwrap();
        assert_eq!(err.error_code, ErrorCode::IoError);
        assert!(!err.retryable);
        assert!(err.detail.contains("backend"));
    }

    /// The name is valid; only the file is wrong, so the code must not blame
    /// the name.
    #[test]
    fn corrupt_metadata_is_not_reported_as_an_invalid_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("brave").join("work");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("afhttp-profile.json"), b"{not json").unwrap();
        let err = info("work", Some("brave"), Some(tmp.path())).err().unwrap();
        assert_eq!(err.error_code, ErrorCode::IoError);
        assert!(!err.retryable);

        let mut meta = meta::ProfileMeta::new("other", "brave");
        std::fs::write(
            dir.join("afhttp-profile.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
        let err = info("work", Some("brave"), Some(tmp.path())).err().unwrap();
        assert_eq!(err.error_code, ErrorCode::IoError);
        assert!(err.detail.contains("name"));

        meta.name = "work".into();
        meta.schema_version = 1;
        std::fs::write(
            dir.join("afhttp-profile.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
        let err = info("work", Some("brave"), Some(tmp.path())).err().unwrap();
        assert_eq!(err.error_code, ErrorCode::IoError);
    }

    /// The removal holds the lock, so a host cannot claim the profile while
    /// its data is being removed; and a held lock stops the removal before
    /// anything is touched.
    #[test]
    fn removal_holds_the_profile_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = touch_profile(tmp.path(), "chromium", "work");
        std::fs::create_dir_all(dir.join("Default")).unwrap();
        std::fs::write(dir.join("Default").join("Cookies"), b"c").unwrap();

        let held = lock::Guard::acquire(&dir).unwrap();
        let err = remove_profile_dir(&dir).unwrap_err();
        assert_eq!(err.error_code, ErrorCode::ProfileLocked);
        assert!(dir.join("Default").join("Cookies").exists());
        drop(held);

        remove_profile_dir(&dir).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn prune_skips_a_profile_locked_after_listing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = touch_profile(tmp.path(), "chromium", "work");
        let _held = lock::Guard::acquire(&dir).unwrap();
        let removed = prune(Duration::from_secs(0), false, Some(tmp.path())).unwrap();
        assert!(removed.is_empty());
        assert!(dir.exists());
    }
}
