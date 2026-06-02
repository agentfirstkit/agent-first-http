use std::path::PathBuf;

use crate::host::bootstrap::ProfileChoice;
use crate::shared::error::{Error, ErrorCode};

pub(super) fn resolve_profile_dir(
    choice: &ProfileChoice,
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
            crate::sdk::profile::paths::ensure_no_filesystem_collision(&root, name)?;
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).map_err(|e| {
                Error::new(
                    ErrorCode::ProfileRootUnavailable,
                    format!("create persistent profile dir {}: {e}", dir.display()),
                )
            })?;
            let guard = crate::sdk::profile::lock::Guard::acquire(&dir)?;
            let meta = crate::sdk::profile::meta::ProfileMeta::new(name);
            let meta_path = dir.join("afhttp-profile.json");
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
