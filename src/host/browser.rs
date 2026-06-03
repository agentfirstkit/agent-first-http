//! Browser handle. Holds a backend subprocess plus the discovered CDP WS URL.
//!
//! Chromium-family backends are spawned directly with an isolated environment
//! and then connected through chromiumoxide. Lightpanda and Camoufox are also
//! raw subprocesses. Callers outside this module only ever read `ws_url`; the
//! engine-specific keepalive lives in a private enum so each variant can clean
//! up on drop.

use std::collections::VecDeque;
use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chromiumoxide::Browser;
use tokio::io::AsyncBufReadExt;
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::host::bootstrap::{BrowserChoice, HostArgs};
use crate::shared::error::{Error, ErrorCode};

mod camoufox;
mod chromium;
mod lightpanda;
mod profile_launch;

/// Maximum number of stderr lines buffered per browser process.
const STDERR_RING_CAP: usize = 200;

/// A running browser. `Drop` cleans up the engine-specific resources:
/// aborting chromiumoxide event loops and terminating backend subprocesses.
pub struct BrowserHandle {
    pub ws_url: String,
    pub family: String,
    pub version: String,
    /// OS process id for the primary backend process, when the host spawned one.
    pub process_id: Option<u32>,
    /// Resolved on-disk profile directory. Either the persistent profile
    /// path under `$XDG_DATA_HOME/afhttp/profiles/<name>/` or the
    /// ephemeral tempdir backing this host.
    pub profile_path: PathBuf,
    /// Browser-initiated downloads are captured inside the active profile.
    pub download_dir: PathBuf,
    /// `Some(path)` for the runtime tempdir backing an ephemeral profile.
    /// Dropping the TempDir removes it from disk.
    pub _ephemeral_dir: Option<tempfile::TempDir>,
    /// Held for persistent profiles so lifecycle tooling can detect active use.
    pub _profile_lock: Option<crate::sdk::profile::lock::Guard>,
    _keepalive: BackendKeepalive,
    /// Tail of the browser subprocess's stderr, capped at [`STDERR_RING_CAP`]
    /// lines.
    pub stderr_ring: Arc<Mutex<VecDeque<String>>>,
}

/// Engine-specific resources that must outlive every fetch against the host.
/// Kept opaque so call sites can't reach into chromiumoxide types when the
/// backend happens to be Lightpanda (or vice versa).
enum BackendKeepalive {
    Chromium {
        _browser: Arc<Mutex<Browser>>,
        handler_task: JoinHandle<()>,
        child: Child,
    },
    /// Generic subprocess slot used by CDP-compatible subprocess backends
    /// (lightpanda's own `serve`, the foxbridge -> camoufox stack, future
    /// subprocess-driven engines). `kill_on_drop` plus the explicit
    /// `start_kill` below guarantee the child process tree dies with
    /// this handle.
    Subprocess { child: Child },
    /// No-op keepalive for synthetic handles (tests only).
    None,
}

impl Drop for BackendKeepalive {
    fn drop(&mut self) {
        match self {
            BackendKeepalive::Chromium {
                handler_task,
                child,
                ..
            } => {
                handler_task.abort();
                let _ = child.start_kill();
            }
            BackendKeepalive::Subprocess { child, .. } => {
                // start_kill is non-blocking and the only thing we can do
                // from a synchronous Drop. The subprocess gets SIGKILL'd by
                // the OS; tempdir cleanup happens after.
                let _ = child.start_kill();
            }
            BackendKeepalive::None => {}
        }
    }
}

impl BrowserHandle {
    /// Create a synthetic handle that carries only a `profile_path`. Used
    /// in tests that exercise the HTTP path without a real browser subprocess.
    #[cfg(any(test, feature = "host"))]
    pub fn synthetic(profile_path: PathBuf) -> Self {
        BrowserHandle {
            ws_url: String::new(),
            family: "synthetic".to_string(),
            version: String::new(),
            process_id: None,
            profile_path,
            download_dir: PathBuf::new(),
            _ephemeral_dir: None,
            _profile_lock: None,
            _keepalive: BackendKeepalive::None,
            stderr_ring: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    #[cfg(test)]
    pub(crate) fn synthetic_ephemeral(ephemeral_dir: tempfile::TempDir) -> Self {
        let profile_path = ephemeral_dir.path().to_path_buf();
        BrowserHandle {
            ws_url: String::new(),
            family: "synthetic".to_string(),
            version: String::new(),
            process_id: None,
            profile_path,
            download_dir: PathBuf::new(),
            _ephemeral_dir: Some(ephemeral_dir),
            _profile_lock: None,
            _keepalive: BackendKeepalive::None,
            stderr_ring: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
}

pub async fn launch(args: &HostArgs) -> Result<BrowserHandle, Error> {
    match args.browser {
        BrowserChoice::Lightpanda => lightpanda::launch(args).await,
        BrowserChoice::Camoufox => camoufox::launch(args).await,
        _ => chromium::launch(args).await,
    }
}

/// Stable 32-bit FNV-1a hash of the profile path, used to seed
/// fingerprint-chromium. Persistent profiles repeat across host
/// restarts (so the spoofed surface stays consistent); ephemeral
/// tempdir paths are unique per host instance (so each ephemeral host
/// gets a distinct fingerprint). FNV-1a is portable and stable across
/// Rust versions, unlike `std::hash::DefaultHasher`.
fn fingerprint_seed_from_path(path: &std::path::Path) -> u32 {
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut h: u32 = 0x811c_9dc5;
    for b in bytes {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    // The upstream tool documents `--fingerprint=<u32>`; we coerce
    // away the value 0 so an unlikely all-zeros hash doesn't disable
    // the spoofing pipeline by accident.
    if h == 0 {
        1
    } else {
        h
    }
}

async fn ensure_download_dir(profile_dir: &std::path::Path) -> Result<PathBuf, Error> {
    let dir = profile_dir.join("downloads");
    tokio::fs::create_dir_all(&dir).await.map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("create download dir {}: {e}", dir.display()),
        )
    })?;
    Ok(dir)
}

/// Configure a subprocess `Command` for backend launch with full env
/// isolation: scrub everything, then re-inject the small allowlist of vars
/// we actually need plus the explicit `--engine-env` passthroughs.
///
/// Allowlist rationale:
/// - `PATH` — child may shell-exec helper utilities (DNS resolvers, fonts).
/// - `HOME` — chromium falls back here for some XDG paths even with
///   `--user-data-dir`; lightpanda uses it for cache dirs.
/// - `LANG`, `LC_*` — engine locale is an honest engine-level fingerprint
///   surface; agents that want to override pass `--engine-env`.
/// - `TZ` — same reasoning for timezone.
/// - `TMPDIR` — chromium's child processes use this for IPC.
/// - `DISPLAY` — only meaningful for headful mode, but always cheap to
///   pass through; the engine ignores it under headless.
///
/// Deliberate omissions (these are silent egress / behavior leaks):
/// - `HTTP_PROXY`, `HTTPS_PROXY`, `SOCKS_PROXY`, `NO_PROXY`,
///   `ALL_PROXY` — explicit `--proxy-url` (future flag) only.
/// - `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `XDG_CACHE_HOME` — we override
///   with `--user-data-dir`; honoring these too could escape the profile.
/// - `BROWSER` — affects xdg-open inside the engine.
/// - `CHROME_*`, `MOZ_*`, `LIGHTPANDA_*` — engine-specific tunables.
fn apply_subprocess_env(cmd: &mut Command, engine_envs: &[(String, String)]) {
    cmd.env_clear();
    const ALLOWLIST: &[&str] = &[
        "PATH", "HOME", "LANG", "LC_ALL", "LC_CTYPE", "TZ", "TMPDIR", "DISPLAY",
    ];
    // Windows-essential variables. Without SYSTEMROOT the browser cannot
    // initialize winsock (`WSALookupServiceBegin` fails) and the CDP/DevTools
    // HTTP server never binds — so a scrubbed env silently breaks every
    // browser-backed fetch on Windows. These are OS plumbing, not ambient
    // browsing config (HTTP_PROXY/XDG/BROWSER stay scrubbed per the isolation
    // invariant).
    #[cfg(windows)]
    const WINDOWS_ALLOWLIST: &[&str] = &[
        "SYSTEMROOT",
        "SystemDrive",
        "windir",
        "TEMP",
        "TMP",
        "APPDATA",
        "LOCALAPPDATA",
        "USERPROFILE",
        "ProgramData",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "PATHEXT",
        "COMSPEC",
        "NUMBER_OF_PROCESSORS",
        "PROCESSOR_ARCHITECTURE",
    ];
    for key in ALLOWLIST {
        if let Ok(value) = std::env::var(key) {
            cmd.env(key, value);
        }
    }
    #[cfg(windows)]
    for key in WINDOWS_ALLOWLIST {
        if let Ok(value) = std::env::var(key) {
            cmd.env(key, value);
        }
    }
    for (k, v) in engine_envs {
        cmd.env(k, v);
    }
}

/// Spawn a task that reads `stderr` lines into a bounded ring buffer. Returns
/// an empty ring when stderr is not piped.
fn new_stderr_ring(stderr: Option<tokio::process::ChildStderr>) -> Arc<Mutex<VecDeque<String>>> {
    let ring = Arc::new(Mutex::new(VecDeque::<String>::new()));
    if let Some(stderr) = stderr {
        let ring_w = ring.clone();
        tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let mut guard = ring_w.lock().await;
                if guard.len() >= STDERR_RING_CAP {
                    guard.pop_front();
                }
                guard.push_back(line);
            }
        });
    }
    ring
}

/// Look up a specifically named binary on the standard install paths.
/// `override_bin` lets the host accept an explicit path for the primary
/// binary (foxbridge); the secondary (camoufox) is always discovered.
pub(crate) fn resolve_named_bin(
    name: &str,
    override_bin: &Option<PathBuf>,
) -> Result<PathBuf, Error> {
    if let Some(p) = override_bin {
        if p.file_name().and_then(|n| n.to_str()) == Some(name) && p.exists() {
            return Ok(p.clone());
        }
    }
    for dir in [
        "/usr/local/bin",
        "/usr/bin",
        "/opt/camoufox",
        "/opt/foxbridge",
    ] {
        let candidate = PathBuf::from(dir).join(name);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            let candidate = PathBuf::from(dir).join(name);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    Err(Error::new(
        ErrorCode::BrowserLaunchFailed,
        format!("could not find {name} binary on PATH"),
    ))
}

/// Reserve a localhost port by binding a TCP listener, reading its assigned
/// port, and closing it. There is a small window before lightpanda binds
/// the same port in which a third process could steal it — extremely
/// unlikely in practice and the launch will fail loudly if it happens.
pub(crate) fn pick_ephemeral_port() -> std::io::Result<u16> {
    let listener = StdTcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// Poll the given (host, port) with TCP connects until the target accepts
/// a connection or `timeout` elapses.
pub(crate) async fn wait_for_tcp_ready(
    target: (&str, u16),
    timeout: Duration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    let addr: SocketAddr = format!("{}:{}", target.0, target.1)
        .parse()
        .map_err(|e| format!("parse {}:{}: {e}", target.0, target.1))?;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("timed out after {timeout:?}"));
        }
        match tokio::time::timeout(Duration::from_millis(200), TcpStream::connect(addr)).await {
            Ok(Ok(_)) => return Ok(()),
            _ => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
}

fn resolve_browser_bin(args: &HostArgs) -> Result<PathBuf, Error> {
    if let Some(p) = &args.browser_bin {
        if !p.exists() {
            return Err(Error::new(
                ErrorCode::BrowserLaunchFailed,
                format!("--browser-bin {} does not exist", p.display()),
            ));
        }
        return Ok(resolve_chromium_wrapper_target(p));
    }
    let candidates: Vec<&str> = match args.browser {
        BrowserChoice::Lightpanda => vec!["lightpanda"],
        BrowserChoice::Chrome => vec!["google-chrome", "google-chrome-stable", "chrome"],
        BrowserChoice::ChromeShell => vec!["chrome-headless-shell"],
        BrowserChoice::FingerprintChromium => vec!["fingerprint-chromium"],
        BrowserChoice::Edge => vec!["microsoft-edge", "edge"],
        BrowserChoice::Brave => vec!["brave-browser", "brave"],
        BrowserChoice::Chromium | BrowserChoice::Auto => vec![
            "chromium",
            "chromium-browser",
            "google-chrome",
            "google-chrome-stable",
        ],
        // Camoufox is launched via launch_camoufox(), not this helper —
        // resolve_browser_bin is only reachable from chromium-family + the
        // lightpanda path. Return an unambiguous error if the dispatcher
        // somehow regressed.
        BrowserChoice::Camoufox => {
            return Err(Error::new(
                ErrorCode::InternalError,
                "resolve_browser_bin invoked for camoufox; should route through launch_camoufox",
            ));
        }
    };
    for name in candidates {
        for dir in [
            "/usr/bin",
            "/usr/local/bin",
            "/opt/google/chrome",
            "/Applications/Google Chrome.app/Contents/MacOS",
        ] {
            let p = PathBuf::from(dir).join(name);
            if p.exists() {
                return Ok(resolve_chromium_wrapper_target(&p));
            }
        }
        // Walk $PATH.
        if let Ok(path) = std::env::var("PATH") {
            for dir in path.split(':') {
                let p = PathBuf::from(dir).join(name);
                if p.exists() {
                    return Ok(resolve_chromium_wrapper_target(&p));
                }
            }
        }
    }

    // Standard app-bundle / Program Files locations the name×dir loop can't
    // express: macOS binaries contain a space ("Google Chrome") and Windows
    // installs live outside $PATH. Only meaningful for the chromium/chrome
    // family; on Linux this block is compiled out entirely.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    if matches!(
        args.browser,
        BrowserChoice::Auto | BrowserChoice::Chromium | BrowserChoice::Chrome
    ) {
        let mut app_candidates: Vec<PathBuf> = Vec::new();
        #[cfg(target_os = "macos")]
        {
            let mut roots = vec![PathBuf::from("/Applications")];
            if let Ok(home) = std::env::var("HOME") {
                roots.push(PathBuf::from(home).join("Applications"));
            }
            for root in roots {
                app_candidates.push(root.join("Google Chrome.app/Contents/MacOS/Google Chrome"));
                app_candidates.push(root.join(
                    "Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
                ));
                app_candidates.push(root.join("Chromium.app/Contents/MacOS/Chromium"));
            }
        }
        #[cfg(target_os = "windows")]
        {
            let mut roots: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
                .iter()
                .filter_map(|var| std::env::var(var).ok())
                .map(PathBuf::from)
                .collect();
            roots.push(PathBuf::from(r"C:\Program Files"));
            roots.push(PathBuf::from(r"C:\Program Files (x86)"));
            for root in roots {
                app_candidates.push(root.join(r"Google\Chrome\Application\chrome.exe"));
                app_candidates.push(root.join(r"Chromium\Application\chrome.exe"));
            }
        }
        for p in app_candidates {
            if p.exists() {
                return Ok(resolve_chromium_wrapper_target(&p));
            }
        }
    }

    Err(Error::new(
        ErrorCode::BrowserLaunchFailed,
        "no browser binary found; set --browser-bin or install chromium",
    ))
}

fn resolve_chromium_wrapper_target(path: &std::path::Path) -> PathBuf {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return path.to_path_buf();
    };
    if name != "chromium" && name != "chromium-browser" {
        return path.to_path_buf();
    }
    for candidate in [
        "/usr/lib/chromium/chromium",
        "/usr/lib/chromium-browser/chromium-browser",
    ] {
        let actual = PathBuf::from(candidate);
        if actual.exists() {
            return actual;
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_ephemeral_port_returns_usable_local_port() {
        let port = pick_ephemeral_port().expect("pick");
        assert!(port > 0);
        // Can rebind immediately — confirms the listener was dropped cleanly
        // and the port is available for the lightpanda subprocess.
        let l = StdTcpListener::bind(("127.0.0.1", port)).expect("rebind");
        drop(l);
    }

    #[test]
    fn fingerprint_seed_is_stable_per_path_and_distinct_across_paths() {
        let a = std::path::PathBuf::from("/var/lib/afhttp/profiles/work");
        let b = std::path::PathBuf::from("/var/lib/afhttp/profiles/other");
        // Same path → same seed across calls (the agent's identity
        // contract: persistent profile keeps its fingerprint).
        assert_eq!(
            fingerprint_seed_from_path(&a),
            fingerprint_seed_from_path(&a)
        );
        // Different paths → almost certainly different seeds.
        assert_ne!(
            fingerprint_seed_from_path(&a),
            fingerprint_seed_from_path(&b)
        );
        // Zero is never returned (would no-op the upstream tool).
        assert_ne!(fingerprint_seed_from_path(std::path::Path::new("")), 0);
    }
}
