//! CLI layer. Parses arguments, calls into the SDK, formats output.

pub mod args;
pub mod cmd;
pub mod output;

use std::process::ExitCode;

use crate::shared::error::Error;

/// Binary entry point. Always returns `ExitCode::SUCCESS` on a structured
/// response (success *or* error); the response itself carries the
/// success/failure shape on stdout.
pub fn run() -> ExitCode {
    // Install the process-wide rustls crypto provider before anything builds a
    // reqwest/TLS client. On the inline-fetch path the host-side CDP fetch runs
    // before the SDK client's own guard, so without this `afhttp fetch` panics
    // with "no rustls crypto provider is configured".
    crate::host::bootstrap::install_rustls_provider();

    // Help is rendered without spinning up the async runtime. `--help-markdown`
    // feeds scripts/projects/agent-first-http/generate-cli-doc.sh; top-level
    // `--help` mirrors afpsql's recursive afdata-rendered help. Subcommand help
    // (e.g. `afhttp fetch --help`) is left to clap.
    if let Some(code) = maybe_render_help() {
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
    let exit = rt.block_on(async {
        match args::parse() {
            Ok(parsed) => dispatch(parsed).await,
            Err(err) => {
                emit_cli_error(&err);
                Err(err)
            }
        }
    });
    match exit {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(1),
    }
}

/// Render top-level help and return an exit code, or `None` to continue normal
/// parsing. afhttp has no top-level global flags, so detection is a simple scan.
fn maybe_render_help() -> Option<ExitCode> {
    use clap::CommandFactory;
    use std::io::Write;

    let raw: Vec<String> = std::env::args().collect();
    let mut handle = std::io::stdout();

    // `afhttp --help` / `-h` only (let clap handle `afhttp <sub> --help`).
    if raw.len() == 2 && matches!(raw[1].as_str(), "--help" | "-h") {
        let _ = writeln!(
            handle,
            "{}",
            agent_first_data::cli_render_help(&args::Cli::command(), &[])
        );
        return Some(ExitCode::SUCCESS);
    }

    // `afhttp --help-markdown` anywhere before a `--` terminator.
    let wants_markdown = raw
        .iter()
        .skip(1)
        .take_while(|a| a.as_str() != "--")
        .any(|a| a == "--help-markdown");
    if wants_markdown {
        let _ = writeln!(
            handle,
            "{}",
            agent_first_data::cli_render_help_markdown(&args::Cli::command(), &[])
        );
        return Some(ExitCode::SUCCESS);
    }

    None
}

async fn dispatch(parsed: args::Parsed) -> Result<(), Error> {
    let res = match parsed.command {
        args::Command::Host(a) => cmd::host::run(a).await,
        args::Command::Fetch(a) => cmd::fetch::run(*a).await,
        args::Command::Upload(a) => cmd::upload::run(a).await,
        args::Command::Cdp(a) => cmd::cdp::run(a).await,
        args::Command::Ui(a) => cmd::ui::run(a).await,
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
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let _ = crate::shared::envelope::emit_error(&mut handle, err);
}

fn emit_bootstrap_error(msg: &str) {
    // Fallback path used before the runtime exists. Stays on stdout to
    // match the AFDATA protocol channel rule (clippy bans stderr usage).
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let _ = writeln!(
        handle,
        "{{\"code\":\"error\",\"error_code\":\"internal_error\",\"error\":{},\"retryable\":false}}",
        serde_json::Value::String(msg.to_string())
    );
}
