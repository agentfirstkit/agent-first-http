#![cfg(feature = "cli")]
#![allow(clippy::unwrap_used)]

use std::process::Command;

#[test]
fn empty_environment_and_file_tokens_fail_before_browser_launch() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("token.json");
    std::fs::write(&config, r#"{"token_secret":""}"#).unwrap();
    let blank = temp.path().join("blank.json");
    std::fs::write(&blank, r#"{"token_secret":" \t"}"#).unwrap();
    let sources = [
        "env:AFHTTP_TEST_EMPTY_TOKEN".to_string(),
        "env:AFHTTP_TEST_BLANK_TOKEN".to_string(),
        format!("file:{}#token_secret", config.display()),
        format!("file:{}#token_secret", blank.display()),
    ];
    for source in sources {
        let output = Command::new(env!("CARGO_BIN_EXE_afhttp"))
            .args([
                "host",
                "--listen",
                "tcp:0.0.0.0:9222",
                "--token-secret",
                &source,
                "--browser-bin",
                "/nonexistent-browser-must-not-be-started",
            ])
            .env("AFHTTP_TEST_EMPTY_TOKEN", "")
            .env("AFHTTP_TEST_BLANK_TOKEN", " ")
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = output
            .stdout
            .split(|byte| *byte == b'\n')
            .chain(output.stderr.split(|byte| *byte == b'\n'))
            .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
            .find(|event| event["kind"] == "error")
            .unwrap();
        assert_eq!(error["error"]["code"], "invalid_argument");
        assert_eq!(
            error["error"]["message"],
            "--token-secret: token source resolved to an empty or whitespace-only value",
            "{source}"
        );
    }
}
