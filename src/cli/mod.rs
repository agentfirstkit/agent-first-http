//! CLI layer. Resolves argv against the closed-world registry, calls into the
//! SDK, and formats output.

pub mod args;
pub mod cmd;
pub mod output;
pub mod spec;

use std::process::ExitCode;

use agent_first_data::{
    BoundOutcome, OutputPlan, OutputTo, cli_error_event, cli_help_event, cli_parse_output,
    cli_version_event, render_cli_reference,
};

use crate::shared::error::{Error, ErrorCode};

/// Binary entry point. Successful structured responses exit zero; structured
/// errors carry their stable AFDATA shape on the selected output route and
/// exit nonzero.
pub fn run() -> ExitCode {
    let cli = match spec::cli_spec() {
        Ok(cli) => cli,
        Err(error) => return emit_startup_error("cli_spec_invalid", &error.to_string()),
    };
    let app = match cli.bind_actions(args::handlers()) {
        Ok(app) => app,
        Err(error) => return emit_startup_error("cli_actions_invalid", &error.to_string()),
    };

    // Rejected before anything ran: the error names its own rule in
    // `error.code`, and always lands on the diagnostic stream. No output plan
    // exists yet, so this is the one path that cannot honor `--stdout-file`.
    let outcome = match app.resolve_from(std::env::args_os()) {
        Ok(outcome) => outcome,
        Err(error) => {
            let code = error.exit_code();
            return emit_lifecycle_event(
                cli_error_event(&error),
                agent_first_data::OutputFormat::Json,
                OutputTo::Stderr,
                code,
            );
        }
    };

    match outcome {
        BoundOutcome::Run(invocation) => {
            // The plan is readable before the handler runs, so output routing is
            // established before anything can be written.
            let _redirect = match install_redirect(invocation.output_plan()) {
                Ok(redirect) => redirect,
                Err(code) => return code,
            };
            if let Err(code) = install_route(invocation.output_plan()) {
                return code;
            }
            let command = match invocation.run() {
                Ok(command) => command,
                Err(error) => {
                    let _ = crate::shared::afdata::emit_process_error(&error);
                    return ExitCode::from(2);
                }
            };
            run_command(command)
        }
        // `--docs` renders the whole registry as raw Markdown, so it carries no
        // format of its own and never becomes a protocol event.
        BoundOutcome::Docs(docs) => {
            let _redirect = match install_redirect(docs.output_plan()) {
                Ok(redirect) => redirect,
                Err(code) => return code,
            };
            write_text(&render_cli_reference(&cli), stream_of(docs.output_plan()))
        }
        BoundOutcome::Help(help) => {
            let _redirect = match install_redirect(help.output_plan()) {
                Ok(redirect) => redirect,
                Err(code) => return code,
            };
            let format = plan_format(help.output_plan());
            if format == agent_first_data::OutputFormat::Plain {
                write_text(&help.plain(), stream_of(help.output_plan()))
            } else {
                emit_lifecycle_event(
                    cli_help_event(&help),
                    format,
                    route_of(help.output_plan()),
                    0,
                )
            }
        }
        BoundOutcome::Version(version) => {
            let _redirect = match install_redirect(version.output_plan()) {
                Ok(redirect) => redirect,
                Err(code) => return code,
            };
            emit_lifecycle_event(
                cli_version_event(&version),
                plan_format(version.output_plan()),
                route_of(version.output_plan()),
                0,
            )
        }
    }
}

/// Build the tokio runtime and drive the dispatched command to completion.
fn run_command(command: args::Command) -> ExitCode {
    // Install the process-wide rustls crypto provider before anything builds a
    // reqwest/TLS client. On the inline-fetch path the host-side CDP fetch runs
    // before the SDK client's own guard, so without this `afhttp fetch` panics
    // with "no rustls crypto provider is configured".
    crate::host::bootstrap::install_rustls_provider();

    // The fetch/host pipeline polls a deeply nested future chain (inline host
    // launch → CDP handshake → …). Polling that depth builds a deep synchronous
    // call stack that overflows Windows' default 1 MiB main-thread stack
    // (Linux/macOS default to 8 MiB). Run the runtime on a thread with a generous
    // stack so behavior is uniform across platforms.
    match std::thread::Builder::new()
        .name("afhttp-main".to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || run_blocking(command))
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

fn run_blocking(command: args::Command) -> ExitCode {
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
    match rt.block_on(dispatch(command)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(1),
    }
}

async fn dispatch(command: args::Command) -> Result<(), Error> {
    let command = match command {
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
        let _ = crate::shared::afdata::emit_process_error(e);
    }
    res
}

/// Send this run's process streams to the files the plan names, for as long as
/// the returned guard lives.
fn install_redirect(
    plan: &OutputPlan,
) -> Result<Option<agent_first_data::stream_redirect::InstalledStreamRedirect>, ExitCode> {
    let config = agent_first_data::stream_redirect::StreamRedirectConfig::new(
        plan.stdout_file().map(std::path::Path::to_path_buf),
        plan.stderr_file().map(std::path::Path::to_path_buf),
    )
    .map_err(|error| emit_startup_error("output_setup_failed", &error.to_string()))?;
    config
        .as_ref()
        .map(agent_first_data::stream_redirect::install)
        .transpose()
        .map_err(|error| emit_startup_error("output_setup_failed", &error.to_string()))
}

/// Record where protocol events go for the rest of the process.
fn install_route(plan: &OutputPlan) -> Result<(), ExitCode> {
    crate::shared::afdata::install_output_to(route_of(plan)).map_err(|error| {
        let _ = crate::shared::afdata::emit_process_error(&error);
        ExitCode::from(2)
    })
}

fn route_of(plan: &OutputPlan) -> OutputTo {
    plan.destination()
        .and_then(|destination| OutputTo::parse(destination).ok())
        .unwrap_or(OutputTo::Split)
}

fn plan_format(plan: &OutputPlan) -> agent_first_data::OutputFormat {
    plan.format()
        .and_then(|format| cli_parse_output(format).ok())
        .unwrap_or(agent_first_data::OutputFormat::Json)
}

fn stream_of(plan: &OutputPlan) -> OutputTo {
    if plan.destination() == Some("stderr") {
        OutputTo::Stderr
    } else {
        OutputTo::Stdout
    }
}

/// Emit one lifecycle event (`--help`, `--version`, or a rejected argv) before
/// the process-wide route exists.
fn emit_lifecycle_event(
    event: agent_first_data::Event,
    format: agent_first_data::OutputFormat,
    output_to: OutputTo,
    exit_code: u8,
) -> ExitCode {
    let mut emitter =
        agent_first_data::CliEmitter::from_output_to(output_to, format).with_strict_protocol();
    match emitter.emit(event) {
        Ok(()) => ExitCode::from(exit_code),
        Err(_) => ExitCode::from(4),
    }
}

// AFDATA injects the raw outcomes this writes (`--docs`, plain help), so it owns
// the routing and the rule that a closed reader is success rather than failure.
fn write_text(text: &str, output_to: OutputTo) -> ExitCode {
    match agent_first_data::write_raw(text, output_to) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(4),
    }
}

/// A registry that fails to build, or a route that cannot be installed, is a
/// programming or environment fault rather than a caller mistake, so it reports
/// before any output contract has been resolved.
fn emit_startup_error(code: &str, message: &str) -> ExitCode {
    let event = match agent_first_data::json_error(code, message).build() {
        Ok(event) => event,
        Err(_) => return ExitCode::from(4),
    };
    emit_lifecycle_event(
        event,
        agent_first_data::OutputFormat::Json,
        OutputTo::Stderr,
        1,
    )
}

fn emit_bootstrap_error(msg: &str) {
    let err = Error::new(ErrorCode::InternalError, msg);
    let _ = crate::shared::afdata::emit_process_error(&err);
}

#[cfg(test)]
mod tests {
    use agent_first_data::CliOutcome;
    use serde_json::Value;

    use super::*;

    fn help(argv: &[&str]) -> Value {
        // The unbound registry, which resolves to `CliOutcome` — help needs no
        // handler, so there is nothing to bind.
        let cli = spec::cli_spec().expect("registry must build");
        let CliOutcome::Help(help) = cli.resolve_from(argv.to_vec()).expect("help resolves") else {
            panic!("{argv:?} did not resolve to help");
        };
        serde_json::to_value(cli_help_event(&help).as_value()).expect("help serializes")
    }

    #[test]
    fn root_help_indexes_the_commands_as_ready_to_run_calls() {
        let event = help(&["afhttp", "--help"]);
        assert_eq!(event["kind"], "result");
        let model = &event["result"]["help"];
        assert_eq!(model["schema"], "cli-help-v2");
        assert_eq!(model["command_path"], "afhttp");
        // The root has no shapes of its own; what it owes a caller is the list
        // of commands, each already a runnable next call.
        let subcommands: Vec<&str> = model["subcommands"]
            .as_array()
            .expect("subcommands")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(subcommands.contains(&"afhttp fetch --help"), "{model}");
        assert!(subcommands.contains(&"afhttp container --help"), "{model}");
    }

    #[test]
    fn fetch_help_returns_every_shape_complete_in_one_call() {
        let event = help(&["afhttp", "fetch", "--help"]);
        let model = &event["result"]["help"];
        assert_eq!(model["command_path"], "afhttp fetch");
        let shapes = model["shapes"].as_array().expect("shapes");
        let ids: Vec<&str> = shapes
            .iter()
            .filter_map(|shape| shape["id"].as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "fetch",
                "fetch-data",
                "fetch-form",
                "fetch-takeover",
                "fetch-takeover-data",
                "fetch-takeover-form",
            ]
        );
        for shape in shapes {
            let usage = shape["usage"].as_str().expect("usage");
            assert!(usage.starts_with("afhttp fetch <URL>"), "{usage}");
            // Optional arguments are in the answer, not behind a second call.
            assert!(usage.contains("[--out <DIR>]"), "{usage}");
            assert!(
                shape["about"]
                    .as_str()
                    .is_some_and(|about| !about.is_empty()),
                "every shape of a multi-shape command says how it differs: {shape}"
            );
        }
        // The takeover shapes advertise only the render modes that reach a
        // browser, which is the constraint that used to be a runtime error.
        let takeover = shapes
            .iter()
            .find(|shape| shape["id"] == "fetch-takeover")
            .expect("takeover shape");
        let usage = takeover["usage"].as_str().unwrap_or_default();
        assert!(usage.contains("[--render <auto|always>]"), "{usage}");
        assert!(usage.contains("--takeover"), "{usage}");

        assert_eq!(model["defaults"]["--render"], "auto");
        assert!(model["notes"]["--takeover"].as_str().is_some(), "{model}");
    }
}
