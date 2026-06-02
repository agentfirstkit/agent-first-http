//! Lexical path helpers shared by artifact and cookie-jar resolution.

use std::path::{Component, PathBuf};

/// Make `path` absolute and collapse `.`/`..` lexically (no filesystem
/// access, no symlink resolution). Relative paths are joined onto the current
/// working directory. Used so the JSON contract always reports absolute
/// artifact and cookie-jar paths regardless of how `--out`/`--cookie-jar`
/// were passed.
#[must_use]
pub(crate) fn absolute_lexical(path: PathBuf) -> PathBuf {
    let mut out = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    };
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}
