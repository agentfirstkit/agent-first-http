//! Runtime process execution and bounded build-log reads.
use crate::shared::error::{Error, ErrorCode};
use std::path::Path;
use std::process::Command;

fn spawn_error(bin: &str, err: &std::io::Error) -> Error {
    if err.kind() == std::io::ErrorKind::NotFound {
        Error::new(
            ErrorCode::InvalidArgument,
            format!("container runtime `{bin}` not found on PATH"),
        )
    } else {
        Error::new(
            ErrorCode::IoError,
            format!("spawning `{bin}` failed: {err}"),
        )
    }
}

/// Flags whose value can carry a credential: the host's own `--engine-env`
/// and proxy flags (a proxy URL routinely has `user:pw@`), its token source,
/// and the runtime's environment flags.
const VALUE_HIDDEN_FLAGS: &[&str] = &[
    "--engine-env",
    "--token-secret",
    "--proxy",
    "--proxy-url",
    "-e",
    "--env",
];

/// The command line as it may be written to a log or an error: the value of
/// every flag in [`VALUE_HIDDEN_FLAGS`] is replaced, in both the `--flag value`
/// and `--flag=value` spellings.
fn display_args(args: &[String]) -> String {
    let mut shown = Vec::with_capacity(args.len());
    let mut hide_next = false;
    for arg in args {
        if hide_next {
            shown.push("[redacted]".to_string());
            hide_next = false;
            continue;
        }
        match arg.split_once('=') {
            Some((flag, _)) if VALUE_HIDDEN_FLAGS.contains(&flag) => {
                shown.push(format!("{flag}=[redacted]"));
            }
            _ => {
                hide_next = VALUE_HIDDEN_FLAGS.contains(&arg.as_str());
                shown.push(arg.clone());
            }
        }
    }
    shown.join(" ")
}

/// Run a runtime command, inheriting stdio so the user sees build/run progress.
pub(super) fn exec_inherit(bin: &str, args: &[String]) -> Result<(), Error> {
    let status = Command::new(bin)
        .args(args)
        .status()
        .map_err(|e| spawn_error(bin, &e))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::new(
            ErrorCode::InternalError,
            format!("`{bin} {}` failed ({status})", display_args(args)),
        ))
    }
}

/// Run a runtime command with stdout/stderr appended to a log file, keeping
/// CLI stdout reserved for the final AFDATA envelope.
pub(super) fn exec_to_log(bin: &str, args: &[String], log_file: &Path) -> Result<(), Error> {
    exec_to_log_impl(bin, args, log_file, true)
}

pub(super) fn exec_to_log_without_header(
    bin: &str,
    args: &[String],
    log_file: &Path,
) -> Result<(), Error> {
    exec_to_log_impl(bin, args, log_file, false)
}

fn exec_to_log_impl(
    bin: &str,
    args: &[String],
    log_file: &Path,
    write_header: bool,
) -> Result<(), Error> {
    use std::io::Write;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
        .map_err(|e| {
            Error::new(
                ErrorCode::IoError,
                format!("open log file {}: {e}", log_file.display()),
            )
        })?;
    if write_header {
        writeln!(file, "\n$ {bin} {}", display_args(args)).map_err(|e| {
            Error::new(
                ErrorCode::IoError,
                format!("write log file {}: {e}", log_file.display()),
            )
        })?;
    }
    let stdout = file.try_clone().map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("clone log file {}: {e}", log_file.display()),
        )
    })?;
    let stderr = file.try_clone().map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("clone log file {}: {e}", log_file.display()),
        )
    })?;
    let status = Command::new(bin)
        .args(args)
        .stdout(stdout)
        .stderr(stderr)
        .status()
        .map_err(|e| spawn_error(bin, &e))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::new(
            ErrorCode::InternalError,
            format!(
                "`{bin} {}` failed ({status}); full output was written to {}",
                display_args(args),
                log_file.display()
            ),
        ))
    }
}

pub(super) fn tail_lines_from_file(
    path: &Path,
    max_lines: usize,
) -> Result<(Vec<String>, bool), Error> {
    use std::io::{Read, Seek, SeekFrom};

    const MAX_TAIL_BYTES: u64 = 256 * 1024;
    let mut file = std::fs::File::open(path).map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("open container log file {}: {e}", path.display()),
        )
    })?;
    let len = file
        .metadata()
        .map_err(|e| {
            Error::new(
                ErrorCode::IoError,
                format!("stat container log file {}: {e}", path.display()),
            )
        })?
        .len();
    let start = len.saturating_sub(MAX_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("seek container log file {}: {e}", path.display()),
        )
    })?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("read container log file {}: {e}", path.display()),
        )
    })?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().collect();
    let truncated_by_bytes = start > 0;
    if truncated_by_bytes && !text.starts_with('\n') && !lines.is_empty() {
        lines.remove(0);
    }
    let truncated = truncated_by_bytes || lines.len() > max_lines;
    let tail_lines = lines
        .iter()
        .skip(lines.len().saturating_sub(max_lines))
        .map(|line| (*line).to_string())
        .collect();
    Ok((tail_lines, truncated))
}

/// Run a runtime command capturing stdout/stderr.
pub(super) fn capture(bin: &str, args: &[String]) -> Result<std::process::Output, Error> {
    Command::new(bin)
        .args(args)
        .output()
        .map_err(|e| spawn_error(bin, &e))
}

#[cfg(test)]
mod tests {
    use super::display_args;

    #[test]
    fn a_logged_command_line_hides_credential_bearing_values() {
        let args: Vec<String> = [
            "run",
            "--name",
            "afhttp-host",
            "-e",
            "AFHTTP_TOKEN_SECRET=abc",
            "afhttp-host:latest",
            "--engine-env",
            "HTTPS_PROXY=http://user:pw@proxy:3128",
            "--proxy=http://user:pw@proxy:3128",
            "--token-secret",
            "literal:xyz",
        ]
        .map(String::from)
        .to_vec();
        let shown = display_args(&args);
        for secret in ["abc", "user:pw", "xyz"] {
            assert!(!shown.contains(secret), "{shown}");
        }
        assert!(shown.contains("--name afhttp-host"), "{shown}");
        assert!(shown.contains("--proxy=[redacted]"), "{shown}");
    }
}
