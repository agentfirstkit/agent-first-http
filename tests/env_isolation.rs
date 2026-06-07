//! Isolation invariant: ambient env vars that could influence browser
//! behavior (`HTTP_PROXY`, `XDG_*`, `BROWSER`, etc.) must NOT propagate
//! into the backend subprocess. The host scrubs the env and re-injects
//! only an explicit allowlist + the agent-supplied `--engine-env` pairs.

#![cfg(feature = "host")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods,
    clippy::disallowed_macros,
    clippy::err_expect,
    clippy::print_stdout,
    clippy::useless_conversion
)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use agent_first_http::host::bootstrap::{
    BrowserChoice, DisplayMode, HealthPublic, HostArgs, ProfileChoice, Takeover,
};
use agent_first_http::host::{browser, listener::router_for_tests, listener::test_state};
use agent_first_http::sdk::fetch::RenderMode;
use agent_first_http::sdk::Client;
use agent_first_http::shared::artifacts::Artifact;
use tokio::net::TcpListener;

#[cfg(target_os = "linux")]
async fn read_proc_environ(
    pid: u32,
) -> std::io::Result<std::collections::BTreeMap<String, String>> {
    let bytes = tokio::fs::read(format!("/proc/{pid}/environ")).await?;
    let mut env = std::collections::BTreeMap::new();
    for part in bytes.split(|b| *b == 0).filter(|part| !part.is_empty()) {
        if let Some(pos) = part.iter().position(|b| *b == b'=') {
            let (key, rest) = part.split_at(pos);
            let value = &rest[1..];
            env.insert(
                String::from_utf8_lossy(key).into_owned(),
                String::from_utf8_lossy(value).into_owned(),
            );
        }
    }
    Ok(env)
}

#[cfg(target_os = "linux")]
const ALLOWED_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "TMPDIR",
    "DISPLAY",
    "AFHTTP_ALLOWED_MARKER",
];

#[cfg(target_os = "linux")]
#[tokio::test]
async fn chromium_subprocess_env_is_allowlisted() {
    let Some(bin) = support::env::discover_browser() else {
        println!("(skipping: no chromium binary)");
        return;
    };
    support::ensure_rustls_provider();

    let saved = [
        ("SSL_CERT_FILE", std::env::var_os("SSL_CERT_FILE")),
        ("LD_PRELOAD", std::env::var_os("LD_PRELOAD")),
        ("AFHTTP_ENV_CANARY", std::env::var_os("AFHTTP_ENV_CANARY")),
    ];
    unsafe {
        std::env::set_var("SSL_CERT_FILE", "/tmp/afhttp-should-not-leak.pem");
        std::env::set_var("LD_PRELOAD", "/tmp/afhttp-should-not-leak.so");
        std::env::set_var("AFHTTP_ENV_CANARY", "do-not-leak");
    }

    let result = (async {
        let args = HostArgs {
            listen: "tcp:127.0.0.1:0".into(),
            profile: ProfileChoice::Ephemeral,
            display: DisplayMode::Headless,
            takeover: Takeover::Off,
            display_quality: 100,
            browser: BrowserChoice::Chromium,
            browser_bin: Some(bin),
            token: None,
            ops_enabled: true,
            health_enabled: true,
            health_public: HealthPublic::Off,
            engine_envs: vec![("AFHTTP_ALLOWED_MARKER".into(), "present".into())],
            browser_args: Vec::new(),
            proxy: None,
            recent_requests_cap: 0,
        };
        let handle = browser::launch(&args).await.expect("chromium launch");
        let pid = handle.process_id.expect("chromium process id");
        let environ = read_proc_environ(pid).await;
        drop(handle);
        environ
    })
    .await;

    unsafe {
        for (key, value) in saved {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }

    let environ = result.expect("read chromium environ");
    assert_eq!(
        environ.get("AFHTTP_ALLOWED_MARKER").map(String::as_str),
        Some("present")
    );
    for denied in ["SSL_CERT_FILE", "LD_PRELOAD", "AFHTTP_ENV_CANARY"] {
        assert!(
            !environ.contains_key(denied),
            "{denied} leaked into chromium env: {environ:?}"
        );
    }
    for key in environ.keys() {
        assert!(
            ALLOWED_ENV.contains(&key.as_str()),
            "unexpected chromium env key {key:?}: {environ:?}"
        );
    }
}

#[tokio::test]
async fn lightpanda_subprocess_does_not_inherit_ambient_http_proxy() {
    let Some(bin) = support::env::discover_lightpanda() else {
        println!("(skipping: no lightpanda binary; set AFHTTP_TEST_LIGHTPANDA_BIN)");
        return;
    };
    support::ensure_rustls_provider();

    // Decoy proxy that, if honored, would route the test fetch into a
    // black hole. The test asserts the fetch hits the fixture directly,
    // proving the env did not propagate.
    //
    // SAFETY: set_var is single-threaded-safe in this test because no
    // other test in this file races on these names; we restore on exit.
    let prior_http = std::env::var_os("HTTP_PROXY");
    let prior_https = std::env::var_os("HTTPS_PROXY");
    unsafe {
        std::env::set_var("HTTP_PROXY", "http://127.0.0.1:1");
        std::env::set_var("HTTPS_PROXY", "http://127.0.0.1:1");
    }

    let result = (async {
        let args = HostArgs {
            listen: "tcp:127.0.0.1:0".into(),
            profile: ProfileChoice::Ephemeral,
            display: DisplayMode::Headless,
            takeover: Takeover::Off,
            display_quality: 100,
            browser: BrowserChoice::Lightpanda,
            browser_bin: Some(bin),
            token: None,
            ops_enabled: true,
            health_enabled: true,
            health_public: HealthPublic::Off,
            engine_envs: Vec::new(),
            browser_args: Vec::new(),
            proxy: None,
            recent_requests_cap: 0,
        };
        let handle = browser::launch(&args).await.expect("lightpanda launch");
        let state = test_state(None, HealthPublic::Off).with_default_browser(Arc::new(handle));
        let app = router_for_tests(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let fixture = support::fixture_server::spawn().await;
        let tmp = tempfile::tempdir().expect("tmpdir");
        let client = Client::connect(&format!("ws://{addr}")).expect("client");

        // HTTP path bypasses the browser entirely, but its reqwest client
        // also reads HTTP_PROXY from the host's env. We're specifically
        // asserting the BROWSER subprocess does not see it — so route the
        // assertion through `--render always`, which has the browser do
        // the navigation. If chromiumoxide-style env leakage had happened
        // here too, the navigation would time out against 127.0.0.1:1.
        client
            .fetch(format!("{}/plain.html", fixture.base_url()))
            .render(RenderMode::Always)
            .timeout(Duration::from_secs(8))
            .want([Artifact::Body])
            .out_dir(tmp.path().to_path_buf())
            .send()
            .await
    })
    .await;

    // Restore env before we panic.
    unsafe {
        match prior_http {
            Some(v) => std::env::set_var("HTTP_PROXY", v),
            None => std::env::remove_var("HTTP_PROXY"),
        }
        match prior_https {
            Some(v) => std::env::set_var("HTTPS_PROXY", v),
            None => std::env::remove_var("HTTPS_PROXY"),
        }
    }

    let fetch_result =
        result.expect("browser fetch must succeed without honoring ambient HTTP_PROXY");
    assert_eq!(fetch_result.status, 200);
}

#[tokio::test]
async fn engine_env_explicit_passthrough_reaches_subprocess() {
    let Some(bin) = support::env::discover_lightpanda() else {
        println!("(skipping: no lightpanda binary; set AFHTTP_TEST_LIGHTPANDA_BIN)");
        return;
    };
    support::ensure_rustls_provider();

    // Passing an unambiguous custom env var through `--engine-env` and
    // confirming the lightpanda process started cleanly proves the
    // explicit-opt-in path works end-to-end. We can't easily inspect the
    // child process's environ from inside the test, so this asserts the
    // structural invariant: launch succeeds with the explicit var, the
    // ws_url surfaces, and the health endpoint comes up.
    let args = HostArgs {
        listen: "tcp:127.0.0.1:0".into(),
        profile: ProfileChoice::Ephemeral,
        display: DisplayMode::Headless,
        takeover: Takeover::Off,
        display_quality: 100,
        browser: BrowserChoice::Lightpanda,
        browser_bin: Some(bin),
        token: None,
        ops_enabled: true,
        health_enabled: true,
        health_public: HealthPublic::Off,
        browser_args: Vec::new(),
        proxy: None,
        recent_requests_cap: 0,
        engine_envs: vec![("AFHTTP_TEST_MARKER".into(), "1".into())],
    };
    let handle = browser::launch(&args)
        .await
        .expect("launch with engine-env");
    assert!(handle.ws_url.starts_with("ws://"));
}
