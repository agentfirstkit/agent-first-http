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
    /// Display-provider image quality as a percentage (0-100). Higher is
    /// crisper but uses more bandwidth. Current KasmVNC provider maps this to
    /// its 0-9 quality tiers.
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
    Screencast,
    Display { provider: DisplayProvider },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayProvider {
    KasmVnc,
}

impl DisplayProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::KasmVnc => "kasmvnc",
        }
    }
}

impl std::str::FromStr for DisplayProvider {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "kasmvnc" => Self::KasmVnc,
            other => return Err(format!("unknown {other:?}; expected kasmvnc")),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BrowserChoice {
    #[default]
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

impl std::str::FromStr for BrowserChoice {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "auto" => Self::Auto,
            "chromium" => Self::Chromium,
            "chrome" => Self::Chrome,
            "chrome_shell" | "chrome-headless-shell" => Self::ChromeShell,
            "fingerprint_chromium" | "fingerprint-chromium" => Self::FingerprintChromium,
            "edge" => Self::Edge,
            "brave" => Self::Brave,
            "lightpanda" => Self::Lightpanda,
            "camoufox" => Self::Camoufox,
            other => return Err(format!("unknown {other:?}")),
        })
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_choice_default_is_auto() {
        assert_eq!(BrowserChoice::default(), BrowserChoice::Auto);
    }

    #[test]
    fn browser_choice_parses_every_variant_and_aliases() {
        assert_eq!(
            "auto".parse::<BrowserChoice>().unwrap(),
            BrowserChoice::Auto
        );
        assert_eq!(
            "chromium".parse::<BrowserChoice>().unwrap(),
            BrowserChoice::Chromium
        );
        assert_eq!(
            "chrome".parse::<BrowserChoice>().unwrap(),
            BrowserChoice::Chrome
        );
        // Both spellings of the headless-shell and fingerprint backends.
        assert_eq!(
            "chrome_shell".parse::<BrowserChoice>().unwrap(),
            BrowserChoice::ChromeShell
        );
        assert_eq!(
            "chrome-headless-shell".parse::<BrowserChoice>().unwrap(),
            BrowserChoice::ChromeShell
        );
        assert_eq!(
            "fingerprint_chromium".parse::<BrowserChoice>().unwrap(),
            BrowserChoice::FingerprintChromium
        );
        assert_eq!(
            "fingerprint-chromium".parse::<BrowserChoice>().unwrap(),
            BrowserChoice::FingerprintChromium
        );
        assert_eq!(
            "edge".parse::<BrowserChoice>().unwrap(),
            BrowserChoice::Edge
        );
        assert_eq!(
            "brave".parse::<BrowserChoice>().unwrap(),
            BrowserChoice::Brave
        );
        assert_eq!(
            "lightpanda".parse::<BrowserChoice>().unwrap(),
            BrowserChoice::Lightpanda
        );
        assert_eq!(
            "camoufox".parse::<BrowserChoice>().unwrap(),
            BrowserChoice::Camoufox
        );
    }

    #[test]
    fn browser_choice_rejects_unknown() {
        let err = "netscape".parse::<BrowserChoice>().unwrap_err();
        assert!(err.contains("netscape"), "error was {err:?}");
    }
}
