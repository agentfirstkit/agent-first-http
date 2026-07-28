//! CLI layer. Parses arguments, calls into the SDK, formats output.

pub mod args;
pub mod cmd;
pub mod output;

use std::process::ExitCode;

use crate::shared::error::Error;

/// Binary entry point. Successful structured responses exit zero; structured
/// errors carry their stable AFDATA shape on the selected output route and
/// exit nonzero.
pub fn run() -> ExitCode {
    if let Err(err) = crate::shared::afdata::install_output_to(std::env::args()) {
        let _ = crate::shared::afdata::emit_process_error(&err);
        return ExitCode::from(2);
    }

    let _stream_redirect =
        match agent_first_data::stream_redirect::install_from_raw_args(std::env::args()) {
            Ok(redirect) => redirect,
            Err(err) => {
                emit_bootstrap_error(&err.to_string());
                return ExitCode::from(2);
            }
        };

    // Install the process-wide rustls crypto provider before anything builds a
    // reqwest/TLS client. On the inline-fetch path the host-side CDP fetch runs
    // before the SDK client's own guard, so without this `afhttp fetch` panics
    // with "no rustls crypto provider is configured".
    crate::host::bootstrap::install_rustls_provider();

    // Version and progressively scoped help are rendered without spinning up
    // the async runtime, and share one machine-readable discovery path.
    if let Some(code) = maybe_render_version_or_help() {
        return code;
    }
    // The fetch/host pipeline polls a deeply nested future chain (inline host
    // launch → CDP handshake → …). Polling that depth builds a deep synchronous
    // call stack that overflows Windows' default 1 MiB main-thread stack
    // (Linux/macOS default to 8 MiB). Run the runtime on a thread with a generous
    // stack so behavior is uniform across platforms.
    match std::thread::Builder::new()
        .name("afhttp-main".to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn(run_blocking)
    {
        Ok(handle) => match handle.join() {
            Ok(code) => code,
            Err(_) => {
                emit_bootstrap_error("afhttp worker thread panicked");
                ExitCode::from(2)
            }
        },
        Err(e) => {
            emit_bootstrap_error(&format!("spawn worker thread: {e}"));
            ExitCode::from(2)
        }
    }
}

/// Build the tokio runtime and drive the dispatched command to completion.
/// Runs on a dedicated large-stack thread spawned by `run`.
fn run_blocking() -> ExitCode {
    let parsed = match args::parse() {
        Ok(parsed) => parsed,
        Err(err) => {
            emit_cli_error(&err);
            return ExitCode::from(2);
        }
    };
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(16 * 1024 * 1024)
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            emit_bootstrap_error(&format!("tokio runtime: {e}"));
            return ExitCode::from(2);
        }
    };
    let exit = rt.block_on(dispatch(parsed));
    match exit {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(1),
    }
}

/// Render version/help and return an exit code, or `None` to continue parsing.
fn maybe_render_version_or_help() -> Option<ExitCode> {
    use clap::CommandFactory;

    let raw: Vec<String> = std::env::args().collect();
    let build = match env!("GIT_SHA") {
        "unknown" => None,
        sha => Some(sha),
    };
    match agent_first_data::cli_handle_version_or_help_or_continue(
        &raw,
        &args::Cli::command(),
        &help_config(),
        "afhttp",
        Some(env!("DISPLAY_NAME")),
        env!("CARGO_PKG_VERSION"),
        build,
    ) {
        Ok(Some(output)) => match crate::shared::afdata::write_process_result(&output) {
            Ok(()) => Some(ExitCode::SUCCESS),
            Err(_) => Some(ExitCode::from(4)),
        },
        Ok(None) => None,
        Err(err) => {
            let err = Error::new(
                crate::shared::error::ErrorCode::InvalidArgument,
                err.to_string(),
            );
            let _ = crate::shared::afdata::emit_process_error(&err);
            Some(ExitCode::from(2))
        }
    }
}

fn help_config() -> agent_first_data::HelpConfig {
    // afhttp intentionally has no business `--output` flag: every command
    // emits protocol JSON. Help therefore uses JSON as its caller fallback,
    // while an explicit help-only `--output plain|yaml|markdown` still wins.
    agent_first_data::HelpConfig::output_aware_with_fallback(agent_first_data::HelpFormat::Json)
}

async fn dispatch(parsed: args::Parsed) -> Result<(), Error> {
    let command = match parsed.command {
        args::Command::Fetch(a) => {
            // `fetch` owns error emission so it can attach the fetch-local trace
            // without adding trace fields to the global Error type.
            return cmd::fetch::run(*a).await;
        }
        command => command,
    };
    let res = match command {
        args::Command::Host(a) => cmd::host::run(a).await,
        args::Command::Fetch(_) => unreachable!("fetch handled above"),
        args::Command::Upload(a) => cmd::upload::run(a).await,
        args::Command::Cdp(a) => cmd::cdp::run(a).await,
        args::Command::Panel(a) => cmd::panel::run(a).await,
        args::Command::Health(a) => cmd::health::run(a).await,
        args::Command::Capabilities(a) => cmd::capabilities::run(a).await,
        args::Command::Profile(a) => cmd::profile::run(a).await,
        args::Command::Tabs(a) => cmd::tabs::run(a).await,
        args::Command::Skill(a) => cmd::skill::run(a).await,
        args::Command::Container(a) => cmd::container::run(a).await,
    };
    if let Err(ref e) = res {
        emit_cli_error(e);
    }
    res
}

fn emit_cli_error(err: &Error) {
    let _ = crate::shared::afdata::emit_process_error(err);
}

fn emit_bootstrap_error(msg: &str) {
    let err = Error::new(crate::shared::error::ErrorCode::InternalError, msg);
    let _ = crate::shared::afdata::emit_process_error(&err);
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;
    use serde_json::Value;

    use super::*;

    fn render_discovery(raw: &[&str]) -> String {
        let raw = raw.iter().map(ToString::to_string).collect::<Vec<_>>();
        agent_first_data::cli_handle_version_or_help_or_continue(
            &raw,
            &args::Cli::command(),
            &help_config(),
            "afhttp",
            Some(env!("DISPLAY_NAME")),
            env!("CARGO_PKG_VERSION"),
            None,
        )
        .expect("valid discovery request")
        .expect("discovery request should render")
    }

    fn count_help_surface(command: &Value) -> (usize, usize) {
        let mut commands = 1;
        let mut arguments = command["arguments"].as_array().map_or(0, Vec::len);
        if let Some(subcommands) = command["subcommands"].as_array() {
            for subcommand in subcommands {
                let (subcommand_count, argument_count) = count_help_surface(subcommand);
                commands += subcommand_count;
                arguments += argument_count;
            }
        }
        (commands, arguments)
    }

    #[test]
    fn bare_help_uses_fixed_json_contract() {
        let rendered = render_discovery(&["afhttp", "--help"]);
        let event: Value = serde_json::from_str(&rendered).expect("bare help must be JSON");
        let help = &event["result"]["help"];
        assert_eq!(event["kind"], "result");
        assert_eq!(event["result"]["code"], "help");
        assert_eq!(help["scope"], "one_level");
        assert_eq!(help["command_path"], "afhttp");
        assert!(
            help["arguments"]
                .as_array()
                .expect("root arguments")
                .iter()
                .filter(|argument| {
                    matches!(
                        argument["name"].as_str(),
                        Some("--stdout-file" | "--stderr-file")
                    )
                })
                .all(|argument| argument["global"] == true),
            "stream redirect arguments must be marked global: {help}"
        );
        assert!(
            help["subcommands"]
                .as_array()
                .expect("root subcommands")
                .iter()
                .all(|command| command["name"] != "help"),
            "the clap help pseudo-command must not be advertised: {help}"
        );
    }

    #[test]
    fn fetch_help_is_scoped_and_keeps_details_progressive() {
        let rendered = render_discovery(&["afhttp", "fetch", "--help"]);
        let event: Value = serde_json::from_str(&rendered).expect("scoped help must be JSON");
        let help = &event["result"]["help"];
        assert_eq!(help["command_path"], "afhttp fetch");
        assert_eq!(
            help["inherited_arguments_from"],
            serde_json::json!(["afhttp"])
        );
        assert!(
            help["arguments"]
                .as_array()
                .expect("fetch arguments")
                .iter()
                .all(|argument| {
                    !matches!(
                        argument["name"].as_str(),
                        Some("--stdout-file" | "--stderr-file")
                    )
                }),
            "scoped structured help must not repeat inherited globals: {help}"
        );
        let takeover_help = help["arguments"]
            .as_array()
            .expect("fetch arguments")
            .iter()
            .find(|argument| argument["name"] == "--takeover")
            .and_then(|argument| argument["help"].as_str())
            .expect("--takeover help");
        assert_eq!(
            takeover_help,
            "Escalate captcha, login, or 2FA walls to human takeover"
        );
        assert!(
            !rendered.contains("next_action"),
            "compact structured help eagerly exposed long-form detail"
        );

        let plain = render_discovery(&["afhttp", "fetch", "--help", "--output", "plain"]);
        assert!(plain.contains("Usage: afhttp fetch"));
        assert!(plain.contains("--stdout-file"));

        let markdown = render_discovery(&["afhttp", "fetch", "--help", "--output", "markdown"]);
        assert!(
            markdown.contains("next_action"),
            "Markdown export must retain the long-form takeover detail"
        );
    }

    #[test]
    fn recursive_help_stays_within_structural_budget() {
        let rendered = render_discovery(&["afhttp", "--help", "--recursive", "--output", "json"]);
        let event: Value = serde_json::from_str(&rendered).expect("recursive help must be JSON");
        let help = &event["result"]["help"];
        let (commands, arguments) = count_help_surface(help);
        let budget = 512 + commands * 160 + arguments * 120;
        assert!(
            rendered.len() < budget,
            "recursive help exceeded its payload budget: {} >= {budget}",
            rendered.len()
        );
    }
}
