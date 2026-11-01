//! `afhttp-profile.json` metadata file schema (architecture.md §7).

use std::time::SystemTime;
#[cfg(any(feature = "host", test))]
use std::{io, path::Path};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileMeta {
    pub schema_version: u32,
    pub name: String,
    pub backend: String,
    pub created_at_rfc3339: String,
    pub last_used_at_rfc3339: String,
    pub last_host_version: String,
}

impl ProfileMeta {
    pub const SCHEMA_VERSION: u32 = 2;

    pub fn new(name: impl Into<String>, backend: impl Into<String>) -> Self {
        let now = now_rfc3339();
        Self {
            schema_version: Self::SCHEMA_VERSION,
            name: name.into(),
            backend: backend.into(),
            created_at_rfc3339: now.clone(),
            last_used_at_rfc3339: now,
            last_host_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    pub fn touch_for_host(mut self) -> Self {
        self.schema_version = Self::SCHEMA_VERSION;
        self.last_used_at_rfc3339 = now_rfc3339();
        self.last_host_version = env!("CARGO_PKG_VERSION").to_string();
        self
    }
}

/// Current UTC time as a second-precision RFC3339 string (`YYYY-MM-DDTHH:MM:SSZ`).
pub fn now_rfc3339() -> String {
    humantime::format_rfc3339_seconds(SystemTime::now()).to_string()
}

/// Publish complete metadata without ever truncating the previous file. The
/// profile lock must be held by the caller. A failure before rename leaves the
/// old file untouched; a directory-sync failure after rename leaves a complete
/// new file, whose crash durability could not be confirmed.
#[cfg(any(feature = "host", test))]
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    agent_first_data::document::write_atomic(path, bytes).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_replacement_is_complete_and_refuses_non_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metadata.json");
        write_atomic(&path, b"old").unwrap();
        write_atomic(&path, b"new complete metadata").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new complete metadata");
        assert!(write_atomic(dir.path(), b"refused").is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn now_rfc3339_is_second_precision_utc() {
        let s = now_rfc3339();
        assert!(s.ends_with('Z'));
        assert_eq!(s.len(), 20); // YYYY-MM-DDTHH:MM:SSZ
    }

    #[test]
    fn meta_round_trips_through_json() {
        let m = ProfileMeta::new("work", "brave");
        let s = serde_json::to_string(&m).unwrap_or_default();
        let back: ProfileMeta = serde_json::from_str(&s).unwrap();
        assert_eq!(back.name, "work");
        assert_eq!(back.backend, "brave");
        assert_eq!(back.schema_version, ProfileMeta::SCHEMA_VERSION);
    }
}
