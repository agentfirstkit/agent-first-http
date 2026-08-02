//! `afhttp profile` subcommand. Local profile lifecycle.

use std::path::PathBuf;

use crate::cli::output;
use crate::sdk::profile;
use crate::shared::error::Error;
use crate::shared::time::parse_duration;

#[derive(Debug)]
pub struct Args {
    pub sub: ProfileSub,
}

#[derive(Debug)]
pub enum ProfileSub {
    List(ListArgs),
    Info(InfoArgs),
    LockStatus(InfoArgs),
    Downloads(InfoArgs),
    Delete(DeleteArgs),
    Prune(PruneArgs),
    Cookies(InfoArgs),
}

#[derive(Debug)]
pub struct ListArgs {
    pub profile_root: Option<PathBuf>,
}

#[derive(Debug)]
pub struct InfoArgs {
    pub name: String,
    pub backend: Option<String>,
    pub profile_root: Option<PathBuf>,
}

#[derive(Debug)]
pub struct DeleteArgs {
    pub name: String,
    pub backend: Option<String>,
    pub confirm: String,
    pub profile_root: Option<PathBuf>,
}

#[derive(Debug)]
pub struct PruneArgs {
    pub older_than: String,
    pub dry_run: bool,
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
