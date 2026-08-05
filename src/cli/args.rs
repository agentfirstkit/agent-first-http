//! Turn one resolved `cli-spec-v1` invocation into the typed command the async
//! dispatcher runs.
//!
//! The registry in [`crate::cli::spec`] has already decided that the argv is
//! legal, which shape it matched, and what every value's type is. What is left
//! here is projection: reading those values, applying the two environment
//! fallbacks the registry cannot see (`AFHTTP_ENDPOINT_URL` and
//! `AFHTTP_TOKEN_SECRET`), and narrowing registry integers to the unsigned
//! ranges the domain types use.

use std::path::PathBuf;

use agent_first_data::{CliValue, ResolvedInvocation};

use crate::cli::cmd;
use crate::host::bootstrap::{
    BrowserChoice, DisplayMode, HealthPublic, Takeover, TakeoverProviderKind,
};
use crate::sdk::fetch::{NetworkBodies, RenderMode};
use crate::shared::error::{Error, ErrorCode};

const ENDPOINT_ENV: &str = "AFHTTP_ENDPOINT_URL";
const TOKEN_ENV: &str = "AFHTTP_TOKEN_SECRET";

/// One dispatchable command, already projected off its invocation.
pub enum Command {
    Fetch(Box<cmd::fetch::Args>),
    Host(cmd::host::Args),
    Upload(cmd::upload::Args),
    Cdp(cmd::cdp::Args),
    Panel(cmd::panel::Args),
    Health(cmd::health::Args),
    Capabilities(cmd::capabilities::Args),
    Profile(cmd::profile::Args),
    Tabs(cmd::tabs::Args),
    Skill(cmd::skill::Args),
    Container(cmd::container::Args),
}

/// What every action handler produces. Projection can still fail — an endpoint
/// that is neither on argv nor in the environment, an integer outside its
/// domain range — and those are argument errors, not run failures.
pub type Projected = Result<Command, Error>;

type Handler = fn(&ResolvedInvocation) -> Projected;

/// Exactly one handler per action id in the registry; `bind_actions` proves the
/// two sets match at startup.
pub fn handlers() -> [(&'static str, Handler); 23] {
    [
        ("fetch", fetch as Handler),
        ("host", host),
        ("upload", upload),
        ("cdp", cdp),
        ("panel", panel),
        ("health", health),
        ("capabilities", capabilities),
        ("profile_list", profile_list),
        ("profile_info", profile_info),
        ("profile_lock_status", profile_lock_status),
        ("profile_downloads", profile_downloads),
        ("profile_cookies", profile_cookies),
        ("profile_delete", profile_delete),
        ("profile_prune", profile_prune),
        ("tabs_list", tabs_list),
        ("tabs_close", tabs_close),
        ("skill_status", skill_status),
        ("skill_install", skill_install),
        ("skill_uninstall", skill_uninstall),
        ("container_install", container_install),
        ("container_uninstall", container_uninstall),
        ("container_status", container_status),
        ("container_logs", container_logs),
    ]
}

// ── value projection ────────────────────────────────────────────────────────

/// A value the matched shape declares as required or fixed.
///
/// Reading it cannot fail: the shape that matched supplies every id it
/// declares. Asking for an id it does not declare is a defect in this file, and
/// `call_every_combination` is where that surfaces, from a test.
fn string(invocation: &ResolvedInvocation, id: &str) -> String {
    invocation
        .required(id)
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn opt_string(invocation: &ResolvedInvocation, id: &str) -> Option<String> {
    invocation
        .optional(id)
        .and_then(CliValue::as_str)
        .map(str::to_string)
}

fn strings(invocation: &ResolvedInvocation, id: &str) -> Vec<String> {
    invocation
        .repeated(id)
        .iter()
        .filter_map(CliValue::as_str)
        .map(str::to_string)
        .collect()
}

fn flag(invocation: &ResolvedInvocation, id: &str) -> bool {
    invocation
        .optional(id)
        .and_then(CliValue::as_bool)
        .unwrap_or(false)
}

fn opt_path(invocation: &ResolvedInvocation, id: &str) -> Option<PathBuf> {
    opt_string(invocation, id).map(PathBuf::from)
}

/// Registry integers are `i64`. Every afhttp count is unsigned and bounded, and
/// that range is the one thing `cli-spec-v1` cannot type, so it is checked here
/// and reported as the argument error it is.
fn unsigned(invocation: &ResolvedInvocation, id: &str, name: &str, max: u64) -> Result<u64, Error> {
    let value = invocation
        .optional(id)
        .and_then(CliValue::as_i64)
        .unwrap_or(0);
    u64::try_from(value)
        .ok()
        .filter(|value| *value <= max)
        .ok_or_else(|| {
            Error::new(
                ErrorCode::InvalidArgument,
                format!("{name}: expected an integer from 0 to {max}, got {value}"),
            )
        })
}

/// An environment fallback the registry cannot express: a value is legal on
/// argv, in the environment, or — where the command can still run without one —
/// nowhere at all.
fn from_env(value: Option<String>, variable: &str) -> Option<String> {
    value.or_else(|| {
        std::env::var(variable)
            .ok()
            .filter(|value| !value.is_empty())
    })
}

fn endpoint(invocation: &ResolvedInvocation) -> Option<String> {
    from_env(opt_string(invocation, "endpoint_url"), ENDPOINT_ENV)
}

fn token(invocation: &ResolvedInvocation) -> Option<String> {
    from_env(opt_string(invocation, "token_secret"), TOKEN_ENV)
}

/// Commands that only talk to an already-running host cannot invent one.
fn required_endpoint(invocation: &ResolvedInvocation) -> Result<String, Error> {
    endpoint(invocation).ok_or_else(missing_endpoint_error)
}

fn missing_endpoint_error() -> Error {
    Error::new(
        ErrorCode::InvalidArgument,
        format!("this command needs a running host: pass --endpoint-url or set {ENDPOINT_ENV}"),
    )
}

fn browser(invocation: &ResolvedInvocation) -> BrowserChoice {
    opt_string(invocation, "browser")
        .and_then(|value| value.parse().ok())
        .unwrap_or_default()
}

fn render(invocation: &ResolvedInvocation) -> RenderMode {
    match opt_string(invocation, "render").as_deref() {
        Some("none") => RenderMode::None,
        Some("always") => RenderMode::Always,
        _ => RenderMode::Auto,
    }
}

fn network_bodies(invocation: &ResolvedInvocation) -> NetworkBodies {
    match opt_string(invocation, "network_bodies").as_deref() {
        Some("xhr") => NetworkBodies::Xhr,
        Some("all") => NetworkBodies::All,
        _ => NetworkBodies::Off,
    }
}

fn takeover_provider(invocation: &ResolvedInvocation) -> Takeover {
    match opt_string(invocation, "takeover_provider").as_deref() {
        Some("kasmvnc") => Takeover::On {
            provider: TakeoverProviderKind::KasmVnc,
        },
        _ => Takeover::Off,
    }
}

fn skill_target(invocation: &ResolvedInvocation) -> cmd::skill::TargetArgs {
    cmd::skill::TargetArgs {
        agent: opt_string(invocation, "agent").unwrap_or_else(|| "all".to_string()),
        scope: opt_string(invocation, "scope").unwrap_or_else(|| "personal".to_string()),
        skills_dir: opt_string(invocation, "skills_dir"),
    }
}

fn profile_info_args(invocation: &ResolvedInvocation) -> cmd::profile::InfoArgs {
    cmd::profile::InfoArgs {
        name: string(invocation, "name"),
        backend: opt_string(invocation, "backend"),
        profile_root: opt_path(invocation, "profile_root"),
    }
}

fn container_common(invocation: &ResolvedInvocation) -> Result<cmd::container::CommonArgs, Error> {
    Ok(cmd::container::CommonArgs {
        runtime: opt_string(invocation, "runtime")
            .map(|value| cmd::container::runtime_from_str(&value))
            .transpose()?,
        name: string(invocation, "name"),
    })
}

fn container_port(invocation: &ResolvedInvocation) -> Result<u16, Error> {
    let port = unsigned(invocation, "port", "--port", u64::from(u16::MAX))?;
    Ok(port as u16)
}

// ── actions ─────────────────────────────────────────────────────────────────

fn fetch(invocation: &ResolvedInvocation) -> Projected {
    Ok(Command::Fetch(Box::new(cmd::fetch::Args {
        url: string(invocation, "url"),
        endpoint: endpoint(invocation),
        token: token(invocation),
        browser: browser(invocation),
        browser_bin: opt_path(invocation, "browser_bin"),
        render: render(invocation),
        tab: opt_string(invocation, "tab").unwrap_or_else(|| "new".to_string()),
        takeover: flag(invocation, "takeover"),
        profile: opt_string(invocation, "profile"),
        wait: opt_string(invocation, "wait").unwrap_or_else(|| "auto".to_string()),
        headers: strings(invocation, "header"),
        cookies: strings(invocation, "cookie"),
        user_agent: opt_string(invocation, "user_agent"),
        evaluate_after_wait: strings(invocation, "evaluate_after_wait"),
        want: strings(invocation, "want"),
        method: opt_string(invocation, "method").unwrap_or_else(|| "GET".to_string()),
        data: opt_string(invocation, "data"),
        form: strings(invocation, "form"),
        network_bodies: network_bodies(invocation),
        network_body_max_bytes: unsigned(
            invocation,
            "network_body_max_bytes",
            "--network-body-max-bytes",
            u64::MAX,
        )?,
        readiness_idle_ms: unsigned(
            invocation,
            "readiness_idle_ms",
            "--readiness-idle-ms",
            u64::MAX,
        )?,
        readiness_stable_ms: unsigned(
            invocation,
            "readiness_stable_ms",
            "--readiness-stable-ms",
            u64::MAX,
        )?,
        readiness_min_text_bytes: unsigned(
            invocation,
            "readiness_min_text_bytes",
            "--readiness-min-text-bytes",
            u64::MAX,
        )?,
        no_network_redact: flag(invocation, "no_network_redact"),
        out: opt_path(invocation, "out"),
        cookie_jar: opt_path(invocation, "cookie_jar"),
        no_cookie_jar: flag(invocation, "no_cookie_jar"),
        observe_main_wait_ms: unsigned(
            invocation,
            "observe_main_wait_ms",
            "--observe-main-wait-ms",
            u64::MAX,
        )?,
        max_response_bytes: unsigned(
            invocation,
            "max_response_bytes",
            "--max-response-bytes",
            u64::MAX,
        )?,
        retry: unsigned(invocation, "retry", "--retry", u64::from(u32::MAX))? as u32,
        backoff_ms: unsigned(invocation, "backoff_ms", "--backoff-ms", u64::MAX)?,
        proxy: opt_string(invocation, "proxy_url"),
        ca_cert: opt_path(invocation, "ca_cert"),
        tls_insecure: flag(invocation, "tls_insecure"),
        timeout_ms: unsigned(invocation, "timeout_ms", "--timeout-ms", u64::MAX)?,
        capture_ws: flag(invocation, "capture_ws"),
        capture_sse: flag(invocation, "capture_sse"),
    })))
}

fn host(invocation: &ResolvedInvocation) -> Projected {
    Ok(Command::Host(cmd::host::Args {
        listen: string(invocation, "listen"),
        profile: opt_string(invocation, "profile").unwrap_or_else(|| "-".to_string()),
        display: match opt_string(invocation, "display").as_deref() {
            Some("headful") => Some(DisplayMode::Headful),
            Some("headless") => Some(DisplayMode::Headless),
            _ => None,
        },
        takeover: takeover_provider(invocation),
        takeover_quality_percent: unsigned(
            invocation,
            "takeover_quality_percent",
            "--takeover-quality-percent",
            100,
        )? as u8,
        browser: browser(invocation),
        browser_bin: opt_path(invocation, "browser_bin"),
        token: opt_string(invocation, "token_secret"),
        no_health: flag(invocation, "no_health"),
        health_public: match opt_string(invocation, "health_public").as_deref() {
            Some("minimal") => HealthPublic::Minimal,
            _ => HealthPublic::Off,
        },
        engine_envs: strings(invocation, "engine_env"),
        browser_args: strings(invocation, "browser_arg"),
        proxy: opt_string(invocation, "proxy_url"),
        recent_requests_cap: unsigned(
            invocation,
            "recent_requests_cap",
            "--recent-requests-cap",
            u64::try_from(usize::MAX).unwrap_or(u64::MAX),
        )? as usize,
    }))
}

fn upload(invocation: &ResolvedInvocation) -> Projected {
    Ok(Command::Upload(cmd::upload::Args {
        endpoint: required_endpoint(invocation)?,
        token: token(invocation),
        tab: string(invocation, "tab"),
        selector: string(invocation, "selector"),
        file: PathBuf::from(string(invocation, "file")),
    }))
}

fn cdp(invocation: &ResolvedInvocation) -> Projected {
    Ok(Command::Cdp(cmd::cdp::Args {
        method: string(invocation, "method"),
        endpoint: required_endpoint(invocation)?,
        token: token(invocation),
        tab: string(invocation, "tab"),
        params: opt_string(invocation, "params"),
        wait: opt_string(invocation, "wait_event"),
    }))
}

fn panel(invocation: &ResolvedInvocation) -> Projected {
    Ok(Command::Panel(cmd::panel::Args {
        endpoint: required_endpoint(invocation)?,
        token: token(invocation),
    }))
}

fn health(invocation: &ResolvedInvocation) -> Projected {
    Ok(Command::Health(cmd::health::Args {
        endpoint: required_endpoint(invocation)?,
        token: token(invocation),
    }))
}

fn capabilities(invocation: &ResolvedInvocation) -> Projected {
    Ok(Command::Capabilities(cmd::capabilities::Args {
        endpoint: required_endpoint(invocation)?,
        token: token(invocation),
    }))
}

fn profile_command(sub: cmd::profile::ProfileSub) -> Projected {
    Ok(Command::Profile(cmd::profile::Args { sub }))
}

fn profile_list(invocation: &ResolvedInvocation) -> Projected {
    profile_command(cmd::profile::ProfileSub::List(cmd::profile::ListArgs {
        profile_root: opt_path(invocation, "profile_root"),
    }))
}

fn profile_info(invocation: &ResolvedInvocation) -> Projected {
    profile_command(cmd::profile::ProfileSub::Info(profile_info_args(
        invocation,
    )))
}

fn profile_lock_status(invocation: &ResolvedInvocation) -> Projected {
    profile_command(cmd::profile::ProfileSub::LockStatus(profile_info_args(
        invocation,
    )))
}

fn profile_downloads(invocation: &ResolvedInvocation) -> Projected {
    profile_command(cmd::profile::ProfileSub::Downloads(profile_info_args(
        invocation,
    )))
}

fn profile_cookies(invocation: &ResolvedInvocation) -> Projected {
    profile_command(cmd::profile::ProfileSub::Cookies(profile_info_args(
        invocation,
    )))
}

fn profile_delete(invocation: &ResolvedInvocation) -> Projected {
    profile_command(cmd::profile::ProfileSub::Delete(cmd::profile::DeleteArgs {
        name: string(invocation, "name"),
        backend: opt_string(invocation, "backend"),
        confirm: string(invocation, "confirm"),
        profile_root: opt_path(invocation, "profile_root"),
    }))
}

fn profile_prune(invocation: &ResolvedInvocation) -> Projected {
    profile_command(cmd::profile::ProfileSub::Prune(cmd::profile::PruneArgs {
        older_than: string(invocation, "older_than"),
        dry_run: flag(invocation, "dry_run"),
        profile_root: opt_path(invocation, "profile_root"),
    }))
}

fn tabs_list(invocation: &ResolvedInvocation) -> Projected {
    Ok(Command::Tabs(cmd::tabs::Args {
        sub: cmd::tabs::TabsSub::List(cmd::tabs::EndpointArgs {
            endpoint: required_endpoint(invocation)?,
            token: token(invocation),
        }),
    }))
}

fn tabs_close(invocation: &ResolvedInvocation) -> Projected {
    Ok(Command::Tabs(cmd::tabs::Args {
        sub: cmd::tabs::TabsSub::Close(cmd::tabs::CloseArgs {
            tab: string(invocation, "tab"),
            endpoint: required_endpoint(invocation)?,
            token: token(invocation),
        }),
    }))
}

fn skill_status(invocation: &ResolvedInvocation) -> Projected {
    Ok(Command::Skill(cmd::skill::Args {
        sub: cmd::skill::SkillSub::Status(skill_target(invocation)),
    }))
}

fn skill_install(invocation: &ResolvedInvocation) -> Projected {
    Ok(Command::Skill(cmd::skill::Args {
        sub: cmd::skill::SkillSub::Install(cmd::skill::WriteArgs {
            target: skill_target(invocation),
            force: flag(invocation, "force"),
        }),
    }))
}

fn skill_uninstall(invocation: &ResolvedInvocation) -> Projected {
    Ok(Command::Skill(cmd::skill::Args {
        sub: cmd::skill::SkillSub::Uninstall(cmd::skill::WriteArgs {
            target: skill_target(invocation),
            force: flag(invocation, "force"),
        }),
    }))
}

fn container_install(invocation: &ResolvedInvocation) -> Projected {
    Ok(Command::Container(cmd::container::Args {
        sub: cmd::container::ContainerSub::Install(cmd::container::InstallArgs {
            common: container_common(invocation)?,
            port: container_port(invocation)?,
            profile: opt_string(invocation, "profile"),
            shm_size: opt_string(invocation, "shm_size"),
            takeover_provider: takeover_provider(invocation),
            with: strings(invocation, "with"),
            rebuild: flag(invocation, "rebuild"),
            from_source: flag(invocation, "from_source"),
            context: opt_string(invocation, "context"),
            host_args: strings(invocation, "host_args"),
            reveal_token_secret: flag(invocation, "reveal_token_secret"),
        }),
    }))
}

fn container_uninstall(invocation: &ResolvedInvocation) -> Projected {
    Ok(Command::Container(cmd::container::Args {
        sub: cmd::container::ContainerSub::Uninstall(cmd::container::UninstallArgs {
            common: container_common(invocation)?,
            purge: flag(invocation, "purge"),
        }),
    }))
}

fn container_status(invocation: &ResolvedInvocation) -> Projected {
    Ok(Command::Container(cmd::container::Args {
        sub: cmd::container::ContainerSub::Status(cmd::container::StatusArgs {
            common: container_common(invocation)?,
            port: container_port(invocation)?,
            reveal_token_secret: flag(invocation, "reveal_token_secret"),
        }),
    }))
}

fn container_logs(invocation: &ResolvedInvocation) -> Projected {
    Ok(Command::Container(cmd::container::Args {
        sub: cmd::container::ContainerSub::Logs(cmd::container::LogsArgs {
            common: container_common(invocation)?,
            follow: flag(invocation, "follow"),
            raw: flag(invocation, "raw"),
        }),
    }))
}

#[cfg(test)]
mod tests {
    use agent_first_data::BoundOutcome;

    use super::*;
    use crate::cli::spec::cli_spec;

    fn project(argv: &[&str]) -> Projected {
        let cli = match cli_spec() {
            Ok(cli) => cli,
            Err(error) => panic!("registry must build: {error}"),
        };
        let app = match cli.bind_actions(handlers()) {
            Ok(app) => app,
            Err(error) => panic!("handlers must cover every action: {error}"),
        };
        match app.resolve_from(argv.to_vec()) {
            Ok(BoundOutcome::Run(invocation)) => invocation.run(),
            Ok(_) => panic!("{argv:?} did not resolve to a run"),
            Err(error) => panic!("{argv:?} failed to resolve: {}", error.message),
        }
    }

    #[test]
    fn every_action_has_exactly_one_handler() {
        let cli = cli_spec().expect("registry must build");
        cli.bind_actions(handlers())
            .expect("handlers must cover every action exactly once");
    }

    /// Every declared shape, run through its own handler with strict reads: a
    /// handler that asks for an argument id its shape cannot supply names
    /// itself here rather than degrading to an empty string in production.
    ///
    /// Safe to run because these handlers only project — nothing here opens a
    /// socket, launches a host, or touches a profile directory.
    #[test]
    fn every_combination_reads_only_ids_its_shape_declares() {
        let cli = cli_spec().expect("registry must build");
        let app = cli
            .bind_actions(handlers())
            .expect("handlers must cover every action exactly once");
        app.call_every_combination();
    }

    #[test]
    fn an_environment_fallback_never_overrides_argv() {
        const ABSENT: &str = "AFHTTP_TEST_ABSENT_ENDPOINT_URL";
        assert_eq!(from_env(None, ABSENT), None);
        assert_eq!(
            from_env(Some("ws://argv".to_string()), ABSENT),
            Some("ws://argv".to_string())
        );
    }

    #[test]
    fn a_missing_endpoint_names_both_ways_to_supply_one() {
        let error = missing_endpoint_error();
        assert_eq!(error.error_code, ErrorCode::InvalidArgument);
        assert!(error.detail.contains("--endpoint-url"), "{}", error.detail);
        assert!(error.detail.contains(ENDPOINT_ENV), "{}", error.detail);
    }

    #[test]
    fn an_explicit_endpoint_reaches_the_command() {
        let projected = project(&["afhttp", "health", "--endpoint-url", "ws://127.0.0.1:9222"]);
        let Ok(Command::Health(args)) = projected else {
            panic!("health must project to a health command");
        };
        assert_eq!(args.endpoint, "ws://127.0.0.1:9222");
    }

    #[test]
    fn quality_percent_is_bounded_where_the_registry_cannot_type_it() {
        let error = project(&[
            "afhttp",
            "host",
            "--listen",
            "tcp:127.0.0.1:9222",
            "--takeover-provider",
            "kasmvnc",
            "--takeover-quality-percent",
            "101",
        ])
        .err()
        .expect("out-of-range quality");
        assert_eq!(error.error_code, ErrorCode::InvalidArgument);
    }
}
