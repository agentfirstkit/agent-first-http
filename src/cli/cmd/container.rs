//! `afhttp container` subcommand. Builds the host image and runs it under
//! Docker, Podman, or Apple Container — one command to stand up a long-lived
//! afhttp *host* locally, the orchestration counterpart to `afhttp host` (the
//! in-container browser process). It embeds the canonical `container/docker/
//! Dockerfile` and by default selects its `downloader` stage, which pulls the
//! matching prebuilt release (version hard-pinned to this binary) — so a
//! brew-only user needs no source tree. `--from-source` instead selects the
//! `builder` stage to compile from a checkout. See docs/deployment.md.

use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Args as ClapArgs, Subcommand, ValueEnum};
use serde::Serialize;

use crate::cli::output;
use crate::shared::error::{Error, ErrorCode};

/// Build context embedded in the binary and written to the cache dir at
/// `install` time. It is the SAME canonical Dockerfile used for from-source
/// builds — the embedded path just selects its `downloader` stage via
/// `--build-arg AFHTTP_BIN_FROM=downloader` (single source of truth, no fork).
const DOCKERFILE: &str = include_str!("../../../container/docker/Dockerfile");
const INSTALL_BACKENDS: &str = include_str!("../../../container/docker/install-backends.sh");
const ENTRYPOINT: &str = include_str!("../../../container/docker/entrypoint.sh");

/// This binary's version — the image downloads exactly this release.
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Default container name and image repository.
const DEFAULT_NAME: &str = "afhttp-host";
const IMAGE_REPO: &str = "afhttp-host";

#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub sub: ContainerSub,
}

#[derive(Subcommand, Debug)]
pub enum ContainerSub {
    /// Build the host image if missing and run the container; print the client command.
    Install(InstallArgs),
    /// Stop and remove the container (--purge also removes the image and cache).
    Uninstall(UninstallArgs),
    /// Report whether the host is running, with its endpoint and client command.
    Status(StatusArgs),
    /// Stream the container logs (raw passthrough, not a JSON envelope).
    Logs(LogsArgs),
}

/// Flags shared by every subcommand.
#[derive(ClapArgs, Debug)]
pub struct CommonArgs {
    /// Container runtime: docker, podman, or apple (auto-detected if omitted).
    #[arg(long, value_enum)]
    pub runtime: Option<RuntimeArg>,
    /// Container name.
    #[arg(long, default_value = DEFAULT_NAME)]
    pub name: String,
}

#[derive(ClapArgs, Debug)]
pub struct InstallArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Host CDP port, published on 127.0.0.1.
    #[arg(long, default_value_t = 9222)]
    pub port: u16,
    /// Profile name inside the container.
    #[arg(long, default_value = "work")]
    pub profile: String,
    /// Chromium /dev/shm size.
    #[arg(long = "shm-size", default_value = "1g")]
    pub shm_size: String,
    /// Optional backend to build in (repeatable): chrome-headless-shell,
    /// lightpanda, fingerprint-chromium, camoufox, kasmvnc.
    #[arg(long = "with", value_name = "BACKEND")]
    pub with: Vec<String>,
    /// Rebuild the image even if it already exists.
    #[arg(long)]
    pub rebuild: bool,
    /// Build the full image from a source checkout (container/docker/Dockerfile)
    /// instead of downloading the prebuilt release. Needs the source tree.
    #[arg(long = "from-source")]
    pub from_source: bool,
    /// Source checkout to build from with --from-source (default: current dir).
    #[arg(long, value_name = "DIR")]
    pub context: Option<String>,
    /// Extra args passed through to `afhttp host` inside the container.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub host_args: Vec<String>,
}

#[derive(ClapArgs, Debug)]
pub struct UninstallArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Also remove the built image and the cached build context.
    #[arg(long)]
    pub purge: bool,
}

#[derive(ClapArgs, Debug)]
pub struct StatusArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Published host port, used to format the endpoint and client command.
    #[arg(long, default_value_t = 9222)]
    pub port: u16,
}

#[derive(ClapArgs, Debug)]
pub struct LogsArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Follow the log output.
    #[arg(long, short = 'f')]
    pub follow: bool,
}

/// CLI spelling of the runtime selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum RuntimeArg {
    Docker,
    Podman,
    Apple,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Runtime {
    Docker,
    Podman,
    Apple,
}

impl From<RuntimeArg> for Runtime {
    fn from(arg: RuntimeArg) -> Self {
        match arg {
            RuntimeArg::Docker => Runtime::Docker,
            RuntimeArg::Podman => Runtime::Podman,
            RuntimeArg::Apple => Runtime::Apple,
        }
    }
}

impl Runtime {
    /// The runtime's CLI binary name.
    fn bin(self) -> &'static str {
        match self {
            Runtime::Docker => "docker",
            Runtime::Podman => "podman",
            Runtime::Apple => "container",
        }
    }

    /// Human label used in output and errors.
    fn label(self) -> &'static str {
        match self {
            Runtime::Docker => "docker",
            Runtime::Podman => "podman",
            Runtime::Apple => "apple",
        }
    }
}

pub async fn run(args: Args) -> Result<(), Error> {
    match args.sub {
        ContainerSub::Install(a) => install(a).await,
        ContainerSub::Uninstall(a) => uninstall(a),
        ContainerSub::Status(a) => status(a).await,
        ContainerSub::Logs(a) => logs(a),
    }
}

// ── install ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct InstallResult {
    runtime: &'static str,
    image: String,
    container: String,
    endpoint: String,
    profile: String,
    token: String,
    client_command: String,
    backends: Vec<String>,
}

async fn install(args: InstallArgs) -> Result<(), Error> {
    let runtime = resolve_runtime(args.common.runtime)?;
    let backends = resolve_backends(&args.with)?;
    let image = image_tag();

    start_daemon(runtime);

    // --from-source always rebuilds (the canonical Dockerfile compiles afhttp);
    // the embedded path reuses a cached image unless --rebuild is set.
    if args.from_source {
        let ctx = resolve_source_context(args.context.as_deref())?;
        let build = build_args(
            &image,
            runtime,
            BuildSource::FromSource { ctx: &ctx },
            &backends,
        );
        exec_inherit(runtime.bin(), &build)?;
    } else if args.rebuild || !image_exists(runtime, &image) {
        let ctx = write_build_context()?;
        let target = target_triple(runtime, std::env::consts::ARCH);
        let build = build_args(
            &image,
            runtime,
            BuildSource::Embedded { ctx: &ctx, target },
            &backends,
        );
        exec_inherit(runtime.bin(), &build).map_err(|_| build_failed_error(target))?;
    }

    // Recreate cleanly. The profile + token live in the named volume, so the
    // token is stable across recreation.
    let _ = capture(runtime.bin(), &["stop".into(), args.common.name.clone()]);
    let _ = capture(runtime.bin(), &["rm".into(), args.common.name.clone()]);

    let run = run_args(
        &args.common.name,
        &image,
        args.port,
        &args.profile,
        &args.shm_size,
        &args.host_args,
    );
    exec_inherit(runtime.bin(), &run)?;

    let token = read_token(runtime, &args.common.name).await?;
    let endpoint = endpoint_url(args.port);
    output::emit(
        "container_install",
        &InstallResult {
            runtime: runtime.label(),
            image,
            container: args.common.name.clone(),
            endpoint,
            profile: args.profile.clone(),
            client_command: client_command(args.port, &token),
            token,
            backends: backends.iter().map(|b| b.name.to_string()).collect(),
        },
    )
}

// ── uninstall ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct UninstallResult {
    runtime: &'static str,
    container: String,
    removed: bool,
    image_removed: bool,
    purged: bool,
}

fn uninstall(args: UninstallArgs) -> Result<(), Error> {
    let runtime = resolve_runtime(args.common.runtime)?;
    let _ = capture(runtime.bin(), &["stop".into(), args.common.name.clone()]);
    let removed = capture(runtime.bin(), &["rm".into(), args.common.name.clone()])
        .map(|o| o.status.success())
        .unwrap_or(false);

    let mut image_removed = false;
    if args.purge {
        let image = image_tag();
        image_removed = capture(runtime.bin(), &["rmi".into(), image])
            .map(|o| o.status.success())
            .unwrap_or(false);
        if let Ok(ctx) = cache_context_dir() {
            let _ = std::fs::remove_dir_all(&ctx);
        }
    }

    output::emit(
        "container_uninstall",
        &UninstallResult {
            runtime: runtime.label(),
            container: args.common.name,
            removed,
            image_removed,
            purged: args.purge,
        },
    )
}

// ── status ─────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct StatusResult {
    runtime: &'static str,
    container: String,
    running: bool,
    endpoint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_command: Option<String>,
}

async fn status(args: StatusArgs) -> Result<(), Error> {
    let runtime = resolve_runtime(args.common.runtime)?;
    let running = container_running(runtime, &args.common.name);
    let endpoint = endpoint_url(args.port);

    let token = if running {
        read_token(runtime, &args.common.name).await.ok()
    } else {
        None
    };
    let client_command = token.as_deref().map(|t| client_command(args.port, t));

    output::emit(
        "container_status",
        &StatusResult {
            runtime: runtime.label(),
            container: args.common.name,
            running,
            endpoint,
            token,
            client_command,
        },
    )
}

// ── logs ───────────────────────────────────────────────────────────────────

fn logs(args: LogsArgs) -> Result<(), Error> {
    let runtime = resolve_runtime(args.common.runtime)?;
    let mut argv: Vec<String> = vec!["logs".into()];
    if args.follow {
        argv.push("-f".into());
    }
    argv.push(args.common.name);
    exec_inherit(runtime.bin(), &argv)
}

// ── runtime resolution ───────────────────────────────────────────────────────

fn resolve_runtime(explicit: Option<RuntimeArg>) -> Result<Runtime, Error> {
    if let Some(r) = explicit {
        return Ok(r.into());
    }
    if let Some(v) = std::env::var_os("AFHTTP_CONTAINER_RUNTIME") {
        return runtime_from_str(v.to_string_lossy().trim());
    }
    if on_path("docker") {
        Ok(Runtime::Docker)
    } else if on_path("podman") {
        Ok(Runtime::Podman)
    } else if on_path("container") {
        Ok(Runtime::Apple)
    } else {
        Err(Error::new(
            ErrorCode::InvalidArgument,
            "no container runtime found: install Docker, Podman, or Apple `container`, or pass --runtime",
        ))
    }
}

fn runtime_from_str(value: &str) -> Result<Runtime, Error> {
    match value {
        "docker" => Ok(Runtime::Docker),
        "podman" => Ok(Runtime::Podman),
        "apple" | "container" => Ok(Runtime::Apple),
        other => Err(Error::new(
            ErrorCode::InvalidArgument,
            format!("invalid container runtime '{other}': expected docker, podman, or apple"),
        )),
    }
}

fn on_path(bin: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file())
}

/// Apple's runtime needs its daemon started first; on Docker this is a no-op.
/// Best-effort — a real failure surfaces at the build step.
fn start_daemon(runtime: Runtime) {
    if runtime == Runtime::Apple {
        let _ = capture(runtime.bin(), &["system".into(), "start".into()]);
    }
}

// ── arg builders (pure, unit-tested) ─────────────────────────────────────────

fn image_tag() -> String {
    format!("{IMAGE_REPO}:{VERSION}")
}

fn volume_name(name: &str) -> String {
    format!("{name}-data")
}

fn endpoint_url(port: u16) -> String {
    format!("ws://127.0.0.1:{port}")
}

fn client_command(port: u16, token: &str) -> String {
    format!(
        "afhttp fetch https://example.com --endpoint-url ws://127.0.0.1:{port} --token-secret {token}"
    )
}

/// The Linux target triple for the image arch. Apple Container always runs
/// linux/arm64; Docker and Podman match the host arch.
fn target_triple(runtime: Runtime, host_arch: &str) -> &'static str {
    match runtime {
        Runtime::Apple => "aarch64-unknown-linux-gnu",
        Runtime::Docker | Runtime::Podman => match host_arch {
            "aarch64" | "arm64" => "aarch64-unknown-linux-gnu",
            _ => "x86_64-unknown-linux-gnu",
        },
    }
}

/// A resolved optional backend: the `--with` name plus its Dockerfile ARG.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Backend {
    name: &'static str,
    build_arg: &'static str,
}

const BACKENDS: [Backend; 5] = [
    Backend {
        name: "chrome-headless-shell",
        build_arg: "WITH_CHROME_HEADLESS_SHELL",
    },
    Backend {
        name: "lightpanda",
        build_arg: "WITH_LIGHTPANDA",
    },
    Backend {
        name: "fingerprint-chromium",
        build_arg: "WITH_FINGERPRINT_CHROMIUM",
    },
    Backend {
        name: "camoufox",
        build_arg: "WITH_CAMOUFOX",
    },
    Backend {
        name: "kasmvnc",
        build_arg: "WITH_KASMVNC",
    },
];

fn resolve_backends(names: &[String]) -> Result<Vec<Backend>, Error> {
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let backend = BACKENDS.iter().find(|b| b.name == name).ok_or_else(|| {
            Error::new(
                ErrorCode::InvalidArgument,
                format!(
                    "unknown backend '{name}': expected one of {}",
                    BACKENDS
                        .iter()
                        .map(|b| b.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
        })?;
        if !out.contains(backend) {
            out.push(*backend);
        }
    }
    Ok(out)
}

/// Which `AFHTTP_BIN_FROM` stage of the canonical Dockerfile provides the binary.
/// `Embedded` selects the `downloader` stage (prebuilt release, the default
/// `container install` path); `FromSource` selects the `builder` stage (compile
/// from a checkout). Both build the same `container/docker/Dockerfile`.
enum BuildSource<'a> {
    Embedded { ctx: &'a Path, target: &'a str },
    FromSource { ctx: &'a Path },
}

fn build_args(
    image: &str,
    runtime: Runtime,
    source: BuildSource,
    backends: &[Backend],
) -> Vec<String> {
    let mut a: Vec<String> = vec!["build".into()];
    if runtime == Runtime::Apple {
        a.push("--platform".into());
        a.push("linux/arm64".into());
    }
    let ctx = match source {
        BuildSource::Embedded { ctx, target } => {
            a.push("--build-arg".into());
            a.push("AFHTTP_BIN_FROM=downloader".into());
            a.push("--build-arg".into());
            a.push(format!("AFHTTP_VERSION={VERSION}"));
            a.push("--build-arg".into());
            a.push(format!("AFHTTP_TARGET={target}"));
            ctx
        }
        BuildSource::FromSource { ctx } => {
            a.push("--build-arg".into());
            a.push("AFHTTP_BIN_FROM=builder".into());
            ctx
        }
    };
    for b in backends {
        a.push("--build-arg".into());
        a.push(format!("{}=1", b.build_arg));
    }
    a.push("-t".into());
    a.push(image.to_string());
    a.push("-f".into());
    a.push(
        ctx.join("container/docker/Dockerfile")
            .to_string_lossy()
            .into_owned(),
    );
    a.push(ctx.to_string_lossy().into_owned());
    a
}

/// Resolve and validate the source checkout for `--from-source`.
fn resolve_source_context(arg: Option<&str>) -> Result<PathBuf, Error> {
    let dir = match arg {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir()
            .map_err(|e| Error::new(ErrorCode::IoError, format!("cannot read current dir: {e}")))?,
    };
    let dockerfile = dir.join("container/docker/Dockerfile");
    if !dockerfile.is_file() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            format!(
                "--from-source needs a source checkout: {} not found \
                 (run from the spore root or pass --context <dir>)",
                dockerfile.display()
            ),
        ));
    }
    Ok(dir)
}

fn run_args(
    name: &str,
    image: &str,
    port: u16,
    profile: &str,
    shm_size: &str,
    host_args: &[String],
) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "run".into(),
        "-d".into(),
        "--name".into(),
        name.to_string(),
        "-v".into(),
        format!("{}:/data", volume_name(name)),
        "-e".into(),
        format!("AFHTTP_PORT={port}"),
        "-e".into(),
        format!("AFHTTP_PROFILE={profile}"),
        "--shm-size".into(),
        shm_size.to_string(),
        "-p".into(),
        format!("127.0.0.1:{port}:{port}"),
        image.to_string(),
    ];
    a.extend(host_args.iter().cloned());
    a
}

// ── process plumbing ─────────────────────────────────────────────────────────

fn spawn_error(bin: &str, err: &std::io::Error) -> Error {
    if err.kind() == std::io::ErrorKind::NotFound {
        Error::new(
            ErrorCode::InvalidArgument,
            format!("container runtime `{bin}` not found on PATH"),
        )
    } else {
        Error::new(
            ErrorCode::IoError,
            format!("spawning `{bin}` failed: {err}"),
        )
    }
}

/// Run a runtime command, inheriting stdio so the user sees build/run progress.
fn exec_inherit(bin: &str, args: &[String]) -> Result<(), Error> {
    let status = Command::new(bin)
        .args(args)
        .status()
        .map_err(|e| spawn_error(bin, &e))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::new(
            ErrorCode::InternalError,
            format!("`{bin} {}` failed ({status})", args.join(" ")),
        ))
    }
}

/// Run a runtime command capturing stdout/stderr.
fn capture(bin: &str, args: &[String]) -> Result<std::process::Output, Error> {
    Command::new(bin)
        .args(args)
        .output()
        .map_err(|e| spawn_error(bin, &e))
}

fn image_exists(runtime: Runtime, image: &str) -> bool {
    capture(
        runtime.bin(),
        &["image".into(), "inspect".into(), image.to_string()],
    )
    .map(|o| o.status.success())
    .unwrap_or(false)
}

fn container_running(runtime: Runtime, name: &str) -> bool {
    // Plain `ps` (no --format) so the check works the same on Docker and Apple.
    capture(runtime.bin(), &["ps".into()])
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(name))
        .unwrap_or(false)
}

/// Read the bearer token the entrypoint persisted to the data volume. The
/// entrypoint writes it on first start, so retry briefly after `run`.
async fn read_token(runtime: Runtime, name: &str) -> Result<String, Error> {
    let argv = vec![
        "exec".into(),
        name.to_string(),
        "cat".into(),
        "/data/afhttp/host-token".into(),
    ];
    for attempt in 0..10 {
        if let Ok(out) = capture(runtime.bin(), &argv) {
            if out.status.success() {
                let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !token.is_empty() {
                    return Ok(token);
                }
            }
        }
        if attempt < 9 {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
    }
    Err(Error::new(
        ErrorCode::InternalError,
        format!("could not read host token from container `{name}`"),
    ))
}

fn build_failed_error(target: &str) -> Error {
    Error::new(
        ErrorCode::InternalError,
        format!(
            "image build failed. If v{VERSION} has no published release asset for \
             {target}, build from a source checkout instead: \
             `afhttp container install --from-source` (or \
             docker compose -f container/docker/compose.yaml up --build)"
        ),
    )
}

// ── embedded build context ───────────────────────────────────────────────────

fn cache_context_dir() -> Result<PathBuf, Error> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .ok_or_else(|| {
            Error::new(
                ErrorCode::IoError,
                "cannot resolve cache dir: set HOME or XDG_CACHE_HOME",
            )
        })?;
    Ok(base.join("afhttp").join("container").join(VERSION))
}

fn write_build_context() -> Result<PathBuf, Error> {
    let root = cache_context_dir()?;
    // Mirror the repo's container/docker/ layout so the Dockerfile's COPY paths
    // resolve the same way they do for a from-source build. The downloader stage
    // pulls the binary over the network, so no source tree is needed here.
    let dir = root.join("container").join("docker");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("Dockerfile"), DOCKERFILE)?;
    std::fs::write(dir.join("install-backends.sh"), INSTALL_BACKENDS)?;
    std::fs::write(dir.join("entrypoint.sh"), ENTRYPOINT)?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_from_str_parses_and_rejects() {
        assert_eq!(runtime_from_str("docker").unwrap(), Runtime::Docker);
        assert_eq!(runtime_from_str("podman").unwrap(), Runtime::Podman);
        assert_eq!(runtime_from_str("apple").unwrap(), Runtime::Apple);
        assert_eq!(runtime_from_str("container").unwrap(), Runtime::Apple);
        assert_eq!(
            runtime_from_str("nerdctl").unwrap_err().error_code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn explicit_runtime_wins_over_detection() {
        assert_eq!(
            resolve_runtime(Some(RuntimeArg::Apple)).unwrap(),
            Runtime::Apple
        );
        assert_eq!(
            resolve_runtime(Some(RuntimeArg::Docker)).unwrap(),
            Runtime::Docker
        );
    }

    #[test]
    fn target_triple_tracks_runtime_and_arch() {
        assert_eq!(
            target_triple(Runtime::Apple, "x86_64"),
            "aarch64-unknown-linux-gnu"
        );
        assert_eq!(
            target_triple(Runtime::Docker, "aarch64"),
            "aarch64-unknown-linux-gnu"
        );
        assert_eq!(
            target_triple(Runtime::Docker, "x86_64"),
            "x86_64-unknown-linux-gnu"
        );
        // Podman matches the host arch, same as Docker.
        assert_eq!(
            target_triple(Runtime::Podman, "aarch64"),
            "aarch64-unknown-linux-gnu"
        );
        assert_eq!(
            target_triple(Runtime::Podman, "x86_64"),
            "x86_64-unknown-linux-gnu"
        );
    }

    #[test]
    fn backend_names_map_to_build_args_and_reject_unknown() {
        let resolved = resolve_backends(&["camoufox".into(), "kasmvnc".into()]).unwrap();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].build_arg, "WITH_CAMOUFOX");
        assert_eq!(resolved[1].build_arg, "WITH_KASMVNC");

        // Duplicates collapse.
        let deduped = resolve_backends(&["camoufox".into(), "camoufox".into()]).unwrap();
        assert_eq!(deduped.len(), 1);

        assert_eq!(
            resolve_backends(&["nope".into()]).unwrap_err().error_code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn embedded_build_args_include_version_target_and_apple_platform() {
        let ctx = PathBuf::from("/cache/ctx");
        let backends = resolve_backends(&["lightpanda".into()]).unwrap();
        let docker = build_args(
            "afhttp-host:1.2.3",
            Runtime::Docker,
            BuildSource::Embedded {
                ctx: &ctx,
                target: "x86_64-unknown-linux-gnu",
            },
            &backends,
        );
        assert_eq!(docker[0], "build");
        assert!(!docker.contains(&"--platform".to_string()));
        assert!(docker.contains(&"AFHTTP_BIN_FROM=downloader".to_string()));
        assert!(docker.contains(&format!("AFHTTP_VERSION={VERSION}")));
        assert!(docker.contains(&"AFHTTP_TARGET=x86_64-unknown-linux-gnu".to_string()));
        assert!(docker.contains(&"WITH_LIGHTPANDA=1".to_string()));
        assert_eq!(
            docker[docker.len() - 2],
            "/cache/ctx/container/docker/Dockerfile"
        );
        assert_eq!(docker.last().unwrap(), "/cache/ctx");

        let apple = build_args(
            "afhttp-host:1.2.3",
            Runtime::Apple,
            BuildSource::Embedded {
                ctx: &ctx,
                target: "aarch64-unknown-linux-gnu",
            },
            &[],
        );
        let pos = apple.iter().position(|a| a == "--platform").unwrap();
        assert_eq!(apple[pos + 1], "linux/arm64");
    }

    #[test]
    fn from_source_build_args_use_canonical_dockerfile_no_release_args() {
        let repo = PathBuf::from("/repo");
        let backends = resolve_backends(&["camoufox".into()]).unwrap();
        let args = build_args(
            "afhttp-host:1.2.3",
            Runtime::Podman,
            BuildSource::FromSource { ctx: &repo },
            &backends,
        );
        // Selects the builder stage; no download build-args.
        assert!(args.contains(&"AFHTTP_BIN_FROM=builder".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("AFHTTP_VERSION=")));
        assert!(!args.iter().any(|a| a.starts_with("AFHTTP_TARGET=")));
        assert!(args.contains(&"WITH_CAMOUFOX=1".to_string()));
        assert_eq!(args[args.len() - 2], "/repo/container/docker/Dockerfile");
        assert_eq!(args.last().unwrap(), "/repo");
        // Podman gets no --platform (host arch), same as Docker.
        assert!(!args.contains(&"--platform".to_string()));
    }

    #[test]
    fn run_args_publish_loopback_and_pass_host_args() {
        let a = run_args(
            "afhttp-host",
            "afhttp-host:1.2.3",
            9222,
            "work",
            "1g",
            &["--browser".into(), "camoufox".into()],
        );
        assert!(a.contains(&"afhttp-host-data:/data".to_string()));
        assert!(a.contains(&"AFHTTP_PORT=9222".to_string()));
        assert!(a.contains(&"AFHTTP_PROFILE=work".to_string()));
        assert!(a.contains(&"127.0.0.1:9222:9222".to_string()));
        // Image precedes the passthrough host args.
        let img = a.iter().position(|x| x == "afhttp-host:1.2.3").unwrap();
        let br = a.iter().position(|x| x == "--browser").unwrap();
        assert!(img < br);
    }

    #[test]
    fn client_command_uses_loopback_endpoint() {
        let cmd = client_command(9333, "deadbeef");
        assert!(cmd.contains("--endpoint-url ws://127.0.0.1:9333"));
        assert!(cmd.contains("--token-secret deadbeef"));
    }

    #[test]
    fn build_failure_error_points_at_compose_fallback() {
        let err = build_failed_error("aarch64-unknown-linux-gnu");
        assert_eq!(err.error_code, ErrorCode::InternalError);
        assert!(err.detail.contains("compose"));
        assert!(err.detail.contains("aarch64-unknown-linux-gnu"));
    }
}
