use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use chromiumoxide::Browser;
use futures::StreamExt;
use serde::Deserialize;
use tokio::process::Command;
use tokio::sync::Mutex;

use super::{
    apply_subprocess_env, ensure_download_dir, fingerprint_seed_from_path, new_stderr_ring,
    pick_ephemeral_port, profile_launch::resolve_profile_dir, resolve_browser_bin,
    wait_for_tcp_ready, BackendKeepalive, BrowserHandle,
};
use crate::host::bootstrap::{BrowserChoice, DisplayMode, HostArgs};
use crate::shared::error::{Error, ErrorCode};

pub(super) async fn launch(args: &HostArgs) -> Result<BrowserHandle, Error> {
    let profile = &args.profile;
    let (profile_dir, ephemeral, profile_lock) = resolve_profile_dir(profile)?;
    let download_dir = ensure_download_dir(&profile_dir).await?;
    let bin = resolve_browser_bin(args)?;
    let mut last_error = None;
    for attempt in 1..=3 {
        let port = pick_ephemeral_port().map_err(|e| {
            Error::new(
                ErrorCode::BrowserLaunchFailed,
                format!("could not reserve ephemeral port for chromium: {e}"),
            )
        })?;

        let mut cmd = Command::new(&bin);
        cmd.args(chromium_args(args, &profile_dir, port));
        cmd.kill_on_drop(true)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        apply_subprocess_env(&mut cmd, &args.engine_envs);

        let mut child = cmd.spawn().map_err(|e| {
            Error::new(
                ErrorCode::BrowserLaunchFailed,
                format!("spawn chromium {}: {e}", bin.display()),
            )
        })?;
        let process_id = child.id();
        let stderr_ring = new_stderr_ring(child.stderr.take());

        if let Err(e) = wait_for_tcp_ready(("127.0.0.1", port), Duration::from_secs(15)).await {
            let _ = child.start_kill();
            last_error = Some(Error::new(
                ErrorCode::BrowserLaunchFailed,
                format!("chromium did not accept connections on port {port}: {e}"),
            ));
            continue;
        }
        let ws_url = match read_chromium_ws_url(port).await {
            Ok(ws_url) => ws_url,
            Err(e) => {
                let _ = child.start_kill();
                last_error = Some(Error::new(
                    e.error_code,
                    format!("chromium attempt {attempt}/3 on port {port}: {}", e.detail),
                ));
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        if let Err(e) = enable_download_capture(&ws_url, &download_dir).await {
            let _ = child.start_kill();
            last_error = Some(Error::new(
                e.error_code,
                format!("chromium attempt {attempt}/3 download setup: {}", e.detail),
            ));
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }
        let (browser, mut handler) = match Browser::connect(ws_url.clone()).await {
            Ok(pair) => pair,
            Err(e) => {
                let _ = child.start_kill();
                last_error = Some(Error::new(
                    ErrorCode::BrowserLaunchFailed,
                    format!("chromium attempt {attempt}/3 chromiumoxide connect: {e}"),
                ));
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };

        let handler_task = tokio::spawn(async move {
            while let Some(_event) = handler.next().await {
                // The handler task drives chromiumoxide's internal command/event
                // dispatch. We discard events at this level; per-fetch consumers
                // open their own CDP sessions through the proxy.
            }
        });
        let version = match browser.version().await {
            Ok(v) => v.product,
            Err(_) => "unknown".into(),
        };

        let family = match args.browser {
            BrowserChoice::FingerprintChromium => "fingerprint-chromium",
            _ => "chromium",
        }
        .to_string();

        return Ok(BrowserHandle {
            ws_url,
            family,
            version,
            process_id,
            profile_path: profile_dir,
            download_dir,
            _ephemeral_dir: ephemeral,
            _profile_lock: profile_lock,
            _keepalive: BackendKeepalive::Chromium {
                _browser: Arc::new(Mutex::new(browser)),
                handler_task,
                child,
            },
            stderr_ring,
        });
    }
    Err(last_error.unwrap_or_else(|| {
        Error::new(
            ErrorCode::BrowserLaunchFailed,
            "chromium launch exhausted startup attempts",
        )
    }))
}

fn chromium_args(args: &HostArgs, profile_dir: &std::path::Path, port: u16) -> Vec<String> {
    let mut out = Vec::new();
    for flag in CHROMIUM_DEFAULT_FLAGS {
        out.push(format!("--{flag}"));
    }
    for (key, values) in CHROMIUM_DEFAULT_VALUES {
        out.push(format!("--{key}={}", values.join(",")));
    }
    out.push(format!("--remote-debugging-port={port}"));
    out.push("--remote-debugging-address=127.0.0.1".to_string());
    out.push(format!("--user-data-dir={}", profile_dir.display()));
    out.push("--disable-extensions".to_string());
    // Secure by default: keep Chromium's OS sandbox ON. Containers (and the
    // Docker test image) — root, no user namespaces, where the sandbox can't
    // initialize and the container is itself the isolation boundary — opt back
    // in via AFHTTP_NO_SANDBOX. Native runs (e.g. the inline ephemeral host)
    // keep the sandbox. See docs/deployment.md.
    if std::env::var_os("AFHTTP_NO_SANDBOX").is_some() {
        out.push("--no-sandbox".to_string());
        out.push("--disable-setuid-sandbox".to_string());
    }
    out.push("--disable-gpu".to_string());
    match args.proxy.as_deref() {
        Some(url) => out.push(format!("--proxy-server={url}")),
        None => out.push("--no-proxy-server".to_string()),
    }
    if matches!(args.browser, BrowserChoice::FingerprintChromium) {
        out.push(format!(
            "--fingerprint={}",
            fingerprint_seed_from_path(profile_dir)
        ));
    }
    if matches!(args.display, DisplayMode::Headless) {
        out.push("--headless=new".to_string());
        out.push("--hide-scrollbars".to_string());
        out.push("--mute-audio".to_string());
    }
    for raw in &args.browser_args {
        out.push(normalize_browser_arg(raw));
    }
    out
}

// Curated from chromiumoxide's default Puppeteer-derived launch flags, plus
// afhttp's stability flags. Chromium honors last-wins, so browser_args append.
const CHROMIUM_DEFAULT_FLAGS: &[&str] = &[
    "disable-background-networking",
    "disable-background-timer-throttling",
    "disable-backgrounding-occluded-windows",
    "disable-breakpad",
    "disable-client-side-phishing-detection",
    "disable-component-extensions-with-background-pages",
    "disable-default-apps",
    "disable-dev-shm-usage",
    "disable-hang-monitor",
    "disable-ipc-flooding-protection",
    "disable-popup-blocking",
    "disable-prompt-on-repost",
    "disable-renderer-backgrounding",
    "disable-sync",
    "metrics-recording-only",
    "no-first-run",
    "enable-automation",
];

const CHROMIUM_DEFAULT_VALUES: &[(&str, &[&str])] = &[
    (
        "enable-features",
        &["NetworkService", "NetworkServiceInProcess"],
    ),
    ("disable-features", &["TranslateUI"]),
    ("force-color-profile", &["srgb"]),
    ("password-store", &["basic"]),
    ("enable-blink-features", &["IdleDetection"]),
    ("lang", &["en_US"]),
];

fn normalize_browser_arg(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with("--") {
        trimmed.to_string()
    } else {
        format!("--{trimmed}")
    }
}

#[derive(Deserialize)]
struct ChromiumVersion {
    #[serde(rename = "webSocketDebuggerUrl")]
    web_socket_debugger_url: String,
}

async fn read_chromium_ws_url(port: u16) -> Result<String, Error> {
    let url = format!("http://127.0.0.1:{port}/json/version");
    let http = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(|e| Error::new(ErrorCode::InternalError, format!("reqwest client: {e}")))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let resp = match http.get(&url).send().await {
            Ok(resp) => resp,
            Err(e) => {
                if std::time::Instant::now() >= deadline {
                    return Err(Error::new(
                        ErrorCode::BrowserLaunchFailed,
                        format!("GET {url}: {e}"),
                    ));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let status = resp.status();
        if !status.is_success() {
            if std::time::Instant::now() >= deadline {
                return Err(Error::new(
                    ErrorCode::BrowserLaunchFailed,
                    format!("GET {url}: status {status}"),
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }
        let version: ChromiumVersion = match resp.json().await {
            Ok(version) => version,
            Err(e) => {
                if std::time::Instant::now() >= deadline {
                    return Err(Error::new(
                        ErrorCode::BrowserLaunchFailed,
                        format!("decode {url}: {e}"),
                    ));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        if version.web_socket_debugger_url.is_empty() {
            if std::time::Instant::now() >= deadline {
                return Err(Error::new(
                    ErrorCode::BrowserLaunchFailed,
                    format!("{url}: missing webSocketDebuggerUrl"),
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }
        return Ok(version.web_socket_debugger_url);
    }
}

async fn enable_download_capture(
    ws_url: &str,
    download_dir: &std::path::Path,
) -> Result<(), Error> {
    let conn = crate::sdk::cdp::ws_client::Connection::connect(ws_url, None).await?;
    let result = conn
        .send(
            "Browser.setDownloadBehavior",
            &serde_json::json!({
                "behavior": "allow",
                "downloadPath": download_dir.display().to_string(),
                "eventsEnabled": true,
            }),
            None,
        )
        .await;
    conn.close();
    result.map(|_| ())
}
