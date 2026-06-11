use std::path::PathBuf;

use crate::host::bootstrap::ProfileChoice;
use crate::shared::error::{Error, ErrorCode};

pub(super) fn resolve_profile_dir(
    choice: &ProfileChoice,
    backend: &str,
) -> Result<
    (
        PathBuf,
        Option<tempfile::TempDir>,
        Option<crate::sdk::profile::lock::Guard>,
    ),
    Error,
> {
    match choice {
        ProfileChoice::Ephemeral => {
            let td = tempfile::Builder::new()
                .prefix("afhttp-profile-")
                .tempdir()
                .map_err(|e| {
                    Error::new(
                        ErrorCode::ProfileRootUnavailable,
                        format!("ephemeral profile tempdir: {e}"),
                    )
                })?;
            Ok((td.path().to_path_buf(), Some(td), None))
        }
        ProfileChoice::Persistent(name) => {
            crate::sdk::profile::paths::validate_name(name)?;
            let root = crate::sdk::profile::paths::default_root();
            let backend_dir = crate::sdk::profile::paths::ensure_backend_dir(&root, backend)?;
            crate::sdk::profile::paths::ensure_no_filesystem_collision(&backend_dir, name)?;
            let dir = backend_dir.join(name);
            std::fs::create_dir_all(&dir).map_err(|e| {
                Error::new(
                    ErrorCode::ProfileRootUnavailable,
                    format!("create persistent profile dir {}: {e}", dir.display()),
                )
            })?;
            let guard = crate::sdk::profile::lock::Guard::acquire(&dir)?;
            let meta_path = dir.join("afhttp-profile.json");
            let meta = if meta_path.exists() {
                let existing = crate::sdk::profile::read_profile_meta(&meta_path)?;
                crate::sdk::profile::validate_profile_meta(&existing, backend, name, &dir)?;
                existing.touch_for_host()
            } else {
                crate::sdk::profile::meta::ProfileMeta::new(name, backend)
            };
            let meta_json = serde_json::to_vec_pretty(&meta).map_err(|e| {
                Error::new(
                    ErrorCode::InternalError,
                    format!("serialize profile metadata {}: {e}", meta_path.display()),
                )
            })?;
            std::fs::write(&meta_path, meta_json).map_err(|e| {
                Error::new(
                    ErrorCode::IoError,
                    format!("write profile metadata {}: {e}", meta_path.display()),
                )
            })?;
            Ok((dir, None, Some(guard)))
        }
    }
}
