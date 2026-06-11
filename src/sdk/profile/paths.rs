//! Profile-root resolution + name validation.
//!
//! Root: `$XDG_DATA_HOME/afhttp/profiles/` (or `$HOME/.local/share/afhttp/profiles/`).
//! Persistent profiles are scoped by backend family under
//! `<root>/<backend>/<name>`.
//! Name validation rejects anything that would escape the root or break
//! filesystem traversal.

use std::path::{Path, PathBuf};

use crate::shared::error::{Error, ErrorCode};

pub const KNOWN_BACKENDS: &[&str] = &[
    "chromium",
    "chrome",
    "chrome-headless-shell",
    "fingerprint-chromium",
    "edge",
    "brave",
    "lightpanda",
    "camoufox",
];

/// Resolve the default profile root.
pub fn default_root() -> PathBuf {
    if let Some(dir) = dirs::data_dir() {
        return dir.join("afhttp").join("profiles");
    }
    PathBuf::from("./afhttp-profiles")
}

/// Validate a profile name. Allowed: ASCII alphanumerics plus `-`, `_`,
/// `.` (but never starting with `.`), max 64 chars. Rejects Windows
/// reserved device names so a profile created on Linux cannot become a
/// poisoned path when the directory is read on Windows.
///
/// This is the structural check only — it does not touch the filesystem.
/// Callers that need to ensure the target directory doesn't collide with
/// an existing non-directory entry should chain
/// [`ensure_no_filesystem_collision`] after this.
pub fn validate_name(name: &str) -> Result<(), Error> {
    if name.is_empty() {
        return Err(Error::new(
            ErrorCode::ProfileInvalidName,
            "profile name cannot be empty",
        ));
    }
    if name.len() > 64 {
        return Err(Error::new(
            ErrorCode::ProfileInvalidName,
            "profile name longer than 64 characters",
        ));
    }
    if name.starts_with('.') {
        return Err(Error::new(
            ErrorCode::ProfileInvalidName,
            "profile name cannot start with '.'",
        ));
    }
    for c in name.chars() {
        let ok = c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.';
        if !ok {
            return Err(Error::new(
                ErrorCode::ProfileInvalidName,
                format!("profile name contains forbidden character: {c:?}"),
            ));
        }
    }
    if is_windows_reserved(name) {
        return Err(Error::new(
            ErrorCode::ProfileInvalidName,
            format!(
                "profile name {name:?} matches a Windows reserved device \
                 name; pick another to keep profiles portable"
            ),
        ));
    }
    Ok(())
}

pub fn validate_backend_key(backend: &str) -> Result<(), Error> {
    if KNOWN_BACKENDS.contains(&backend) {
        Ok(())
    } else {
        Err(Error::new(
            ErrorCode::InvalidArgument,
            format!(
                "unknown profile backend {backend:?}; expected one of {}",
                KNOWN_BACKENDS.join(", ")
            ),
        ))
    }
}

/// Windows reserved device names per [MSDN naming
/// conventions](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file).
/// A reserved match is the bare name *or* the bare name followed by
/// `.<anything>` (e.g. `con.log` is also reserved on Windows). Case
/// insensitive.
fn is_windows_reserved(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    let base = name.split_once('.').map(|(b, _)| b).unwrap_or(name);
    let lower = base.to_ascii_lowercase();
    RESERVED.iter().any(|r| *r == lower)
}

/// Ensure that `<root>/<name>` either does not exist or is already a
/// directory. Rejects the case where a regular file or symlink occupies
/// the slot — `afhttp host --profile foo` should not silently start over
/// a non-directory entry the user (or another tool) put there.
pub fn ensure_no_filesystem_collision(root: &Path, name: &str) -> Result<(), Error> {
    let target = root.join(name);
    let md = match std::fs::symlink_metadata(&target) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(Error::new(
                ErrorCode::ProfileRootUnavailable,
                format!("stat {}: {e}", target.display()),
            ));
        }
    };
    if md.is_dir() {
        return Ok(());
    }
    let kind = if md.file_type().is_symlink() {
        "symlink"
    } else if md.is_file() {
        "regular file"
    } else {
        "non-directory entry"
    };
    Err(Error::new(
        ErrorCode::ProfileInvalidName,
        format!(
            "profile name {name:?} would collide with a {kind} at {} — \
             remove or rename it before reusing the name",
            target.display()
        ),
    ))
}

pub fn ensure_backend_dir(root: &Path, backend: &str) -> Result<PathBuf, Error> {
    validate_backend_key(backend)?;
    let dir = root.join(backend);
    match std::fs::symlink_metadata(&dir) {
        Ok(md) if md.is_dir() => Ok(dir),
        Ok(md) => {
            let kind = if md.file_type().is_symlink() {
                "symlink"
            } else if md.is_file() {
                "regular file"
            } else {
                "non-directory entry"
            };
            Err(Error::new(
                ErrorCode::ProfileRootUnavailable,
                format!(
                    "profile backend {backend:?} would collide with a {kind} at {}",
                    dir.display()
                ),
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(&dir).map_err(|e| {
                Error::new(
                    ErrorCode::ProfileRootUnavailable,
                    format!("create profile backend dir {}: {e}", dir.display()),
                )
            })?;
            Ok(dir)
        }
        Err(e) => Err(Error::new(
            ErrorCode::ProfileRootUnavailable,
            format!("stat {}: {e}", dir.display()),
        )),
    }
}

/// Join the root + name into a concrete profile directory path. Performs
/// structural validation only; callers that touch the filesystem should
/// also run [`ensure_no_filesystem_collision`].
pub fn join_root_name(root: &Path, name: &str) -> Result<PathBuf, Error> {
    validate_name(name)?;
    Ok(root.join(name))
}

pub fn join_root_backend_name(root: &Path, backend: &str, name: &str) -> Result<PathBuf, Error> {
    validate_backend_key(backend)?;
    validate_name(name)?;
    Ok(root.join(backend).join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal() {
        assert_eq!(
            validate_name("..").err().map(|e| e.error_code),
            Some(ErrorCode::ProfileInvalidName)
        );
        assert_eq!(
            validate_name("a/b").err().map(|e| e.error_code),
            Some(ErrorCode::ProfileInvalidName)
        );
        assert_eq!(
            validate_name(".hidden").err().map(|e| e.error_code),
            Some(ErrorCode::ProfileInvalidName)
        );
    }

    #[test]
    fn rejects_empty_and_long() {
        assert!(validate_name("").is_err());
        let big = "a".repeat(65);
        assert!(validate_name(&big).is_err());
    }

    #[test]
    fn accepts_reasonable_names() {
        for n in ["work", "main_2026", "alpha-rc.1", "x"] {
            validate_name(n).unwrap_or_else(|e| panic!("{n}: {e}"));
        }
    }

    #[test]
    fn rejects_windows_reserved_device_names() {
        // Bare names.
        for n in ["con", "PRN", "Nul", "COM1", "lpt9", "AUX"] {
            assert_eq!(
                validate_name(n).err().map(|e| e.error_code),
                Some(ErrorCode::ProfileInvalidName),
                "{n} should be rejected",
            );
        }
        // Reserved-with-extension forms (`con.log` is still a device on Windows).
        assert!(validate_name("CON.log").is_err());
        assert!(validate_name("nul.txt").is_err());
        // Lookalikes that share a prefix must still be accepted.
        for n in ["console", "containers", "comet1", "nullify"] {
            validate_name(n).unwrap_or_else(|e| panic!("{n}: {e}"));
        }
    }

    #[test]
    fn rejects_unicode_control_and_non_ascii() {
        // The ASCII-alphanumeric allowlist already catches these; this is
        // a load-bearing sanity test that documents the contract.
        assert!(validate_name("work\u{0000}").is_err()); // NUL byte
        assert!(validate_name("work\u{0007}").is_err()); // BEL
        assert!(validate_name("работа").is_err()); // Cyrillic
        assert!(validate_name("作業").is_err()); // CJK
        assert!(validate_name("café").is_err()); // accented Latin
                                                 // Windows-style separator.
        assert!(validate_name("a\\b").is_err());
    }

    #[test]
    fn ensure_no_filesystem_collision_accepts_missing_and_dir_targets() {
        let tmp = tempfile::tempdir().unwrap();
        // Missing target — fine, will be mkdir'd later.
        ensure_no_filesystem_collision(tmp.path(), "fresh").unwrap();
        // Existing directory — also fine, this is the reuse case.
        std::fs::create_dir(tmp.path().join("reused")).unwrap();
        ensure_no_filesystem_collision(tmp.path(), "reused").unwrap();
    }

    #[test]
    fn ensure_no_filesystem_collision_rejects_file_collision() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("squatter"), b"not a dir").unwrap();
        let err = ensure_no_filesystem_collision(tmp.path(), "squatter")
            .err()
            .unwrap();
        assert_eq!(err.error_code, ErrorCode::ProfileInvalidName);
        assert!(
            err.detail.contains("regular file"),
            "detail should name the collision kind: {}",
            err.detail
        );
    }

    #[cfg(unix)]
    #[test]
    fn ensure_no_filesystem_collision_rejects_symlink_collision() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("real");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, tmp.path().join("alias")).unwrap();
        let err = ensure_no_filesystem_collision(tmp.path(), "alias")
            .err()
            .unwrap();
        assert_eq!(err.error_code, ErrorCode::ProfileInvalidName);
        assert!(err.detail.contains("symlink"));
    }

    #[test]
    fn validate_name_round_trips_legal_inputs() {
        // Deterministic fuzz over the legal alphabet. Every accepted name
        // must round-trip through `join_root_name` to the exact filesystem
        // path we expect — no NFC drift, no character substitution, no
        // surprises in path joining.
        let alphabet: &[u8] = b"abcdefghijklmnopqrstuvwxyz\
                                 ABCDEFGHIJKLMNOPQRSTUVWXYZ\
                                 0123456789-_.";
        let root = std::path::Path::new("/tmp/afhttp");
        for seed in 0u32..2000 {
            let len = ((seed >> 8) % 12 + 1) as usize;
            let mut buf = Vec::with_capacity(len);
            let mut s = seed.wrapping_mul(2654435761);
            for _ in 0..len {
                s = s.wrapping_mul(2654435761).wrapping_add(1);
                buf.push(alphabet[(s as usize) % alphabet.len()]);
            }
            let name = std::str::from_utf8(&buf).unwrap();
            if validate_name(name).is_ok() {
                let p = join_root_name(root, name).unwrap();
                assert_eq!(p, root.join(name), "join_root_name drift for {name:?}");
            }
        }
    }

    #[test]
    fn backend_scoped_join_uses_backend_then_name() {
        let root = std::path::Path::new("/tmp/afhttp/profiles");
        assert_eq!(
            join_root_backend_name(root, "brave", "work").unwrap(),
            root.join("brave").join("work")
        );
        assert_eq!(
            join_root_backend_name(root, "chromium", "work").unwrap(),
            root.join("chromium").join("work")
        );
        assert!(join_root_backend_name(root, "firefox", "work").is_err());
    }

    #[test]
    fn default_root_is_under_data_dir() {
        let p = default_root();
        let s = p.to_string_lossy().to_string();
        assert!(s.contains("afhttp"));
        assert!(s.ends_with("profiles") || s.contains("profiles"));
    }
}
