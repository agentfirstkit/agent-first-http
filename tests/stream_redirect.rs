#![cfg(feature = "cli")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods,
    clippy::disallowed_macros
)]

use std::{fs, process::Command};

#[test]
fn stdout_file_redirects_version_output() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let stdout_file = tempdir.path().join("afhttp.stdout");
    let stderr_file = tempdir.path().join("afhttp.stderr");

    let output = Command::new(env!("CARGO_BIN_EXE_afhttp"))
        .arg("--stdout-file")
        .arg(&stdout_file)
        .arg("--stderr-file")
        .arg(&stderr_file)
        .arg("--version")
        .output()
        .expect("run afhttp --version");

    assert!(output.status.success(), "status = {}", output.status);
    assert!(output.stdout.is_empty(), "stdout was not redirected");
    assert!(output.stderr.is_empty(), "stderr = {:?}", output.stderr);

    let redirected_stdout = fs::read_to_string(&stdout_file).expect("read stdout file");
    // Bare `--version` is a protocol-v1 event by default now, not conventional
    // `<name> <version>` text.
    let parsed: serde_json::Value =
        serde_json::from_str(redirected_stdout.trim()).expect("version output must be JSON");
    assert_eq!(parsed["kind"], "result");
    assert_eq!(parsed["result"]["code"], "version");
    assert_eq!(parsed["result"]["name"], "afhttp");
    assert_eq!(parsed["result"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        fs::read_to_string(&stderr_file).expect("read stderr file"),
        ""
    );
}

#[test]
fn stderr_file_redirects_a_command_failure_in_split_mode() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let stdout_file = tempdir.path().join("afhttp.stdout");
    let stderr_file = tempdir.path().join("afhttp.stderr");

    let output = Command::new(env!("CARGO_BIN_EXE_afhttp"))
        .arg("fetch")
        .arg("http://127.0.0.1:1/will-fail")
        .arg("--render")
        .arg("none")
        .arg("--timeout-ms")
        .arg("1000")
        .arg("--out")
        .arg(tempdir.path())
        .arg("--stdout-file")
        .arg(&stdout_file)
        .arg("--stderr-file")
        .arg(&stderr_file)
        .output()
        .expect("run afhttp fetch");

    assert!(!output.status.success(), "status = {}", output.status);
    assert!(output.stdout.is_empty(), "stdout = {:?}", output.stdout);
    assert!(output.stderr.is_empty(), "stderr = {:?}", output.stderr);

    assert_eq!(
        fs::read_to_string(&stdout_file).expect("read stdout file"),
        ""
    );
    let redirected_stderr = fs::read_to_string(&stderr_file).expect("read stderr file");
    assert!(
        redirected_stderr.contains("\"kind\":\"error\""),
        "stderr file = {redirected_stderr}"
    );
}

/// An argv the registry rejects has no output plan, and therefore no file
/// sinks: the rejection is reported on the process's own diagnostic stream and
/// the named files are never touched.
#[test]
fn a_rejected_argv_reports_before_any_file_sink_exists() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let stdout_file = tempdir.path().join("afhttp.stdout");
    let stderr_file = tempdir.path().join("afhttp.stderr");

    let output = Command::new(env!("CARGO_BIN_EXE_afhttp"))
        .arg("--stdout-file")
        .arg(&stdout_file)
        .arg("--stderr-file")
        .arg(&stderr_file)
        .arg("--not-a-real-afhttp-flag")
        .output()
        .expect("run afhttp invalid args");

    assert!(!output.status.success(), "status = {}", output.status);
    assert!(output.stdout.is_empty(), "stdout = {:?}", output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let parsed: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("rejection must be JSON on stderr");
    assert_eq!(parsed["error"]["code"], "cli_unknown_argument");
    assert!(
        parsed["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("--not-a-real-afhttp-flag")),
        "{parsed}"
    );
    assert!(!stdout_file.exists(), "stdout sink was created anyway");
    assert!(!stderr_file.exists(), "stderr sink was created anyway");
}
