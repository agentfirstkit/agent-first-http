//! `afhttp profile` subcommand. Local profile lifecycle.

use std::path::PathBuf;

use clap::{Args as ClapArgs, Subcommand};

use crate::cli::output;
use crate::sdk::profile;
use crate::shared::error::Error;
use crate::shared::time::parse_duration;

#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub sub: ProfileSub,
}

#[derive(Subcommand, Debug)]
pub enum ProfileSub {
    /// List on-disk profiles under the profiles root.
    List(ListArgs),
    /// Show metadata for one profile (size, last use, lock state).
    Info(InfoArgs),
    /// Report whether a profile is currently locked by a running host.
    LockStatus(InfoArgs),
    /// List files captured in the profile's browser download directory.
    Downloads(InfoArgs),
    /// Delete a profile and all of its on-disk state.
    Delete(DeleteArgs),
    /// Delete profiles whose last use is older than a cutoff.
    Prune(PruneArgs),
    /// Show the non-expired cookies in a profile's jar (values redacted).
    Cookies(InfoArgs),
}

#[derive(ClapArgs, Debug)]
pub struct ListArgs {
    /// Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles`.
    #[arg(long)]
    pub profile_root: Option<PathBuf>,
}

#[derive(ClapArgs, Debug)]
pub struct InfoArgs {
    /// Profile name.
    pub name: String,
    /// Profile backend scope (for example chromium, brave, camoufox).
    /// Required when the same profile name exists under multiple backends.
    #[arg(long)]
    pub backend: Option<String>,
    /// Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles`.
    #[arg(long)]
    pub profile_root: Option<PathBuf>,
}

#[derive(ClapArgs, Debug)]
pub struct DeleteArgs {
    /// Profile name to delete.
    pub name: String,
    /// Profile backend scope (for example chromium, brave, camoufox).
    /// Required when the same profile name exists under multiple backends.
    #[arg(long)]
    pub backend: Option<String>,
    /// Confirmation guard: must equal the profile name for the delete to proceed.
    #[arg(long)]
    pub confirm: String,
    /// Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles`.
    #[arg(long)]
    pub profile_root: Option<PathBuf>,
}

#[derive(ClapArgs, Debug)]
pub struct PruneArgs {
    /// Age cutoff (e.g. `30d`, `12h`); profiles last used before this are removed.
    #[arg(long)]
    pub older_than: String,
    /// Report what would be deleted without deleting anything.
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
    /// Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles`.
    #[arg(long)]
    pub profile_root: Option<PathBuf>,
}

pub async fn run(args: Args) -> Result<(), Error> {
    match args.sub {
        ProfileSub::List(a) => {
            let root = profile_root_for_output(a.profile_root.as_deref());
            let entries = profile::list(a.profile_root.as_deref())?;
            output::emit(
                "profile_list",
                &serde_json::json!({
                    "profile_root": root.display().to_string(),
                    "profiles": entries,
                }),
            )
        }
        ProfileSub::Info(a) => {
            let entry = profile::info(&a.name, a.backend.as_deref(), a.profile_root.as_deref())?;
            output::emit("profile_info", &entry)
        }
        ProfileSub::LockStatus(a) => {
            let status =
                profile::lock_status(&a.name, a.backend.as_deref(), a.profile_root.as_deref())?;
            output::emit("profile_lock_status", &status)
        }
        ProfileSub::Downloads(a) => {
            let entry = profile::info(&a.name, a.backend.as_deref(), a.profile_root.as_deref())?;
            let download_dir = entry.path.join("downloads");
            let download_dir = download_dir.canonicalize().unwrap_or(download_dir);
            let downloads =
                profile::downloads(&a.name, a.backend.as_deref(), a.profile_root.as_deref())?;
            output::emit(
                "profile_downloads",
                &serde_json::json!({
                    "backend": entry.backend,
                    "name": a.name,
                    "download_dir": download_dir.display().to_string(),
                    "downloads": downloads,
                }),
            )
        }
        ProfileSub::Delete(a) => {
            let entry = profile::info(&a.name, a.backend.as_deref(), a.profile_root.as_deref())?;
            profile::delete(
                &a.name,
                &a.confirm,
                a.backend.as_deref(),
                a.profile_root.as_deref(),
            )?;
            output::emit(
                "profile_delete",
                &serde_json::json!({"backend": entry.backend, "name": a.name, "deleted": true}),
            )
        }
        ProfileSub::Prune(a) => {
            let root = profile_root_for_output(a.profile_root.as_deref());
            let older_than = parse_duration(&a.older_than)?;
            let removed = profile::prune(older_than, a.dry_run, a.profile_root.as_deref())?;
            output::emit(
                "profile_prune",
                &serde_json::json!({
                    "profile_root": root.display().to_string(),
                    "dry_run": a.dry_run,
                    "profiles": removed,
                }),
            )
        }
        ProfileSub::Cookies(a) => {
            let entry = profile::info(&a.name, a.backend.as_deref(), a.profile_root.as_deref())?;
            let jar_path = entry.path.join("cookies.jar.json");
            let jar = crate::sdk::profile::cookie_jar::CookieJar::load(&jar_path)?;
            let cookies = jar.cookies_redacted();
            output::emit(
                "profile_cookies",
                &serde_json::json!({
                    "backend": entry.backend,
                    "name": a.name,
                    "jar_path": jar_path.display().to_string(),
                    "count": cookies.len(),
                    "cookies": cookies,
                }),
            )
        }
    }
}

fn profile_root_for_output(root: Option<&std::path::Path>) -> PathBuf {
    root.map(PathBuf::from)
        .unwrap_or_else(profile::paths::default_root)
}
