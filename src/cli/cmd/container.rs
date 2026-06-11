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

use crate::cli::cmd::argenums::TakeoverProviderArg;
use crate::cli::output;
use crate::sdk::capabilities::BackendFamily;
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
/// Source checkout used to compile this binary. Useful when `--from-source` is
/// requested from a different working directory, such as an agent scratch dir.
const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");
/// Default container name and image repository.
const DEFAULT_NAME: &str = "afhttp-host";
const DEFAULT_PORT: u16 = 9222;
const IMAGE_REPO: &str = "afhttp-host";
const HARD_SITE_BROWSER_ARG: &str = "--disable-blink-features=AutomationControlled";

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
    /// Capture or explicitly stream the container logs.
    Logs(LogsArgs),
}

/// Flags shared by every subcommand.
#[derive(ClapArgs, Debug)]
pub struct CommonArgs {
    /// Container runtime: docker, podman, or apple (auto-detected if omitted).
    #[arg(long, value_enum)]
    pub runtime: Option<Runtime>,
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
    /// Initial logical profile name inside the container. Defaults to `-`
    /// (ephemeral); persistent profiles are scoped by backend.
    #[arg(long)]
    pub profile: Option<String>,
    /// Chromium /dev/shm size. Defaults to `1g`, or `2g` when takeover is on.
    #[arg(long = "shm-size")]
    pub shm_size: Option<String>,
    /// Real-display takeover provider for the built host. A provider name
    /// (default `kasmvnc`) builds a Brave + KasmVNC takeover-ready host with an
    /// ephemeral initial profile and 2g /dev/shm; `off` builds a lean headless host.
    #[arg(long = "takeover-provider", default_value = "kasmvnc")]
    pub takeover_provider: TakeoverProviderArg,
    /// Extra component to build into the image (repeatable). Browser backends:
    /// chrome-headless-shell, lightpanda, fingerprint-chromium, camoufox, brave.
    /// Plus the takeover provider: kasmvnc.
    #[arg(long = "with", value_name = "COMPONENT")]
    pub with: Vec<String>,
    /// Rebuild the image even if it already exists.
    #[arg(long)]
    pub rebuild: bool,
    /// Build the full image from a source checkout (container/docker/Dockerfile)
    /// instead of downloading the prebuilt release. Needs the source tree.
    #[arg(long = "from-source")]
    pub from_source: bool,
    /// Source checkout to build from with --from-source (default: current dir,
    /// then the checkout this afhttp binary was built from).
    #[arg(long, value_name = "DIR")]
    pub context: Option<String>,
    /// Extra args passed through to `afhttp host` inside the container.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub host_args: Vec<String>,
    /// Explicitly include the long-lived host token in stdout.
    #[arg(long = "reveal-token-secret")]
    pub reveal_token_secret: bool,
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
    /// Explicitly include the long-lived host token in stdout.
    #[arg(long = "reveal-token-secret")]
    pub reveal_token_secret: bool,
}

#[derive(ClapArgs, Debug)]
pub struct LogsArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// Follow the log output.
    #[arg(long)]
    pub follow: bool,
    /// Stream raw runtime logs instead of returning a JSON summary.
    #[arg(long)]
    pub raw: bool,
}

/// Container runtime selector. Parsed from `--runtime` (clap `ValueEnum`) and
/// from `AFHTTP_CONTAINER_RUNTIME` via [`runtime_from_str`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Runtime {
    Docker,
    Podman,
    /// Apple's `container` CLI. Accepts `apple` or `container` on the
    /// command line; its binary is `container` (see [`Runtime::bin`]).
    #[value(alias = "container")]
    Apple,
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

#[derive(Clone, Debug, Serialize)]
pub(crate) struct InstallResult {
    pub(crate) runtime: &'static str,
    pub(crate) image: String,
    pub(crate) container: String,
    pub(crate) endpoint: String,
    pub(crate) profile: String,
    pub(crate) token_available: bool,
    pub(crate) token_source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) token_secret: Option<String>,
    pub(crate) client_command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) log_file: Option<PathBuf>,
    pub(crate) backends: Vec<String>,
    pub(crate) takeover_ready: bool,
}

async fn install(mut args: InstallArgs) -> Result<(), Error> {
    let result = install_result(&mut args).await?;
    if args.reveal_token_secret {
        output::emit_unredacted("container_install", &result)
    } else {
        output::emit("container_install", &result)
    }
}

async fn install_result(args: &mut InstallArgs) -> Result<InstallResult, Error> {
    apply_hard_site_defaults(args);
    let backends = resolve_backends(&args.with)?;
    validate_install_args(args, &backends)?;
    let runtime = resolve_runtime(args.common.runtime)?;
    let image = image_tag();
    let profile = effective_profile(args);
    let shm_size = effective_shm_size(args);
    let log_file = container_operation_log_file(&args.common.name)?;

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
        exec_to_log(runtime.bin(), &build, &log_file)?;
    } else if args.rebuild || !image_exists(runtime, &image) {
        let ctx = write_build_context()?;
        let target = target_triple(runtime, std::env::consts::ARCH);
        let build = build_args(
            &image,
            runtime,
            BuildSource::Embedded { ctx: &ctx, target },
            &backends,
        );
        exec_to_log(runtime.bin(), &build, &log_file)
            .map_err(|_| build_failed_error(target, &log_file))?;
    }
    validate_container_image_host_args(runtime, &image, &args.host_args)?;

    // Recreate cleanly. The profile + token live in the named volume, so the
    // token is stable across recreation.
    let _ = capture(runtime.bin(), &["stop".into(), args.common.name.clone()]);
    let _ = capture(runtime.bin(), &["rm".into(), args.common.name.clone()]);

    let run = run_args(
        &args.common.name,
        &image,
        args.port,
        &profile,
        &shm_size,
        &args.host_args,
    );
    exec_to_log(runtime.bin(), &run, &log_file)?;

    let token = read_token(runtime, &args.common.name).await?;
    let endpoint = endpoint_url(args.port);
    wait_for_container_health(runtime, &args.common.name, args.port, &token).await?;
    let takeover_ready = install_takeover_provider(args).is_some();
    if takeover_ready {
        validate_running_hard_site(&endpoint, &token).await?;
    }
    Ok(InstallResult {
        runtime: runtime.label(),
        image,
        container: args.common.name.clone(),
        endpoint,
        profile,
        client_command: client_command(args.port),
        token_available: true,
        token_source: "container_volume",
        token_secret: args.reveal_token_secret.then_some(token),
        log_file: Some(log_file),
        backends: backends.iter().map(|b| b.name.to_string()).collect(),
        takeover_ready,
    })
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

#[derive(Debug, Serialize)]
struct StatusResult {
    runtime: &'static str,
    container: String,
    running: bool,
    endpoint: String,
    driver_version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    host_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version_match: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile_backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backend: Option<BackendFamily>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    takeover_ready: Option<bool>,
    token_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    token_source: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token_secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    log_summary: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

async fn status(args: StatusArgs) -> Result<(), Error> {
    let runtime = resolve_runtime(args.common.runtime)?;
    let state = inspect_container_state(runtime, &args.common.name);
    let running = state
        .as_ref()
        .map(|s| s.running)
        .unwrap_or_else(|| container_running(runtime, &args.common.name));
    let endpoint = endpoint_url(args.port);
    let mut warnings = Vec::new();

    let token = if running {
        match read_token(runtime, &args.common.name).await {
            Ok(token) => Some(token),
            Err(e) => {
                warnings.push(format!("could not read token: {}", e.detail));
                None
            }
        }
    } else {
        None
    };
    let client_command = token.as_ref().map(|_| client_command(args.port));
    let mut host_version = None;
    let mut version_match = None;
    let mut profile_kind = None;
    let mut profile = None;
    let mut profile_backend = None;
    let mut backend = None;
    let mut provider = None;
    let mut takeover_ready = None;
    if running {
        if let Some(token) = token.as_deref() {
            let client = crate::sdk::Client::connect(&endpoint)?.with_token(token.to_string());
            match client.health().await {
                Ok(health) => {
                    let version_warning = host_version_warning(&args.common.name, &health.version);
                    if let Some(warning) = version_warning {
                        warnings.push(warning);
                    }
                    version_match = Some(health.version == VERSION);
                    host_version = Some(health.version);
                    if let Some(snapshot) = health.profile {
                        profile_kind = Some(snapshot.kind);
                        profile = snapshot.name;
                    }
                }
                Err(e) => warnings.push(format!("could not read /health: {}", e.detail)),
            }
            match client.capabilities().await {
                Ok(caps) => {
                    profile_backend = profile_kind.as_ref().map(|_| caps.backend.family.clone());
                    provider = caps.takeover.provider.clone();
                    takeover_ready = Some(is_hard_site_capabilities(&caps));
                    backend = Some(caps.backend);
                }
                Err(e) => warnings.push(format!("could not read /capabilities: {}", e.detail)),
            }
        }
    }
    let (exit_code, log_summary) = if running {
        (None, None)
    } else {
        let exit_code = state.as_ref().and_then(|s| s.exit_code);
        let logs = container_logs_summary(runtime, &args.common.name);
        let logs = (!logs.is_empty()).then_some(logs);
        (exit_code, logs)
    };

    let result = StatusResult {
        runtime: runtime.label(),
        container: args.common.name,
        running,
        endpoint,
        driver_version: VERSION,
        host_version,
        version_match,
        profile_kind,
        profile,
        profile_backend,
        backend,
        provider,
        takeover_ready,
        token_available: token.is_some(),
        token_source: token.as_ref().map(|_| "container_volume"),
        token_secret: args.reveal_token_secret.then_some(token).flatten(),
        client_command,
        exit_code,
        log_summary,
        warnings,
    };
    if args.reveal_token_secret {
        output::emit_unredacted("container_status", &result)
    } else {
        output::emit("container_status", &result)
    }
}

/// Validate that a host's `/capabilities` describe a takeover-ready backend:
/// Brave plus a KasmVNC real-display takeover surface. Relocated from the
/// former `takeover` command so the install path can verify the host it built.
pub(crate) fn validate_hard_site_capabilities(
    caps: &crate::sdk::capabilities::CapabilitiesResponse,
) -> Result<(), Error> {
    if caps.backend.family != "brave" {
        return Err(hard_site_host_error(format!(
            "takeover host requires backend.family=brave; host reported {}",
            caps.backend.family
        )));
    }
    if !caps.takeover.supported {
        return Err(hard_site_host_error(
            "takeover host requires takeover.supported=true".to_string(),
        ));
    }
    if caps.takeover.provider.as_deref() != Some("kasmvnc") {
        return Err(hard_site_host_error(format!(
            "takeover host requires takeover.provider=kasmvnc; host reported {:?}",
            caps.takeover.provider
        )));
    }
    Ok(())
}

fn hard_site_host_error(detail: String) -> Error {
    Error::new(
        ErrorCode::BackendUnsupported,
        format!("{detail}. Build a takeover-ready host with `afhttp container install`."),
    )
}

// ── logs ───────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct LogsResult {
    runtime: &'static str,
    container: String,
    log_file: PathBuf,
    bytes: u64,
    truncated: bool,
    tail_lines: Vec<String>,
}

fn logs(args: LogsArgs) -> Result<(), Error> {
    let runtime = resolve_runtime(args.common.runtime)?;
    if args.follow && !args.raw {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "container logs --follow requires --raw because follow mode is an open-ended stream",
        ));
    }
    let container = args.common.name;
    let mut argv: Vec<String> = vec!["logs".into()];
    if args.follow {
        argv.push("-f".into());
    }
    argv.push(container.clone());
    if args.raw {
        return exec_inherit(runtime.bin(), &argv);
    }
    let log_file = container_operation_log_file(&container)?;
    exec_to_log_without_header(runtime.bin(), &argv, &log_file)?;
    const TAIL: usize = 80;
    let (tail_lines, truncated) = tail_lines_from_file(&log_file, TAIL)?;
    let bytes = std::fs::metadata(&log_file).map(|m| m.len()).map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("stat container log file {}: {e}", log_file.display()),
        )
    })?;
    output::emit(
        "container_logs",
        &LogsResult {
            runtime: runtime.label(),
            container,
            log_file,
            bytes,
            truncated,
            tail_lines,
        },
    )
}

// ── runtime resolution ───────────────────────────────────────────────────────

fn resolve_runtime(explicit: Option<Runtime>) -> Result<Runtime, Error> {
    if let Some(r) = explicit {
        return Ok(r);
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

fn client_command(port: u16) -> String {
    format!(
        "AFHTTP_TOKEN_SECRET=<host-token> afhttp fetch https://example.com --endpoint-url ws://127.0.0.1:{port}"
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

const BACKENDS: [Backend; 6] = [
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
        name: "brave",
        build_arg: "WITH_BRAVE",
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

fn validate_install_args(args: &InstallArgs, backends: &[Backend]) -> Result<(), Error> {
    let profile = effective_profile(args);
    if let Some(provider) = install_takeover_provider(args) {
        validate_hard_site_install_args(args, backends, provider)?;
    }
    let camoufox_built = backends.iter().any(|b| b.name == "camoufox");
    if profile != "-" && camoufox_built && host_args_select_camoufox(&args.host_args) {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "afhttp's camoufox backend does not yet support persistent profiles; use `--profile -` for camoufox hosts. Example: `afhttp container install --profile - --with camoufox -- --browser camoufox`.",
        ));
    }
    Ok(())
}

/// The takeover provider requested for the built host, or `None` for `off`.
/// `container install` defaults to `kasmvnc` (takeover on);
/// `--takeover-provider off` builds a lean headless host.
fn install_takeover_provider(args: &InstallArgs) -> Option<&'static str> {
    args.takeover_provider.provider_name()
}

fn apply_hard_site_defaults(args: &mut InstallArgs) {
    let Some(provider) = install_takeover_provider(args).map(str::to_string) else {
        return;
    };
    push_backend_if_missing(&mut args.with, "brave");
    push_backend_if_missing(&mut args.with, "kasmvnc");
    push_host_arg_default(&mut args.host_args, "--browser", "brave");
    push_host_arg_default(&mut args.host_args, "--takeover-provider", &provider);
    push_host_arg_value_if_missing(&mut args.host_args, "--browser-arg", HARD_SITE_BROWSER_ARG);
}

fn effective_profile(args: &InstallArgs) -> String {
    args.profile.clone().unwrap_or_else(|| "-".to_string())
}

fn effective_shm_size(args: &InstallArgs) -> String {
    args.shm_size.clone().unwrap_or_else(|| {
        if install_takeover_provider(args).is_some() {
            "2g"
        } else {
            "1g"
        }
        .to_string()
    })
}

fn push_backend_if_missing(backends: &mut Vec<String>, backend: &str) {
    if !backends.iter().any(|b| b == backend) {
        backends.push(backend.to_string());
    }
}

fn push_host_arg_default(host_args: &mut Vec<String>, name: &str, value: &str) {
    if !host_arg_present(host_args, name) {
        host_args.push(name.to_string());
        host_args.push(value.to_string());
    }
}

fn push_host_arg_value_if_missing(host_args: &mut Vec<String>, name: &str, value: &str) {
    if !host_arg_has_value(host_args, name, value) {
        host_args.push(format!("{name}={value}"));
    }
}

fn validate_hard_site_install_args(
    args: &InstallArgs,
    backends: &[Backend],
    provider: &str,
) -> Result<(), Error> {
    if !backends.iter().any(|b| b.name == "brave") {
        return Err(hard_site_install_error(
            "takeover requires the brave backend; omit conflicting backend overrides".to_string(),
        ));
    }
    if !backends.iter().any(|b| b.name == "kasmvnc") {
        return Err(hard_site_install_error(
            "takeover requires the KasmVNC display backend".to_string(),
        ));
    }
    require_hard_site_host_arg(&args.host_args, "--browser", "brave")?;
    require_hard_site_host_arg(&args.host_args, "--takeover-provider", provider)?;
    require_hard_site_host_arg_value(&args.host_args, "--browser-arg", HARD_SITE_BROWSER_ARG)?;
    Ok(())
}

fn require_hard_site_host_arg(
    host_args: &[String],
    name: &str,
    expected: &str,
) -> Result<(), Error> {
    let Some(value) = host_arg_value(host_args, name) else {
        return Err(hard_site_install_error(format!(
            "takeover requires host arg `{name} {expected}`"
        )));
    };
    if value == expected {
        return Ok(());
    }
    Err(hard_site_install_error(format!(
        "takeover requires host arg `{name} {expected}`; got `{name} {value}`"
    )))
}

fn hard_site_install_error(detail: String) -> Error {
    Error::new(
        ErrorCode::InvalidArgument,
        format!("{detail}. Use `afhttp container install` (takeover is on by default), or `--takeover-provider off` for a lean host."),
    )
}

fn require_hard_site_host_arg_value(
    host_args: &[String],
    name: &str,
    expected: &str,
) -> Result<(), Error> {
    if host_arg_has_value(host_args, name, expected) {
        return Ok(());
    }
    Err(hard_site_install_error(format!(
        "takeover requires host arg `{name} {expected}`"
    )))
}

fn host_args_select_camoufox(host_args: &[String]) -> bool {
    host_arg_value(host_args, "--browser").as_deref() == Some("camoufox")
}

fn host_arg_present(host_args: &[String], name: &str) -> bool {
    let eq_prefix = format!("{name}=");
    host_args
        .iter()
        .any(|arg| arg == name || arg.starts_with(&eq_prefix))
}

fn host_arg_value(host_args: &[String], name: &str) -> Option<String> {
    let eq_prefix = format!("{name}=");
    let mut value = None;
    let mut iter = host_args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == name {
            if let Some(next) = iter.peek() {
                value = Some((*next).to_string());
            }
        } else if let Some(v) = arg.strip_prefix(&eq_prefix) {
            value = Some(v.to_string());
        }
    }
    value
}

fn host_arg_has_value(host_args: &[String], name: &str, expected: &str) -> bool {
    let eq_prefix = format!("{name}=");
    let mut iter = host_args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == name {
            if let Some(next) = iter.peek() {
                if host_arg_values_equal(next, expected) {
                    return true;
                }
            }
        } else if let Some(value) = arg.strip_prefix(&eq_prefix) {
            if host_arg_values_equal(value, expected) {
                return true;
            }
        }
    }
    false
}

fn host_arg_values_equal(value: &str, expected: &str) -> bool {
    value == expected || value.trim_start_matches("--") == expected.trim_start_matches("--")
}

fn validate_container_image_host_args(
    runtime: Runtime,
    image: &str,
    host_args: &[String],
) -> Result<(), Error> {
    if !host_args_need_takeover_support(host_args) {
        return Ok(());
    }
    let Some(help) = container_image_host_help(runtime, image) else {
        return Ok(());
    };
    if !help.contains("--takeover-quality-percent") {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            format!(
                "container image `{image}` contains an older afhttp host binary that does not support the `--takeover-provider <provider>` display surface; rebuild from this source checkout: `afhttp container install --from-source --rebuild`"
            ),
        ));
    }
    if host_arg_value(host_args, "--browser").as_deref() == Some("brave")
        && !container_image_hard_site_components(runtime, image)
    {
        return Err(Error::new(
            ErrorCode::BackendUnsupported,
            format!(
                "container image `{image}` does not expose the Brave + KasmVNC takeover components; rebuild with `afhttp container install --from-source --rebuild`"
            ),
        ));
    }
    Ok(())
}

async fn validate_running_hard_site(endpoint: &str, token: &str) -> Result<(), Error> {
    let client = crate::sdk::Client::connect(endpoint)?.with_token(token.to_string());
    let health = client.health().await.map_err(|e| {
        Error::new(
            e.error_code,
            format!("takeover host /health failed after startup: {}", e.detail),
        )
        .with_retryable(e.retryable)
    })?;
    if health.version != VERSION {
        return Err(Error::new(
            ErrorCode::InternalError,
            format!(
                "takeover host version mismatch after startup: host={}, driver={VERSION}",
                health.version
            ),
        ));
    }
    if health.status != "ok" {
        let detail = health
            .backend_error
            .map(|e| format!("{}: {}", e.error_code, e.error))
            .unwrap_or_else(|| format!("status={}", health.status));
        return Err(Error::new(
            ErrorCode::BrowserLaunchFailed,
            format!("takeover host was not ready after startup: {detail}"),
        ));
    }
    let caps = client.capabilities().await.map_err(|e| {
        Error::new(
            e.error_code,
            format!(
                "takeover host /capabilities failed after startup: {}",
                e.detail
            ),
        )
        .with_retryable(e.retryable)
    })?;
    validate_hard_site_capabilities(&caps)
}

fn is_hard_site_capabilities(caps: &crate::sdk::capabilities::CapabilitiesResponse) -> bool {
    caps.backend.family == "brave"
        && caps.takeover.supported
        && caps.takeover.provider.as_deref() == Some("kasmvnc")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalTakeoverHost {
    pub(crate) endpoint: String,
    pub(crate) token_secret: Option<String>,
}

/// Discover the standard local `afhttp-host` container for `fetch --takeover`
/// when the caller did not pass an endpoint. This is intentionally read-only:
/// it never starts or recreates containers.
pub(crate) async fn discover_default_takeover_host(
    token_override: Option<&str>,
) -> Result<LocalTakeoverHost, Error> {
    let runtime = resolve_runtime(None).map_err(|e| {
        local_takeover_error(format!(
            "could not choose a container runtime to inspect `{DEFAULT_NAME}`: {}",
            e.detail
        ))
    })?;
    let running = inspect_container_state(runtime, DEFAULT_NAME)
        .map(|s| s.running)
        .unwrap_or_else(|| container_running(runtime, DEFAULT_NAME));
    if !running {
        return Err(local_takeover_error(format!(
            "default local container `{DEFAULT_NAME}` is not running"
        )));
    }

    let token_secret = match token_override {
        Some(token) => Some(token.to_string()),
        None => Some(read_token(runtime, DEFAULT_NAME).await.map_err(|e| {
            local_takeover_error(format!(
                "default local container `{DEFAULT_NAME}` is running, but its token could not be read: {}",
                e.detail
            ))
        })?),
    };
    let endpoint = endpoint_url(DEFAULT_PORT);
    let client = match token_secret.as_deref() {
        Some(token) => crate::sdk::Client::connect(&endpoint)?.with_token(token.to_string()),
        None => crate::sdk::Client::connect(&endpoint)?,
    };
    let health = client.health().await.map_err(|e| {
        local_takeover_error(format!(
            "default local container `{DEFAULT_NAME}` at {endpoint} could not be verified: {}",
            e.detail
        ))
        .with_retryable(e.retryable)
    })?;
    if health.version != VERSION {
        return Err(local_takeover_error(host_version_mismatch_detail(
            DEFAULT_NAME,
            &health.version,
        )));
    }
    if health.status != "ok" {
        let detail = health
            .backend_error
            .map(|e| format!("{}: {}", e.error_code, e.error))
            .unwrap_or_else(|| format!("status={}", health.status));
        return Err(local_takeover_error(format!(
            "default local container `{DEFAULT_NAME}` at {endpoint} is not ready: {detail}"
        )));
    }
    let caps = client.capabilities().await.map_err(|e| {
        local_takeover_error(format!(
            "default local container `{DEFAULT_NAME}` at {endpoint} could not be verified: {}",
            e.detail
        ))
        .with_retryable(e.retryable)
    })?;
    validate_hard_site_capabilities(&caps).map_err(|e| {
        local_takeover_error(format!(
            "default local container `{DEFAULT_NAME}` at {endpoint} is not takeover-ready: {}",
            e.detail
        ))
    })?;
    Ok(LocalTakeoverHost {
        endpoint,
        token_secret,
    })
}

fn local_takeover_error(detail: String) -> Error {
    Error::new(
        ErrorCode::InvalidArgument,
        format!(
            "fetch --takeover did not receive --endpoint-url or AFHTTP_ENDPOINT_URL, and {detail}. \
             Start one with `afhttp container install`, inspect it with `afhttp container status`, \
             or pass --endpoint-url/--token-secret explicitly."
        ),
    )
}

fn host_version_warning(name: &str, host_version: &str) -> Option<String> {
    (host_version != VERSION).then(|| host_version_mismatch_detail(name, host_version))
}

fn host_version_mismatch_detail(name: &str, host_version: &str) -> String {
    format!(
        "local container `{name}` is running afhttp host version {host_version}, \
         but this driver is version {VERSION}. Run `afhttp container install` to recreate the \
         container with the matching image; the `{}` volume is reused, so the host token and \
         persistent profiles are preserved.",
        volume_name(name)
    )
}

fn host_args_need_takeover_support(host_args: &[String]) -> bool {
    // A `--takeover-provider <provider>` host arg (anything other than
    // off/none) needs a display-capable host binary in the image.
    match host_arg_value(host_args, "--takeover-provider") {
        Some(value) => value != "off" && value != "none",
        None => false,
    }
}

fn container_image_host_help(runtime: Runtime, image: &str) -> Option<String> {
    let argv = image_host_help_args(image);
    let out = capture(runtime.bin(), &argv).ok()?;
    if !out.status.success() {
        return None;
    }
    let mut help = String::new();
    help.push_str(&String::from_utf8_lossy(&out.stdout));
    help.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(help)
}

fn image_host_help_args(image: &str) -> Vec<String> {
    vec![
        "run".into(),
        "--rm".into(),
        "--entrypoint".into(),
        "/usr/local/bin/afhttp".into(),
        image.to_string(),
        "host".into(),
        "--help".into(),
    ]
}

fn container_image_hard_site_components(runtime: Runtime, image: &str) -> bool {
    let argv = vec![
        "run".into(),
        "--rm".into(),
        "--entrypoint".into(),
        "/bin/sh".into(),
        image.to_string(),
        "-lc".into(),
        "command -v brave-browser >/dev/null 2>&1 && test -x \"${AFHTTP_KASMVNC_BIN:-/usr/bin/Xvnc}\" && test -d \"${AFHTTP_KASMVNC_WEB_ROOT:-/usr/share/kasmvnc/www}\"".into(),
    ];
    capture(runtime.bin(), &argv)
        .map(|out| out.status.success())
        .unwrap_or(false)
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
    if let Some(p) = arg {
        return validate_source_context(PathBuf::from(p), "--context");
    }
    let cwd = std::env::current_dir()
        .map_err(|e| Error::new(ErrorCode::IoError, format!("cannot read current dir: {e}")))?;
    if is_source_context(&cwd) {
        return Ok(cwd);
    }
    let manifest_dir = PathBuf::from(MANIFEST_DIR);
    if manifest_dir != cwd && is_source_context(&manifest_dir) {
        return Ok(manifest_dir);
    }
    Err(Error::new(
        ErrorCode::InvalidArgument,
        format!(
            "--from-source needs a source checkout: checked {} and {} \
             (run from the spore root or pass --context <dir>)",
            cwd.display(),
            manifest_dir.display()
        ),
    ))
}

fn validate_source_context(dir: PathBuf, source: &str) -> Result<PathBuf, Error> {
    let dockerfile = dir.join("container/docker/Dockerfile");
    if !dockerfile.is_file() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            format!(
                "--from-source {source} needs a source checkout: {} not found \
                 (run from the spore root or pass --context <dir>)",
                dockerfile.display()
            ),
        ));
    }
    Ok(dir)
}

fn is_source_context(dir: &Path) -> bool {
    dir.join("container/docker/Dockerfile").is_file()
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

/// Run a runtime command with stdout/stderr appended to a log file, keeping
/// CLI stdout reserved for the final AFDATA envelope.
fn exec_to_log(bin: &str, args: &[String], log_file: &Path) -> Result<(), Error> {
    exec_to_log_impl(bin, args, log_file, true)
}

fn exec_to_log_without_header(bin: &str, args: &[String], log_file: &Path) -> Result<(), Error> {
    exec_to_log_impl(bin, args, log_file, false)
}

fn exec_to_log_impl(
    bin: &str,
    args: &[String],
    log_file: &Path,
    write_header: bool,
) -> Result<(), Error> {
    use std::io::Write;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
        .map_err(|e| {
            Error::new(
                ErrorCode::IoError,
                format!("open log file {}: {e}", log_file.display()),
            )
        })?;
    if write_header {
        writeln!(file, "\n$ {bin} {}", args.join(" ")).map_err(|e| {
            Error::new(
                ErrorCode::IoError,
                format!("write log file {}: {e}", log_file.display()),
            )
        })?;
    }
    let stdout = file.try_clone().map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("clone log file {}: {e}", log_file.display()),
        )
    })?;
    let stderr = file.try_clone().map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("clone log file {}: {e}", log_file.display()),
        )
    })?;
    let status = Command::new(bin)
        .args(args)
        .stdout(stdout)
        .stderr(stderr)
        .status()
        .map_err(|e| spawn_error(bin, &e))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::new(
            ErrorCode::InternalError,
            format!(
                "`{bin} {}` failed ({status}); full output was written to {}",
                args.join(" "),
                log_file.display()
            ),
        ))
    }
}

fn tail_lines_from_file(path: &Path, max_lines: usize) -> Result<(Vec<String>, bool), Error> {
    use std::io::{Read, Seek, SeekFrom};

    const MAX_TAIL_BYTES: u64 = 256 * 1024;
    let mut file = std::fs::File::open(path).map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("open container log file {}: {e}", path.display()),
        )
    })?;
    let len = file
        .metadata()
        .map_err(|e| {
            Error::new(
                ErrorCode::IoError,
                format!("stat container log file {}: {e}", path.display()),
            )
        })?
        .len();
    let start = len.saturating_sub(MAX_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("seek container log file {}: {e}", path.display()),
        )
    })?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("read container log file {}: {e}", path.display()),
        )
    })?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().collect();
    let truncated_by_bytes = start > 0;
    if truncated_by_bytes && !text.starts_with('\n') && !lines.is_empty() {
        lines.remove(0);
    }
    let truncated = truncated_by_bytes || lines.len() > max_lines;
    let tail_lines = lines
        .iter()
        .skip(lines.len().saturating_sub(max_lines))
        .map(|line| (*line).to_string())
        .collect();
    Ok((tail_lines, truncated))
}

/// Run a runtime command capturing stdout/stderr.
fn capture(bin: &str, args: &[String]) -> Result<std::process::Output, Error> {
    Command::new(bin)
        .args(args)
        .output()
        .map_err(|e| spawn_error(bin, &e))
}

fn container_operation_log_file(name: &str) -> Result<PathBuf, Error> {
    let dir = std::env::temp_dir().join("afhttp-container-logs");
    std::fs::create_dir_all(&dir).map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("create container log dir {}: {e}", dir.display()),
        )
    })?;
    let safe_name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(dir.join(format!("{safe_name}-{}.log", uuid::Uuid::new_v4())))
}

fn image_exists(runtime: Runtime, image: &str) -> bool {
    capture(
        runtime.bin(),
        &["image".into(), "inspect".into(), image.to_string()],
    )
    .map(|o| o.status.success())
    .unwrap_or(false)
}

#[derive(Debug, Clone)]
struct ContainerState {
    running: bool,
    exit_code: Option<i64>,
}

fn inspect_container_state(runtime: Runtime, name: &str) -> Option<ContainerState> {
    let out = capture(runtime.bin(), &["inspect".into(), name.to_string()]).ok()?;
    if !out.status.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let state = value
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|v| v.get("State"))
        .or_else(|| value.get("State"))?;
    Some(ContainerState {
        running: state
            .get("Running")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        exit_code: state.get("ExitCode").and_then(|v| v.as_i64()),
    })
}

fn container_running(runtime: Runtime, name: &str) -> bool {
    if let Some(state) = inspect_container_state(runtime, name) {
        return state.running;
    }
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
    for attempt in 0..20 {
        if let Ok(out) = capture(runtime.bin(), &argv) {
            if out.status.success() {
                let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !token.is_empty() {
                    return Ok(token);
                }
            }
        }
        if !container_running(runtime, name) {
            return Err(container_launch_failure_error(
                runtime,
                name,
                "container exited before the host token could be read",
            ));
        }
        if attempt < 19 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
    Err(container_launch_failure_error(
        runtime,
        name,
        "host token was not available before the startup deadline",
    ))
}

async fn wait_for_container_health(
    runtime: Runtime,
    name: &str,
    port: u16,
    token: &str,
) -> Result<(), Error> {
    let endpoint = endpoint_url(port);
    let client = crate::sdk::Client::connect(&endpoint)?.with_token(token.to_string());
    for attempt in 0..30 {
        if !container_running(runtime, name) {
            return Err(container_launch_failure_error(
                runtime,
                name,
                "container exited before /health became ready",
            ));
        }
        match client.health().await {
            Ok(health) if health.version != VERSION => {
                return Err(Error::new(
                    ErrorCode::InternalError,
                    format!(
                        "container host version mismatch after startup: host={}, driver={VERSION}",
                        health.version
                    ),
                ));
            }
            Ok(health) if health.status == "ok" => return Ok(()),
            Ok(health) => {
                if let Some(backend_error) = health.backend_error {
                    return Err(Error::new(
                        backend_error.error_code,
                        format!(
                            "container host /health reported {}: {}",
                            health.status, backend_error.error
                        ),
                    ));
                }
            }
            Err(_) => {}
        }
        if attempt < 29 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
    Err(container_launch_failure_error(
        runtime,
        name,
        "container host did not pass /health before the startup deadline",
    ))
}

fn container_launch_failure_error(runtime: Runtime, name: &str, reason: &str) -> Error {
    let logs = container_logs_summary(runtime, name);
    let lower = logs.to_ascii_lowercase();
    let code = if lower.contains("backend_unsupported")
        || lower.contains("persistent profiles")
        || lower.contains("does not yet support")
    {
        ErrorCode::BackendUnsupported
    } else {
        ErrorCode::BrowserLaunchFailed
    };
    let mut detail = format!("container host launch failed: {reason}");
    if !logs.is_empty() {
        detail.push_str("; recent logs: ");
        detail.push_str(&logs);
    }
    Error::new(code, detail)
}

fn container_logs_summary(runtime: Runtime, name: &str) -> String {
    let Ok(out) = capture(runtime.bin(), &["logs".into(), name.to_string()]) else {
        return String::new();
    };
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&out.stdout));
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    let lines: Vec<&str> = combined.lines().rev().take(60).collect();
    let mut summary = lines.into_iter().rev().collect::<Vec<_>>().join(" | ");
    const MAX: usize = 4000;
    if summary.len() > MAX {
        let start = summary.len() - MAX;
        summary = format!("...{}", &summary[start..]);
    }
    summary
}

fn build_failed_error(target: &str, log_file: &Path) -> Error {
    Error::new(
        ErrorCode::InternalError,
        format!(
            "image build failed. If v{VERSION} has no published release asset for \
             {target}, build from a source checkout instead: \
             `afhttp container install --from-source` (or \
             docker compose -f container/docker/compose.yaml up --build). Full output: {}",
            log_file.display()
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
            resolve_runtime(Some(Runtime::Apple)).unwrap(),
            Runtime::Apple
        );
        assert_eq!(
            resolve_runtime(Some(Runtime::Docker)).unwrap(),
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
        let resolved =
            resolve_backends(&["camoufox".into(), "brave".into(), "kasmvnc".into()]).unwrap();
        assert_eq!(resolved.len(), 3);
        assert_eq!(resolved[0].build_arg, "WITH_CAMOUFOX");
        assert_eq!(resolved[1].build_arg, "WITH_BRAVE");
        assert_eq!(resolved[2].build_arg, "WITH_KASMVNC");

        // Duplicates collapse.
        let deduped = resolve_backends(&["camoufox".into(), "camoufox".into()]).unwrap();
        assert_eq!(deduped.len(), 1);

        assert_eq!(
            resolve_backends(&["nope".into()]).unwrap_err().error_code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn install_precheck_rejects_camoufox_with_persistent_profile() {
        let args = InstallArgs {
            common: CommonArgs {
                runtime: Some(Runtime::Docker),
                name: "afhttp-host".into(),
            },
            port: 9222,
            profile: Some("work".into()),
            shm_size: Some("1g".into()),
            takeover_provider: TakeoverProviderArg::Off,
            with: vec!["camoufox".into()],
            rebuild: false,
            from_source: false,
            context: None,
            host_args: vec!["--browser".into(), "camoufox".into()],
            reveal_token_secret: false,
        };
        let backends = resolve_backends(&args.with).unwrap();
        let err = validate_install_args(&args, &backends).unwrap_err();
        assert_eq!(err.error_code, ErrorCode::InvalidArgument);
        assert!(err.detail.contains("--profile -"));
    }

    #[test]
    fn install_precheck_allows_camoufox_ephemeral_profile() {
        let args = InstallArgs {
            common: CommonArgs {
                runtime: Some(Runtime::Docker),
                name: "afhttp-host".into(),
            },
            port: 9222,
            profile: Some("-".into()),
            shm_size: Some("1g".into()),
            takeover_provider: TakeoverProviderArg::Off,
            with: vec!["camoufox".into()],
            rebuild: false,
            from_source: false,
            context: None,
            host_args: vec!["--browser=camoufox".into()],
            reveal_token_secret: false,
        };
        let backends = resolve_backends(&args.with).unwrap();
        validate_install_args(&args, &backends).unwrap();
    }

    #[test]
    fn install_result_exposes_hard_site_flag() {
        let value = serde_json::to_value(InstallResult {
            runtime: "docker",
            image: "afhttp-host:test".into(),
            container: "afhttp-host".into(),
            endpoint: "ws://127.0.0.1:9222".into(),
            profile: "work".into(),
            token_available: true,
            token_source: "container_volume",
            token_secret: None,
            client_command: "afhttp fetch https://example.com".into(),
            log_file: Some(PathBuf::from("/tmp/afhttp-container-logs/install.log")),
            backends: vec!["brave".into(), "kasmvnc".into()],
            takeover_ready: true,
        })
        .unwrap();
        assert_eq!(value["takeover_ready"], true);
        assert_eq!(value["token_available"], true);
        assert_eq!(value["token_source"], "container_volume");
        assert!(value.get("token_secret").is_none());
        assert!(value.get("token").is_none());
    }

    #[test]
    fn status_result_hides_token_secret_by_default() {
        let value = serde_json::to_value(StatusResult {
            runtime: "docker",
            container: "afhttp-host".into(),
            running: true,
            endpoint: "ws://127.0.0.1:9222".into(),
            driver_version: VERSION,
            host_version: Some(VERSION.into()),
            version_match: Some(true),
            profile_kind: Some("persistent".into()),
            profile: Some("work".into()),
            profile_backend: Some("brave".into()),
            backend: Some(BackendFamily {
                family: "brave".into(),
                version: "1".into(),
            }),
            provider: Some("kasmvnc".into()),
            takeover_ready: Some(true),
            token_available: true,
            token_source: Some("container_volume"),
            token_secret: None,
            client_command: Some("afhttp fetch https://example.com".into()),
            exit_code: None,
            log_summary: None,
            warnings: Vec::new(),
        })
        .unwrap();
        assert!(value.get("token_secret").is_none());
        assert_eq!(value["token_available"], true);
        assert_eq!(value["token_source"], "container_volume");
        assert_eq!(value["profile_kind"], "persistent");
        assert_eq!(value["profile_backend"], "brave");
        assert_eq!(value["backend"]["family"], "brave");
        assert_eq!(value["takeover_ready"], true);
        assert_eq!(value["driver_version"], VERSION);
        assert_eq!(value["host_version"], VERSION);
        assert_eq!(value["version_match"], true);
        assert!(value.get("token").is_none());
    }

    #[test]
    fn status_result_can_report_exited_container_diagnostics() {
        let value = serde_json::to_value(StatusResult {
            runtime: "docker",
            container: "afhttp-host".into(),
            running: false,
            endpoint: "ws://127.0.0.1:9222".into(),
            driver_version: VERSION,
            host_version: None,
            version_match: None,
            profile_kind: None,
            profile: None,
            profile_backend: None,
            backend: None,
            provider: None,
            takeover_ready: None,
            token_available: false,
            token_source: None,
            token_secret: None,
            client_command: None,
            exit_code: Some(42),
            log_summary: Some("browser stderr tail".into()),
            warnings: Vec::new(),
        })
        .unwrap();
        assert_eq!(value["exit_code"], 42);
        assert_eq!(value["log_summary"], "browser stderr tail");
        assert_eq!(value["driver_version"], VERSION);
        assert!(value.get("host_version").is_none());
        assert!(value.get("version_match").is_none());
        assert!(value.get("client_command").is_none());
    }

    #[test]
    fn host_version_warning_points_to_profile_preserving_reinstall() {
        let warning = host_version_warning(DEFAULT_NAME, "0.5.0").expect("warning");
        assert!(warning.contains("0.5.0"));
        assert!(warning.contains(VERSION));
        assert!(warning.contains("afhttp container install"));
        assert!(warning.contains("persistent profiles are preserved"));
        assert!(host_version_warning(DEFAULT_NAME, VERSION).is_none());
    }

    #[test]
    fn local_takeover_error_names_autodiscovery_and_manual_commands() {
        let err =
            local_takeover_error("default local container `afhttp-host` is not running".into());
        assert_eq!(err.error_code, ErrorCode::InvalidArgument);
        assert!(err.detail.contains("afhttp-host"));
        assert!(err.detail.contains("afhttp container install"));
        assert!(err.detail.contains("--endpoint-url/--token-secret"));
    }

    #[test]
    fn entrypoint_generates_base64url_token_secret() {
        assert!(ENTRYPOINT.contains("AFHTTP_TOKEN_SECRET"));
        let legacy_env_probe = ["AFHTTP", "TOKEN:-"].join("_");
        assert!(!ENTRYPOINT.contains(&legacy_env_probe));
        assert!(ENTRYPOINT.contains("head -c 32 /dev/urandom"));
        assert!(ENTRYPOINT.contains("base64 | tr '+/' '-_' | tr -d '=\\n'"));
        assert!(!ENTRYPOINT.contains("od -An -N32 -tx1"));
    }

    #[test]
    fn hard_site_install_defaults_expand_to_display_brave_preset() {
        let mut args = InstallArgs {
            common: CommonArgs {
                runtime: Some(Runtime::Docker),
                name: "afhttp-host".into(),
            },
            port: 9222,
            profile: None,
            shm_size: None,
            takeover_provider: TakeoverProviderArg::Kasmvnc,
            with: Vec::new(),
            rebuild: false,
            from_source: false,
            context: None,
            host_args: Vec::new(),
            reveal_token_secret: false,
        };
        apply_hard_site_defaults(&mut args);
        let backends = resolve_backends(&args.with).unwrap();
        validate_install_args(&args, &backends).unwrap();
        assert_eq!(effective_profile(&args), "-");
        assert_eq!(effective_shm_size(&args), "2g");
        assert_eq!(
            backends.iter().map(|b| b.name).collect::<Vec<_>>(),
            vec!["brave", "kasmvnc"]
        );
        assert_eq!(
            args.host_args,
            vec![
                "--browser".to_string(),
                "brave".to_string(),
                "--takeover-provider".to_string(),
                "kasmvnc".to_string(),
                format!("--browser-arg={HARD_SITE_BROWSER_ARG}")
            ]
        );
    }

    #[test]
    fn hard_site_install_keeps_valid_explicit_overrides_and_shm() {
        let mut args = InstallArgs {
            common: CommonArgs {
                runtime: Some(Runtime::Docker),
                name: "afhttp-host".into(),
            },
            port: 9222,
            profile: Some("work".into()),
            shm_size: Some("3g".into()),
            takeover_provider: TakeoverProviderArg::Kasmvnc,
            with: vec!["kasmvnc".into()],
            rebuild: false,
            from_source: false,
            context: None,
            host_args: vec![
                "--browser=brave".into(),
                "--takeover-provider=kasmvnc".into(),
                format!("--browser-arg={HARD_SITE_BROWSER_ARG}"),
            ],
            reveal_token_secret: false,
        };
        apply_hard_site_defaults(&mut args);
        let backends = resolve_backends(&args.with).unwrap();
        validate_install_args(&args, &backends).unwrap();
        assert_eq!(effective_shm_size(&args), "3g");
        assert_eq!(
            host_arg_value(&args.host_args, "--browser").as_deref(),
            Some("brave")
        );
        assert_eq!(
            backends.iter().map(|b| b.name).collect::<Vec<_>>(),
            vec!["kasmvnc", "brave"]
        );
    }

    #[test]
    fn hard_site_install_appends_stealth_browser_arg_without_clobbering_user_args() {
        let mut args = InstallArgs {
            common: CommonArgs {
                runtime: Some(Runtime::Docker),
                name: "afhttp-host".into(),
            },
            port: 9222,
            profile: None,
            shm_size: None,
            takeover_provider: TakeoverProviderArg::Kasmvnc,
            with: Vec::new(),
            rebuild: false,
            from_source: false,
            context: None,
            host_args: vec!["--browser-arg".into(), "--lang=zh-CN".into()],
            reveal_token_secret: false,
        };
        apply_hard_site_defaults(&mut args);
        let backends = resolve_backends(&args.with).unwrap();
        validate_install_args(&args, &backends).unwrap();
        assert!(host_arg_has_value(
            &args.host_args,
            "--browser-arg",
            "--lang=zh-CN"
        ));
        assert!(host_arg_has_value(
            &args.host_args,
            "--browser-arg",
            HARD_SITE_BROWSER_ARG
        ));
    }

    #[test]
    fn hard_site_install_allows_persistent_profile_with_brave() {
        let mut args = InstallArgs {
            common: CommonArgs {
                runtime: Some(Runtime::Docker),
                name: "afhttp-host".into(),
            },
            port: 9222,
            profile: Some("work".into()),
            shm_size: None,
            takeover_provider: TakeoverProviderArg::Kasmvnc,
            with: Vec::new(),
            rebuild: false,
            from_source: false,
            context: None,
            host_args: vec!["--browser".into(), "brave".into()],
            reveal_token_secret: false,
        };
        apply_hard_site_defaults(&mut args);
        let backends = resolve_backends(&args.with).unwrap();
        validate_install_args(&args, &backends).unwrap();
        assert_eq!(effective_profile(&args), "work");
    }

    #[test]
    fn hard_site_install_allows_ephemeral_initial_profile() {
        let mut args = InstallArgs {
            common: CommonArgs {
                runtime: Some(Runtime::Docker),
                name: "afhttp-host".into(),
            },
            port: 9222,
            profile: Some("-".into()),
            shm_size: None,
            takeover_provider: TakeoverProviderArg::Kasmvnc,
            with: Vec::new(),
            rebuild: false,
            from_source: false,
            context: None,
            host_args: vec!["--browser".into(), "brave".into()],
            reveal_token_secret: false,
        };
        apply_hard_site_defaults(&mut args);
        let backends = resolve_backends(&args.with).unwrap();
        validate_install_args(&args, &backends).unwrap();
        assert_eq!(effective_profile(&args), "-");
    }

    #[test]
    fn hard_site_install_rejects_non_brave_browser_override() {
        let mut args = InstallArgs {
            common: CommonArgs {
                runtime: Some(Runtime::Docker),
                name: "afhttp-host".into(),
            },
            port: 9222,
            profile: None,
            shm_size: None,
            takeover_provider: TakeoverProviderArg::Kasmvnc,
            with: Vec::new(),
            rebuild: false,
            from_source: false,
            context: None,
            host_args: vec!["--browser".into(), "chromium".into()],
            reveal_token_secret: false,
        };
        apply_hard_site_defaults(&mut args);
        let backends = resolve_backends(&args.with).unwrap();
        let err = validate_install_args(&args, &backends).unwrap_err();
        assert_eq!(err.error_code, ErrorCode::InvalidArgument);
        assert!(err.detail.contains("--browser brave"));
        assert!(err.detail.contains("afhttp container install"));
    }

    #[test]
    fn hard_site_install_rejects_missing_browser_value() {
        let mut args = InstallArgs {
            common: CommonArgs {
                runtime: Some(Runtime::Docker),
                name: "afhttp-host".into(),
            },
            port: 9222,
            profile: None,
            shm_size: None,
            takeover_provider: TakeoverProviderArg::Kasmvnc,
            with: Vec::new(),
            rebuild: false,
            from_source: false,
            context: None,
            host_args: vec!["--browser".into()],
            reveal_token_secret: false,
        };
        apply_hard_site_defaults(&mut args);
        let backends = resolve_backends(&args.with).unwrap();
        let err = validate_install_args(&args, &backends).unwrap_err();
        assert_eq!(err.error_code, ErrorCode::InvalidArgument);
        assert!(err.detail.contains("--browser brave"));
    }

    #[test]
    fn takeover_host_args_trigger_image_support_probe() {
        assert!(host_args_need_takeover_support(&[
            "--takeover-provider".into(),
            "kasmvnc".into()
        ]));
        assert!(host_args_need_takeover_support(&[
            "--takeover-provider=kasmvnc".into()
        ]));
        assert!(!host_args_need_takeover_support(&[
            "--takeover-provider".into(),
            "off".into()
        ]));
        assert!(!host_args_need_takeover_support(&[
            "--takeover-provider".into(),
            "none".into()
        ]));
        assert!(!host_args_need_takeover_support(&[
            "--browser".into(),
            "brave".into()
        ]));
    }

    #[test]
    fn image_host_help_args_bypasses_entrypoint() {
        let args = image_host_help_args("afhttp-host:dev");
        assert_eq!(args[0], "run");
        assert!(args.contains(&"--rm".to_string()));
        assert!(args.contains(&"--entrypoint".to_string()));
        assert!(args.contains(&"/usr/local/bin/afhttp".to_string()));
        assert_eq!(args[args.len() - 3], "afhttp-host:dev");
        assert_eq!(args[args.len() - 2], "host");
        assert_eq!(args[args.len() - 1], "--help");
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
        let cmd = client_command(9333);
        assert!(cmd.contains("--endpoint-url ws://127.0.0.1:9333"));
        assert!(cmd.contains("AFHTTP_TOKEN_SECRET=<host-token>"));
        assert!(!cmd.contains("deadbeef"));
    }

    #[test]
    fn build_failure_error_points_at_compose_fallback() {
        let err = build_failed_error(
            "aarch64-unknown-linux-gnu",
            Path::new("/tmp/afhttp-container-logs/build.log"),
        );
        assert_eq!(err.error_code, ErrorCode::InternalError);
        assert!(err.detail.contains("compose"));
        assert!(err.detail.contains("aarch64-unknown-linux-gnu"));
        assert!(err.detail.contains("build.log"));
    }

    #[test]
    fn tail_lines_from_file_reports_truncation_without_full_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("container.log");
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let (tail, truncated) = tail_lines_from_file(&path, 2).unwrap();
        assert_eq!(tail, vec!["two".to_string(), "three".to_string()]);
        assert!(truncated);
    }
}
