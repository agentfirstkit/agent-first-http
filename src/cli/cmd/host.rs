//! `afhttp host` subcommand. Builds [`HostArgs`] from clap and forwards to
//! [`crate::host::bootstrap::run`].

use std::path::PathBuf;

use clap::Args as ClapArgs;

use crate::cli::cmd::argenums::{BrowserArg, DisplayArg, HealthPublicArg, TakeoverProviderArg};
use crate::host::bootstrap::{
    BrowserChoice, DisplayMode, HealthPublic, HostArgs, ProfileChoice, Takeover,
};
use crate::host::listener::{parse_listen, ListenAddr};
use crate::shared::error::{Error, ErrorCode};

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Listener address: `tcp:host:port` or `unix:/path/to.sock`.
    #[arg(long, help_heading = "Listener")]
    pub listen: String,
    /// Initial logical profile name, or `-` for an ephemeral profile.
    /// Persistent profiles are stored under
    /// $XDG_DATA_HOME/afhttp/profiles/<backend>/<name>. A host serves one
    /// active profile at a time but can switch at runtime when a client passes
    /// `?profile=` on the `/cdp` connection (the browser is relaunched).
    #[arg(long, default_value = "-", help_heading = "Profile")]
    pub profile: String,
    /// Display mode. Omit when `--takeover-provider` should imply headful.
    #[arg(long, help_heading = "Display & takeover")]
    pub display: Option<DisplayArg>,
    /// Real-display takeover provider: `off` serves no takeover surface; a
    /// provider name (currently `kasmvnc`) serves a real-display takeover at
    /// /takeover/panel for hard sites (captcha, IME, flaky CDP input —
    /// implies headful).
    #[arg(
        long = "takeover-provider",
        default_value = "off",
        help_heading = "Display & takeover"
    )]
    pub takeover_provider: TakeoverProviderArg,
    /// Takeover-provider image quality hint, 0-100 percent (default 100 = crispest).
    /// The KasmVNC provider maps this to 0-9 quality tiers; lower trades
    /// clarity for bandwidth. Adjustable live in the display panel too.
    #[arg(
        long = "takeover-quality-percent",
        default_value_t = 100,
        help_heading = "Display & takeover"
    )]
    pub takeover_quality_percent: u8,
    /// Browser backend.
    #[arg(long, default_value = "auto", help_heading = "Browser")]
    pub browser: BrowserArg,
    /// Override browser binary path.
    #[arg(long, help_heading = "Browser")]
    pub browser_bin: Option<PathBuf>,
    /// Bearer token required for clients on TCP listeners.
    #[arg(long = "token-secret", help_heading = "Listener")]
    pub token: Option<String>,
    /// Disable serving /health and /capabilities (served by default).
    #[arg(long, help_heading = "Diagnostics")]
    pub no_health: bool,
    /// Make /health public with minimal payload.
    #[arg(long, default_value = "off", help_heading = "Diagnostics")]
    pub health_public: HealthPublicArg,
    /// Propagate an environment variable into the browser subprocess.
    /// Repeatable. The host scrubs all other ambient env (`HTTP_PROXY`,
    /// `XDG_*`, `BROWSER`, locale, etc.) so a browsing environment can
    /// never silently honor configuration the agent did not request.
    /// Use the form `NAME=VALUE`.
    #[arg(
        long = "engine-env",
        value_name = "NAME=VALUE",
        help_heading = "Browser"
    )]
    pub engine_envs: Vec<String>,
    /// Append a raw flag to the backend subprocess command line.
    /// Repeatable. Use for backend-specific surfaces the host doesn't
    /// model first-class — for example
    /// `--browser-arg --fingerprint-brand=Chrome` to override
    /// fingerprint-chromium's brand string. Chromium honors last-wins
    /// for duplicate flags, so an explicit entry overrides any
    /// default the host applied.
    #[arg(long = "browser-arg", value_name = "FLAG", help_heading = "Browser")]
    pub browser_args: Vec<String>,
    /// Explicit upstream proxy URL. The host never inherits
    /// `HTTP_PROXY`/`HTTPS_PROXY` from the environment — this is the
    /// only way to route browser traffic. Example:
    /// `http://user:pass@proxy.local:8080` or `socks5://10.0.0.5:1080`.
    #[arg(long = "proxy-url", help_heading = "Browser")]
    pub proxy: Option<String>,
    /// Enable /recent-requests with a bounded ring of N entries. 0 = off.
    #[arg(long, default_value_t = 0, help_heading = "Diagnostics")]
    pub recent_requests_cap: usize,
}

pub async fn run(args: Args) -> Result<(), Error> {
    enforce_listen_auth(&args.listen, args.token.as_deref())?;
    let profile_raw = args.profile.trim();
    if profile_raw.is_empty() || profile_raw.contains(',') {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "--profile: expected one profile name or '-'",
        ));
    }
    let profile = if profile_raw == "-" {
        ProfileChoice::Ephemeral
    } else {
        ProfileChoice::Persistent(profile_raw.to_string())
    };
    let takeover: Takeover = args.takeover_provider.into();
    let takeover_enabled = !matches!(takeover, Takeover::Off);
    let display_explicit = args.display.is_some();
    let mut display = args
        .display
        .map(DisplayMode::from)
        .unwrap_or(DisplayMode::Headless);
    if matches!(takeover, Takeover::On { .. }) {
        if display_explicit && matches!(display, DisplayMode::Headless) {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "--takeover-provider <provider> requires a headful browser; omit --display or pass --display headful",
            ));
        }
        display = DisplayMode::Headful;
    }
    let browser: BrowserChoice = args.browser.into();
    let health_public: HealthPublic = args.health_public.into();
    let health_enabled: bool = !args.no_health;
    let mut engine_envs = Vec::with_capacity(args.engine_envs.len());
    for raw in &args.engine_envs {
        engine_envs.push(parse_engine_env(raw)?);
    }
    if args.takeover_quality_percent > 100 {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            format!(
                "--takeover-quality-percent: must be 0-100, got {}",
                args.takeover_quality_percent
            ),
        ));
    }
    let host_args = HostArgs {
        listen: args.listen,
        profile,
        display,
        takeover,
        display_quality: args.takeover_quality_percent,
        browser,
        browser_bin: args.browser_bin,
        token: args.token,
        takeover_enabled,
        health_enabled,
        health_public,
        engine_envs,
        browser_args: args.browser_args,
        proxy: args.proxy,
        recent_requests_cap: args.recent_requests_cap,
    };
    crate::host::bootstrap::run(host_args).await
}

/// Refuse to expose a token-less control surface to the network. A TCP listener
/// on any non-loopback address (`0.0.0.0`, a LAN/mesh IP, …) serves `/cdp` —
/// full browser and profile control plus arbitrary in-page JS — to anyone who
/// can reach the port, so a token is mandatory there. Loopback TCP and unix
/// sockets are reachable only locally, so a token stays optional. (When a token
/// is set we skip the parse here; the listener validates the address later.)
fn enforce_listen_auth(listen: &str, token: Option<&str>) -> Result<(), Error> {
    if token.is_some() {
        return Ok(());
    }
    if let ListenAddr::Tcp(addr) = parse_listen(listen)? {
        if !addr.ip().is_loopback() {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                format!(
                    "--listen {listen}: refusing to bind a non-loopback address without --token-secret. \
                     A token-less TCP host exposes full browser and profile control (/cdp) to \
                     anyone who can reach the port. Pass --token-secret, or bind tcp:127.0.0.1:<port> \
                     or a unix: socket."
                ),
            ));
        }
    }
    Ok(())
}

fn parse_engine_env(raw: &str) -> Result<(String, String), Error> {
    let (k, v) = raw.split_once('=').ok_or_else(|| {
        Error::new(
            ErrorCode::InvalidArgument,
            format!("--engine-env: expected NAME=VALUE, got {raw:?}"),
        )
    })?;
    if k.is_empty() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "--engine-env: key must not be empty",
        ));
    }
    Ok((k.to_string(), v.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_tcp_needs_no_token() {
        enforce_listen_auth("tcp:127.0.0.1:9222", None).unwrap();
        enforce_listen_auth("tcp:[::1]:9222", None).unwrap();
    }

    #[test]
    fn unix_socket_needs_no_token() {
        enforce_listen_auth("unix:/run/afhttp.sock", None).unwrap();
    }

    #[test]
    fn non_loopback_tcp_requires_token() {
        for spec in ["tcp:0.0.0.0:9222", "tcp:192.168.1.10:9222", "tcp:[::]:9222"] {
            let err = enforce_listen_auth(spec, None).err().unwrap();
            assert_eq!(err.error_code, ErrorCode::InvalidArgument, "spec={spec}");
        }
    }

    #[test]
    fn token_allows_any_address() {
        enforce_listen_auth("tcp:0.0.0.0:9222", Some("secret")).unwrap();
    }
}
