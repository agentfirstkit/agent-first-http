//! Advisory profile-directory locking via `fs2::FileExt::try_lock_exclusive`.
//! The lockfile lives at `<profile>/afhttp-profile.lock` and is held for
//! the lifetime of the [`Guard`]; release happens on `Drop`. The owner's
//! PID lives in a sibling file `afhttp-profile.pid` so we can rewrite it
//! atomically (rename-over) without invalidating the fs2 lock identity.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::shared::error::{Error, ErrorCode};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockStatus {
    pub locked: bool,
    pub lockfile: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_started_at_rfc3339: Option<String>,
}

/// RAII guard around the profile lockfile. `acquire` opens or creates
/// `<profile>/afhttp-profile.lock` and takes an exclusive advisory lock
/// on it; the lock is released when the guard drops.
pub struct Guard {
    file: std::fs::File,
    profile_dir: PathBuf,
}

impl Guard {
    pub fn acquire(profile_dir: &Path) -> Result<Self, Error> {
        let path = lockfile(profile_dir);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| {
                Error::new(
                    ErrorCode::IoError,
                    format!("open lockfile {}: {e}", path.display()),
                )
            })?;
        file.try_lock_exclusive().map_err(|e| {
            Error::new(
                ErrorCode::ProfileLocked,
                format!("profile {} already locked: {e}", profile_dir.display()),
            )
        })?;
        // Write our PID to a sibling file via tempfile + rename so a racing
        // `status()` probe never reads a truncated PID. The lockfile itself
        // is not touched — that preserves fs2 lock identity across rewrites.
        let pid_string = std::process::id().to_string();
        let _ = atomic_overwrite(&pidfile(profile_dir), pid_string.as_bytes());
        Ok(Self {
            file,
            profile_dir: profile_dir.to_path_buf(),
        })
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
        // Best-effort cleanup of the PID file; the lockfile itself stays
        // so probes continue to find it.
        let _ = std::fs::remove_file(pidfile(&self.profile_dir));
    }
}

fn atomic_overwrite(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = target.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "target path has no parent",
        )
    })?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file_mut().sync_all()?;
    tmp.persist(target).map_err(|e| e.error)?;
    Ok(())
}

/// Returns true if `profile_dir`'s lockfile is currently held by another
/// process.
pub fn probe(profile_dir: &Path) -> bool {
    let path = lockfile(profile_dir);
    if !path.exists() {
        return false;
    }
    let file = match OpenOptions::new().read(true).write(true).open(&path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    match file.try_lock_exclusive() {
        Ok(()) => {
            let _ = FileExt::unlock(&file);
            false
        }
        Err(_) => true,
    }
}

pub fn status(profile_dir: &Path) -> LockStatus {
    let lockfile_path = lockfile(profile_dir);
    let locked = probe(profile_dir);
    let owner_pid = if locked {
        std::fs::read_to_string(pidfile(profile_dir))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
    } else {
        None
    };
    LockStatus {
        locked,
        lockfile: lockfile_path,
        owner_pid,
        owner_started_at_rfc3339: None,
    }
}

fn lockfile(profile_dir: &Path) -> PathBuf {
    profile_dir.join("afhttp-profile.lock")
}

fn pidfile(profile_dir: &Path) -> PathBuf {
    profile_dir.join("afhttp-profile.pid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_and_drop_releases_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let guard = Guard::acquire(dir).unwrap();
        assert!(probe(dir));
        drop(guard);
        assert!(!probe(dir));
    }

    #[test]
    fn second_acquire_returns_profile_locked() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let _g = Guard::acquire(dir).unwrap();
        let err = Guard::acquire(dir).err().unwrap();
        assert_eq!(err.error_code, ErrorCode::ProfileLocked);
    }

    #[test]
    fn status_returns_owner_pid_when_locked() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let _g = Guard::acquire(dir).unwrap();
        let st = status(dir);
        assert!(st.locked);
        assert_eq!(st.owner_pid, Some(std::process::id()));
    }

    #[test]
    fn pid_writes_are_atomic_under_concurrent_reads() {
        // While one thread repeatedly acquires + drops a Guard (writing the
        // PID then deleting the pidfile on Drop), other threads poll the
        // raw pidfile bytes. Without atomic rename the readers could catch
        // a half-written PID; with it, every read must be either absent or
        // the exact full PID string.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let expected_pid = std::process::id().to_string();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let writer_stop = stop.clone();
        let writer_dir = dir.clone();
        let writer = std::thread::spawn(move || {
            while !writer_stop.load(std::sync::atomic::Ordering::Relaxed) {
                let g = Guard::acquire(&writer_dir).unwrap();
                drop(g);
            }
        });

        let pid_path = pidfile(&dir);
        let mut observed_pid = false;
        let mut observed_absent = false;
        for _ in 0..200 {
            match std::fs::read(&pid_path) {
                Ok(bytes) => {
                    let s = String::from_utf8_lossy(&bytes);
                    if s == expected_pid {
                        observed_pid = true;
                    } else {
                        panic!("non-atomic pidfile observed: {s:?}");
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    observed_absent = true;
                }
                Err(_) => {}
            }
            std::thread::sleep(std::time::Duration::from_micros(50));
        }

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        writer.join().unwrap();

        assert!(
            observed_pid || observed_absent,
            "expected at least one snapshot of the pidfile"
        );
    }
}
