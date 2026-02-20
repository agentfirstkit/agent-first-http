use std::path::Path;
use std::process::Command;

fn pick_free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("bind ephemeral test port")
        .local_addr()
        .expect("resolve local addr")
        .port()
}

fn run_script(root: &Path, script: &str, afh_bin: &str) {
    let http_port = pick_free_port().to_string();
    let ws_port = pick_free_port().to_string();
    let output = Command::new("python3")
        .arg(script)
        .current_dir(root)
        .env("AFH_BIN", afh_bin)
        .env("AFH_VERSION", env!("CARGO_PKG_VERSION"))
        .env("AFH_COVERAGE_MODE", "1")
        .env("AFH_TEST_HTTP_PORT", &http_port)
        .env("AFH_TEST_WS_PORT", &ws_port)
        .output()
        .expect("failed to run python3");
    if !output.status.success() {
        panic!(
            "script {} failed (status={}):\nstdout:\n{}\nstderr:\n{}",
            script,
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn run_python_stress_suites_for_coverage() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let afh_bin = env!("CARGO_BIN_EXE_afhttp");
    run_script(root, "tests/stress.py", afh_bin);
    run_script(root, "tests/cli_stress.py", afh_bin);
    run_script(root, "tests/ws_stress.py", afh_bin);
}
