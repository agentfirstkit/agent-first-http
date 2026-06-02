//! `afhttp host` startup: parse args, install rustls provider, install
//! signal handlers, build state, bind the listener, and block until
//! shutdown.

use std::path::PathBuf;

use crate::host::listener::AppState;
use crate::shared::error::Error;

/// Parsed command-line arguments for `afhttp host`. The CLI layer in
/// `cli::cmd::host` builds this struct from `clap`.
#[derive(Debug, Clone)]
pub struct HostArgs {
    pub listen: String,
    /// The single browsing identity bound to this host.
    pub profile: ProfileChoice,
    pub display: DisplayMode,
    pub takeover: Takeover,
    /// Display-takeover image quality as a percentage (0-100), mapped to
    /// KasmVNC's 0-9 quality tiers. Higher is crisper but uses more bandwidth.
    /// Only meaningful when `takeover` is `KasmVnc`.
    pub display_quality: u8,
    pub browser: BrowserChoice,
    pub browser_bin: Option<PathBuf>,
    pub token: Option<String>,
    pub ops_enabled: bool,
    pub health_enabled: bool,
    pub health_public: HealthPublic,
    /// Explicit environment variables to propagate into the backend
    /// subprocess. The host never silently forwards the parent process's
    /// `HTTP_PROXY`, `XDG_*`, `BROWSER`, etc. — agents that genuinely
    /// need an env var inside the browser pass it here.
    pub engine_envs: Vec<(String, String)>,
    /// Raw command-line arguments appended to the backend subprocess
    /// after the host's curated defaults. Used for backend-specific
    /// surfaces the host does not model first-class (e.g.
    /// `--fingerprint-brand=Chrome` for fingerprint-chromium). Chromium
    /// honors last-wins for duplicate flags, so an explicit entry here
    /// overrides any default the host applied.
    pub browser_args: Vec<String>,
    /// Explicit upstream HTTP/HTTPS proxy for all browser traffic in
    /// this host instance. Per the isolation invariant
    /// (`design.md` "Browsing environments are isolated"), the host
    /// never honors ambient `HTTP_PROXY`/`HTTPS_PROXY` — this flag is
    /// the ONLY way to route browser traffic through a proxy.
    /// Format: `http://user:pass@host:port` or `socks5://host:port`.
    pub proxy: Option<String>,
    /// Maximum number of recent requests to keep in the ring. `0` (default)
    /// disables the `/recent-requests` endpoint entirely.
    pub recent_requests_cap: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileChoice {
    Ephemeral,
    Persistent(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayMode {
    Headless,
    Headful,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Takeover {
    Off,
    KasmVnc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserChoice {
    Auto,
    Chromium,
    Chrome,
    ChromeShell,
    FingerprintChromium,
    Edge,
    Brave,
    Lightpanda,
    /// Camoufox (Firefox stealth fork) driven via the foxbridge
    /// CDP→Juggler proxy. The host spawns foxbridge which in turn
    /// spawns camoufox; the agent sees a chromium-style WS endpoint.
    Camoufox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthPublic {
    Off,
    Minimal,
}

pub fn install_rustls_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// Run the host until SIGTERM/SIGINT. Launches the backend browser, builds
/// the listener state around it, and serves until the shutdown signal.
pub async fn run(args: HostArgs) -> Result<(), Error> {
    install_rustls_provider();
    let state = AppState::launch(&args).await?;
    state.serve(&args.listen).await
}
