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
    // `<name> <version>` text — this is also the regression case for the
    // pre-parser mis-stopping at a preceding --stdout-file/--stderr-file value.
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
fn stderr_file_redirects_cli_error_json_in_split_mode() {
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
    assert!(output.stderr.is_empty(), "stderr = {:?}", output.stderr);

    let redirected_stdout = fs::read_to_string(&stdout_file).expect("read stdout file");
    assert_eq!(redirected_stdout, "");
    let redirected_stderr = fs::read_to_string(&stderr_file).expect("read stderr file");
    assert!(redirected_stderr.contains("\"kind\":\"error\""));
    assert!(redirected_stderr.contains("--not-a-real-afhttp-flag"));
}
