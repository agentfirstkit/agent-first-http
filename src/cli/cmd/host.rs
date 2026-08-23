//! `afhttp host` subcommand. Builds [`HostArgs`] from the resolved invocation
//! and forwards to [`crate::host::bootstrap::run`].

use std::path::PathBuf;

use agent_first_data::ValueSource;

use crate::cli::token_source;
use crate::host::bootstrap::{
    BrowserChoice, DisplayMode, HealthPublic, HostArgs, ProfileChoice, Takeover,
};
use crate::host::listener::{ListenAddr, parse_listen};
use crate::shared::error::{Error, ErrorCode};

#[derive(Debug)]
pub struct Args {
    pub listen: String,
    pub profile: String,
    /// Only the no-takeover shape carries a display choice; a takeover host is
    /// headful by construction, so the registry does not accept one there.
    pub display: Option<DisplayMode>,
    pub takeover: Takeover,
    pub takeover_quality_percent: u8,
    pub browser: BrowserChoice,
    pub browser_bin: Option<PathBuf>,
    /// The token this host will require of its callers, still unread: a host
    /// that keeps its token in a config file should not have to put it on argv
    /// or in the environment to start.
    pub token: Option<ValueSource>,
    pub no_health: bool,
    pub health_public: HealthPublic,
    pub engine_envs: Vec<String>,
    pub browser_args: Vec<String>,
    pub proxy: Option<String>,
    pub recent_requests_cap: usize,
}

pub async fn run(args: Args) -> Result<(), Error> {
    let token = args.token.as_ref().map(token_source::read).transpose()?;
    enforce_listen_auth(
        &args.listen,
        token
            .as_ref()
            .map(agent_first_data::value_source::SecretString::expose_secret),
    )?;
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
    let takeover_enabled = !matches!(args.takeover, Takeover::Off);
    let display = match args.takeover {
        Takeover::On { .. } => DisplayMode::Headful,
        Takeover::Off => args.display.unwrap_or(DisplayMode::Headless),
    };
    let health_enabled: bool = !args.no_health;
    let mut engine_envs = Vec::with_capacity(args.engine_envs.len());
    for raw in &args.engine_envs {
        engine_envs.push(parse_engine_env(raw)?);
    }
    let host_args = HostArgs {
        listen: args.listen,
        profile,
        display,
        takeover: args.takeover,
        display_quality: args.takeover_quality_percent,
        browser: args.browser,
        browser_bin: args.browser_bin,
        token: token.map(|token| token.expose_secret().to_string()),
        takeover_enabled,
        health_enabled,
        health_public: args.health_public,
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
    if let ListenAddr::Tcp(addr) = parse_listen(listen)?
        && !addr.ip().is_loopback()
    {
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
    Ok(())
}

// The value half is an environment variable handed to a browser engine, which
// is exactly where a token or a proxy credential is passed. Same rule as
// `--header` and `--cookie`: say what shape was wrong, never what was in it.
pub(crate) fn parse_engine_env(raw: &str) -> Result<(String, String), Error> {
    let (k, v) = raw.split_once('=').ok_or_else(|| {
        Error::new(
            ErrorCode::InvalidArgument,
            "--engine-env: expected NAME=VALUE, with an equals sign separating them".to_string(),
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
