//! `afhttp` binary entrypoint. All logic lives in the `cli` module.

#[cfg(feature = "cli")]
fn main() -> std::process::ExitCode {
    agent_first_http::cli::run()
}

#[cfg(not(feature = "cli"))]
fn main() -> std::process::ExitCode {
    // Without the cli feature there is no protocol writer available.
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let _ = writeln!(
        handle,
        "{{\"kind\":\"error\",\"error\":{{\"code\":\"internal_error\",\"message\":\"afhttp built without the `cli` feature; rebuild with --features cli.\",\"retryable\":false}},\"trace\":{{}}}}"
    );
    std::process::ExitCode::from(2)
}
