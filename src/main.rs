//! `afhttp` binary entrypoint. All logic lives in the `cli` module.

#[cfg(feature = "cli")]
fn main() -> std::process::ExitCode {
    agent_first_http::cli::run()
}

#[cfg(not(feature = "cli"))]
fn main() -> std::process::ExitCode {
    // Without the cli feature there is no protocol writer available;
    // emit a structured JSON error on stdout (the AFDATA channel) and exit.
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let _ = writeln!(
        handle,
        "{{\"code\":\"error\",\"error_code\":\"internal_error\",\"error\":\"afhttp built without the `cli` feature; rebuild with --features cli.\",\"retryable\":false}}"
    );
    std::process::ExitCode::from(2)
}
