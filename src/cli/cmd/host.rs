//! `afhttp host` subcommand. Builds [`HostArgs`] from clap and forwards to
//! [`crate::host::bootstrap::run`].

use std::path::PathBuf;

use clap::Args as ClapArgs;

use crate::host::bootstrap::{
    BrowserChoice, DisplayMode, DisplayProvider, HealthPublic, HostArgs, ProfileChoice, Takeover,
};
use crate::host::listener::{parse_listen, ListenAddr};
use crate::shared::error::{Error, ErrorCode};

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Listener address: `tcp:host:port` or `unix:/path/to.sock`.
    #[arg(long, help_heading = "Listener")]
    pub listen: String,
    /// Profile name under $XDG_DATA_HOME/afhttp/profiles, or `-` for an
    /// ephemeral profile. One host binds exactly one profile.
    #[arg(long, default_value = "-", help_heading = "Profile")]
    pub profile: String,
    /// headless or headful. Omit when --takeover display should imply headful.
    #[arg(long, help_heading = "Display & takeover")]
    pub display: Option<String>,
    /// Human takeover mode (like --render, pick one): none serves no takeover
    /// panel; screencast serves the CDP screencast panel at /ops/screencast
    /// (works headless, no VNC/X needed); display serves a real-display
    /// takeover at /ops/display through a display provider (currently KasmVNC)
    /// for hard sites (captcha, IME, flaky CDP input — implies headful).
    #[arg(
        long,
        default_value = "screencast",
        help_heading = "Display & takeover"
    )]
    pub takeover: String,
    /// Display provider for `--takeover display`. Currently only `kasmvnc` is
    /// supported and it is the default when display takeover is selected.
    #[arg(long = "display-provider", help_heading = "Display & takeover")]
    pub display_provider: Option<String>,
    /// Display-provider image quality hint, 0-100 (default 100 = crispest).
    /// Current KasmVNC provider maps this to 0-9 quality tiers; lower trades
    /// clarity for bandwidth. Adjustable live in the display panel too.
    #[arg(
        long = "display-quality-percent",
        default_value_t = 100,
        help_heading = "Display & takeover"
    )]
    pub display_quality: u8,
    /// auto | chromium | chrome | chrome_shell | fingerprint_chromium | edge | brave | lightpanda | camoufox.
    #[arg(long, default_value = "auto", help_heading = "Browser")]
    pub browser: String,
    /// Override browser binary path.
    #[arg(long, help_heading = "Browser")]
    pub browser_bin: Option<PathBuf>,
    /// Bearer token required for clients on TCP listeners.
    #[arg(long = "token-secret", help_heading = "Listener")]
    pub token: Option<String>,
    /// Serve /health and /capabilities.
    #[arg(long, default_value = "on", help_heading = "Listener")]
    pub health: String,
    /// Make /health public with minimal payload.
    #[arg(long, default_value = "off", help_heading = "Listener")]
    pub health_public: String,
    /// Propagate an environment variable into the browser subprocess.
    /// Repeatable. The host scrubs all other ambient env (`HTTP_PROXY`,
    /// `XDG_*`, `BROWSER`, locale, etc.) so a browsing environment can
    /// never silently honor configuration the agent did not request.
    /// Use the form `K=V`.
    #[arg(long = "engine-env", value_name = "K=V", help_heading = "Browser")]
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
    #[arg(long, default_value_t = 0, help_heading = "Listener")]
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
    let takeover = parse_takeover(&args.takeover, args.display_provider.as_deref())?;
    let ops_enabled = !matches!(takeover, Takeover::Off);
    let display_explicit = args.display.is_some();
    let mut display = match args.display.as_deref().unwrap_or("headless") {
        "headless" => DisplayMode::Headless,
        "headful" => DisplayMode::Headful,
        other => {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                format!("--display: unknown mode {other:?}; expected headless|headful"),
            ));
        }
    };
    if matches!(takeover, Takeover::Display { .. }) {
        if display_explicit && matches!(display, DisplayMode::Headless) {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "--takeover display requires a headful browser; omit --display or pass --display headful",
            ));
        }
        display = DisplayMode::Headful;
    }
    let browser = args
        .browser
        .parse::<BrowserChoice>()
        .map_err(|e| Error::new(ErrorCode::InvalidArgument, format!("--browser: {e}")))?;
    let health_public = match args.health_public.as_str() {
        "off" => HealthPublic::Off,
        "minimal" => HealthPublic::Minimal,
        other => {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                format!("--health-public: unknown {other:?}; expected off|minimal"),
            ));
        }
    };
    let health_enabled = parse_on_off("--health", &args.health)?;
    let mut engine_envs = Vec::with_capacity(args.engine_envs.len());
    for raw in &args.engine_envs {
        engine_envs.push(parse_engine_env(raw)?);
    }
    if args.display_quality > 100 {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            format!(
                "--display-quality-percent: must be 0-100, got {}",
                args.display_quality
            ),
        ));
    }
    let host_args = HostArgs {
        listen: args.listen,
        profile,
        display,
        takeover,
        display_quality: args.display_quality,
        browser,
        browser_bin: args.browser_bin,
        token: args.token,
        ops_enabled,
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
            format!("--engine-env: expected K=V, got {raw:?}"),
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

fn parse_on_off(flag: &str, value: &str) -> Result<bool, Error> {
    match value {
        "on" => Ok(true),
        "off" => Ok(false),
        other => Err(Error::new(
            ErrorCode::InvalidArgument,
            format!("{flag}: expected on|off, got {other:?}"),
        )),
    }
}

fn parse_takeover(raw: &str, display_provider: Option<&str>) -> Result<Takeover, Error> {
    match raw {
        "none" => {
            reject_display_provider_without_display(display_provider)?;
            Ok(Takeover::Off)
        }
        "screencast" => {
            reject_display_provider_without_display(display_provider)?;
            Ok(Takeover::Screencast)
        }
        "display" => {
            let provider = match display_provider {
                Some(raw) => raw.parse::<DisplayProvider>().map_err(|e| {
                    Error::new(ErrorCode::InvalidArgument, format!("--display-provider: {e}"))
                })?,
                None => DisplayProvider::KasmVnc,
            };
            Ok(Takeover::Display { provider })
        }
        "kasmvnc" => Err(Error::new(
            ErrorCode::InvalidArgument,
            "--takeover: mode \"kasmvnc\" was removed; use `--takeover display --display-provider kasmvnc`",
        )),
        other => Err(Error::new(
            ErrorCode::InvalidArgument,
            format!("--takeover: unknown mode {other:?}; expected none|screencast|display"),
        )),
    }
}

fn reject_display_provider_without_display(display_provider: Option<&str>) -> Result<(), Error> {
    if display_provider.is_some() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "--display-provider is only valid with `--takeover display`",
        ));
    }
    Ok(())
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

    #[test]
    fn takeover_modes_parse_new_display_provider_contract() {
        assert_eq!(parse_takeover("none", None).unwrap(), Takeover::Off);
        assert_eq!(
            parse_takeover("screencast", None).unwrap(),
            Takeover::Screencast
        );
        assert_eq!(
            parse_takeover("display", None).unwrap(),
            Takeover::Display {
                provider: DisplayProvider::KasmVnc
            }
        );
        assert_eq!(
            parse_takeover("display", Some("kasmvnc")).unwrap(),
            Takeover::Display {
                provider: DisplayProvider::KasmVnc
            }
        );
    }

    #[test]
    fn takeover_modes_reject_legacy_and_misplaced_provider() {
        let legacy = parse_takeover("kasmvnc", None).err().unwrap();
        assert_eq!(legacy.error_code, ErrorCode::InvalidArgument);
        assert!(legacy
            .detail
            .contains("--takeover display --display-provider kasmvnc"));

        let misplaced = parse_takeover("screencast", Some("kasmvnc")).err().unwrap();
        assert_eq!(misplaced.error_code, ErrorCode::InvalidArgument);

        let unknown_provider = parse_takeover("display", Some("unknown")).err().unwrap();
        assert_eq!(unknown_provider.error_code, ErrorCode::InvalidArgument);
        assert!(unknown_provider.detail.contains("expected kasmvnc"));
    }
}
