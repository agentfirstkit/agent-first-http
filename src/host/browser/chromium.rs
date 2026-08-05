use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use chromiumoxide::Browser;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;
use tokio::sync::Mutex;

use super::{
    BackendKeepalive, BrowserHandle, apply_subprocess_env, ensure_download_dir,
    fingerprint_seed_from_path, new_stderr_ring, pick_ephemeral_port,
    profile_launch::resolve_profile_dir, resolve_browser_bin, stderr_tail_summary,
    wait_for_tcp_ready,
};
use crate::host::bootstrap::{BrowserChoice, DisplayMode, HostArgs};
use crate::shared::error::{Error, ErrorCode};

pub(super) async fn launch(args: &HostArgs) -> Result<BrowserHandle, Error> {
    let profile = &args.profile;
    let backend_key = args.browser.profile_backend_key();
    let (profile_dir, ephemeral, profile_lock) = resolve_profile_dir(profile, backend_key)?;
    preseed_chromium_profile(&profile_dir)?;
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
            let stderr = stderr_tail_summary(&stderr_ring).await;
            last_error = Some(Error::new(
                ErrorCode::BrowserLaunchFailed,
                launch_failure_detail(
                    args,
                    &profile_dir,
                    port,
                    &format!("did not accept connections: {e}"),
                    &stderr,
                ),
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
            BrowserChoice::Auto | BrowserChoice::Chromium => "chromium",
            BrowserChoice::Chrome => "chrome",
            BrowserChoice::FingerprintChromium => "fingerprint-chromium",
            BrowserChoice::Edge => "edge",
            BrowserChoice::Brave => "brave",
            BrowserChoice::Lightpanda | BrowserChoice::Camoufox => "chromium",
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

fn launch_failure_detail(
    args: &HostArgs,
    profile_dir: &std::path::Path,
    port: u16,
    reason: &str,
    stderr: &str,
) -> String {
    let mut detail = format!(
        "chromium backend={} profile={} display={:?} port={port} {reason}",
        args.browser.profile_backend_key(),
        profile_dir.display(),
        args.display
    );
    if !stderr.is_empty() {
        detail.push_str("; browser_stderr_tail=");
        detail.push_str(stderr);
    }
    detail
}

fn chromium_args(args: &HostArgs, profile_dir: &Path, port: u16) -> Vec<String> {
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
    // Do NOT pass --disable-gpu. It switches off the GPU process wholesale, and
    // with it WebGL: a browser launched that way reports webgl=false, webgl2=false
    // and zero WebGL extensions. No real browser looks like that, so every
    // anti-bot scorer reads it as automation — measured against a Cloudflare
    // Turnstile wall, the no-WebGL profile never cleared even with a human
    // clicking the checkbox.
    //
    // --enable-unsafe-swiftshader only *permits* the software fallback; hardware
    // GL still wins wherever it exists, so this is a no-op on a machine with a
    // usable GPU and restores full WebGL 1 + 2 on the headless/container hosts
    // that have none. Callers who really want the GPU off can still pass
    // --disable-gpu through --browser-arg.
    out.push("--enable-unsafe-swiftshader".to_string());
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

fn preseed_chromium_profile(profile_dir: &Path) -> Result<(), Error> {
    // These are browser-chrome hygiene prefs, not target-site state. They keep
    // persistent hard-site profiles from surfacing crash/metrics/password UI
    // over the takeover display after an unclean container stop.
    fs::create_dir_all(profile_dir).map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("create profile dir {}: {e}", profile_dir.display()),
        )
    })?;
    remove_stale_process_singleton_files(profile_dir)?;
    merge_json_file(&profile_dir.join("Local State"), |root| {
        set_json_path(
            root,
            &["browser", "has_seen_welcome_page"],
            Value::Bool(true),
        );
        set_json_path(
            root,
            &["browser", "should_reset_check_default_browser"],
            Value::Bool(false),
        );
        set_json_path(
            root,
            &["user_experience_metrics", "reporting_enabled"],
            Value::Bool(false),
        );
        set_json_path(
            root,
            &["user_experience_metrics", "stability", "exited_cleanly"],
            Value::Bool(true),
        );
        set_json_path(root, &["brave", "p3a", "enabled"], Value::Bool(false));
        set_json_path(
            root,
            &["brave", "stats", "reporting_enabled"],
            Value::Bool(false),
        );
    })?;

    let default_profile = profile_dir.join("Default");
    fs::create_dir_all(&default_profile).map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!(
                "create default profile dir {}: {e}",
                default_profile.display()
            ),
        )
    })?;
    merge_json_file(&default_profile.join("Preferences"), |root| {
        set_json_path(
            root,
            &["profile", "exit_type"],
            Value::String("Normal".into()),
        );
        set_json_path(root, &["profile", "exited_cleanly"], Value::Bool(true));
        set_json_path(
            root,
            &["profile", "password_manager_enabled"],
            Value::Bool(false),
        );
        set_json_path(root, &["credentials_enable_service"], Value::Bool(false));
        set_json_path(root, &["autofill", "profile_enabled"], Value::Bool(false));
        set_json_path(
            root,
            &["autofill", "credit_card_enabled"],
            Value::Bool(false),
        );
        set_json_path(
            root,
            &["safebrowsing", "scout_reporting_enabled_when_deprecated"],
            Value::Bool(false),
        );
        // Downloads must land in the profile's own downloads/ without a native
        // save dialog, and these prefs are the only thing that guarantees it.
        // `Browser.setDownloadBehavior` cannot: that override belongs to the
        // DevTools client that set it and dies with it, and a human clicking a
        // link on the takeover display is not a CDP client at all. Left to the
        // browser default, a takeover download stops on a GTK "wants to save"
        // dialog pointing at ~/Downloads — the display blocks and nothing is
        // captured. Pinned by
        // `takeover_download_lands_in_profile_without_a_save_dialog`.
        let downloads = profile_dir.join("downloads");
        set_json_path(
            root,
            &["download", "prompt_for_download"],
            Value::Bool(false),
        );
        set_json_path(
            root,
            &["download", "default_directory"],
            Value::String(downloads.display().to_string()),
        );
        set_json_path(
            root,
            &["savefile", "default_directory"],
            Value::String(downloads.display().to_string()),
        );
    })?;
    Ok(())
}

fn remove_stale_process_singleton_files(profile_dir: &Path) -> Result<(), Error> {
    // Brave/Chromium leaves these process-singleton artifacts behind on
    // unclean container stops. afhttp has already acquired its own profile
    // lock before this point, so removing stale browser locks is safe here.
    for name in ["SingletonCookie", "SingletonLock", "SingletonSocket"] {
        let path = profile_dir.join(name);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(Error::new(
                    ErrorCode::IoError,
                    format!("remove stale chromium singleton {}: {e}", path.display()),
                ));
            }
        }
    }
    Ok(())
}

fn merge_json_file(path: &Path, mutate: impl FnOnce(&mut Value)) -> Result<(), Error> {
    let mut root = match fs::read(path) {
        Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => Value::Object(Default::default()),
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            Error::new(
                ErrorCode::IoError,
                format!("parse browser profile JSON {}: {e}", path.display()),
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Object(Default::default()),
        Err(e) => {
            return Err(Error::new(
                ErrorCode::IoError,
                format!("read browser profile JSON {}: {e}", path.display()),
            ));
        }
    };
    if !root.is_object() {
        root = Value::Object(Default::default());
    }
    mutate(&mut root);
    write_json_file(path, &root)
}

fn write_json_file(path: &Path, value: &Value) -> Result<(), Error> {
    let parent = path.parent().ok_or_else(|| {
        Error::new(
            ErrorCode::IoError,
            format!("browser profile JSON path {} has no parent", path.display()),
        )
    })?;
    fs::create_dir_all(parent).map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("create browser profile JSON dir {}: {e}", parent.display()),
        )
    })?;
    let tmp_name = format!(
        ".{}.afhttp-tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("profile-json")
    );
    let tmp = parent.join(tmp_name);
    let bytes = serde_json::to_vec(value).map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("serialize browser profile JSON {}: {e}", path.display()),
        )
    })?;
    fs::write(&tmp, bytes).map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("write browser profile JSON {}: {e}", tmp.display()),
        )
    })?;
    fs::rename(&tmp, path).map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!(
                "replace browser profile JSON {} with {}: {e}",
                path.display(),
                tmp.display()
            ),
        )
    })
}

fn set_json_path(root: &mut Value, path: &[&str], value: Value) {
    let mut cursor = root;
    for key in &path[..path.len().saturating_sub(1)] {
        if !cursor.is_object() {
            *cursor = Value::Object(Default::default());
        }
        let Value::Object(map) = cursor else {
            return;
        };
        cursor = map
            .entry((*key).to_string())
            .or_insert_with(|| Value::Object(Default::default()));
    }
    if let Some(last) = path.last() {
        if !cursor.is_object() {
            *cursor = Value::Object(Default::default());
        }
        if let Value::Object(map) = cursor {
            map.insert((*last).to_string(), value);
        }
    }
}

// Curated from chromiumoxide's default Puppeteer-derived launch flags, plus
// afhttp's stability flags. Chromium honors last-wins, so browser_args append.
const CHROMIUM_DEFAULT_FLAGS: &[&str] = &[
    "disable-background-networking",
    "disable-background-timer-throttling",
    "disable-backgrounding-occluded-windows",
    "disable-breakpad",
    "disable-crash-reporter",
    "disable-crash-reporter-for-testing",
    "disable-client-side-phishing-detection",
    "disable-component-extensions-with-background-pages",
    "disable-default-apps",
    "disable-dev-shm-usage",
    "disable-hang-monitor",
    "disable-infobars",
    "disable-ipc-flooding-protection",
    "disable-popup-blocking",
    "disable-prompt-on-repost",
    "disable-renderer-backgrounding",
    "disable-search-engine-choice-screen",
    "disable-session-crashed-bubble",
    "disable-sync",
    "metrics-recording-only",
    "no-default-browser-check",
    "no-first-run",
    "noerrdialogs",
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

// The host used to open a throwaway CDP connection here and send
// `Browser.setDownloadBehavior`. It never did anything: Chromium scopes that
// override to the DevTools client that issued it and drops it the moment the
// client disconnects, which was immediately. The profile prefs written by
// `preseed_chromium_profile` now carry the same download directory and suppress
// the save prompt, they apply to a human clicking on the takeover display (who
// is not a CDP client at all), and each fetch sets its own behavior with
// `eventsEnabled` on its own live connection where it needs download events —
// see `sdk::fetch::pipeline::browser::download`. Nothing at host level consumed
// browser-level download events, so there is nothing left for this to do.

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::host::bootstrap::{HealthPublic, ProfileChoice, Takeover};

    fn base_args() -> HostArgs {
        HostArgs {
            listen: "tcp:127.0.0.1:0".into(),
            profile: ProfileChoice::Ephemeral,
            display: DisplayMode::Headless,
            takeover: Takeover::Off,
            display_quality: 100,
            browser: BrowserChoice::Chromium,
            browser_bin: None,
            token: None,
            takeover_enabled: false,
            health_enabled: true,
            health_public: HealthPublic::Off,
            engine_envs: Vec::new(),
            browser_args: Vec::new(),
            proxy: None,
            recent_requests_cap: 0,
        }
    }

    #[test]
    fn chromium_args_suppress_browser_chrome_popups_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = base_args();
        args.browser_args
            .push("proxy-server=http://override".into());
        let out = chromium_args(&args, dir.path(), 12345);

        for expected in [
            "--disable-crash-reporter",
            "--disable-crash-reporter-for-testing",
            "--disable-infobars",
            "--disable-search-engine-choice-screen",
            "--disable-session-crashed-bubble",
            "--no-default-browser-check",
            "--no-first-run",
            "--noerrdialogs",
        ] {
            assert!(
                out.iter().any(|arg| arg == expected),
                "missing {expected}: {out:?}"
            );
        }
        assert!(
            !out.iter().any(|arg| arg == "--enable-automation"),
            "automation infobar flag should stay off: {out:?}"
        );
        assert_eq!(
            out.last().map(String::as_str),
            Some("--proxy-server=http://override")
        );
    }

    /// --disable-gpu takes WebGL down with it (webgl=false, webgl2=false, zero
    /// extensions), which no real browser reports and every anti-bot scorer
    /// penalises. Both display modes must keep the software-GL fallback instead.
    #[test]
    fn chromium_args_keep_webgl_available_in_both_display_modes() {
        let dir = tempfile::tempdir().unwrap();
        for display in [DisplayMode::Headless, DisplayMode::Headful] {
            let mut args = base_args();
            args.display = display;
            let out = chromium_args(&args, dir.path(), 12345);
            assert!(
                !out.iter().any(|arg| arg == "--disable-gpu"),
                "--disable-gpu removes WebGL entirely ({display:?}): {out:?}"
            );
            assert!(
                out.iter().any(|arg| arg == "--enable-unsafe-swiftshader"),
                "software GL fallback keeps WebGL alive without a GPU ({display:?}): {out:?}"
            );
        }
    }

    /// The escape hatch: a caller who genuinely wants the GPU off can still say
    /// so, and --browser-arg is appended after the defaults so it wins.
    #[test]
    fn browser_arg_can_still_disable_the_gpu() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = base_args();
        args.browser_args.push("disable-gpu".into());
        let out = chromium_args(&args, dir.path(), 12345);
        assert_eq!(out.last().map(String::as_str), Some("--disable-gpu"));
    }

    #[test]
    fn profile_preseed_marks_previous_crash_clean_and_disables_reporting_ui() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Local State"),
            serde_json::to_vec(&json!({
                "keep": 1,
                "user_experience_metrics": {"reporting_enabled": true}
            }))
            .unwrap(),
        )
        .unwrap();
        let default_dir = dir.path().join("Default");
        std::fs::create_dir_all(&default_dir).unwrap();
        std::fs::write(
            default_dir.join("Preferences"),
            serde_json::to_vec(&json!({
                "profile": {"exit_type": "Crashed"},
                "credentials_enable_service": true
            }))
            .unwrap(),
        )
        .unwrap();

        preseed_chromium_profile(dir.path()).unwrap();

        let local_state: Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("Local State")).unwrap())
                .unwrap();
        assert_eq!(local_state["keep"], 1);
        assert_eq!(local_state["browser"]["has_seen_welcome_page"], true);
        assert_eq!(
            local_state["user_experience_metrics"]["reporting_enabled"],
            false
        );
        assert_eq!(local_state["brave"]["p3a"]["enabled"], false);

        let prefs: Value =
            serde_json::from_slice(&std::fs::read(default_dir.join("Preferences")).unwrap())
                .unwrap();
        assert_eq!(prefs["profile"]["exit_type"], "Normal");
        assert_eq!(prefs["profile"]["exited_cleanly"], true);
        assert_eq!(prefs["profile"]["password_manager_enabled"], false);
        assert_eq!(prefs["credentials_enable_service"], false);
    }

    /// A takeover download used to stop on a native "wants to save" dialog
    /// pointing at `~/Downloads`, capturing nothing and blocking the display
    /// until a human clicked through it.
    ///
    /// `Browser.setDownloadBehavior` cannot be the only guard: Chromium scopes
    /// that override to the DevTools client that set it, and a human clicking a
    /// link on the takeover display is not a CDP client at all. The prefs below
    /// are the part that survives both, so they are pinned here rather than on
    /// the CDP call.
    #[test]
    fn profile_preseed_routes_downloads_into_the_profile_without_a_prompt() {
        let dir = tempfile::tempdir().unwrap();

        preseed_chromium_profile(dir.path()).unwrap();

        let prefs: Value = serde_json::from_slice(
            &std::fs::read(dir.path().join("Default").join("Preferences")).unwrap(),
        )
        .unwrap();
        let downloads = dir.path().join("downloads").display().to_string();
        assert_eq!(
            prefs["download"]["prompt_for_download"], false,
            "a save dialog on the takeover display blocks the whole session"
        );
        assert_eq!(prefs["download"]["default_directory"], downloads);
        assert_eq!(prefs["savefile"]["default_directory"], downloads);
    }

    #[test]
    fn profile_preseed_removes_stale_chromium_singleton_files() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["SingletonCookie", "SingletonLock", "SingletonSocket"] {
            std::fs::write(dir.path().join(name), "stale").unwrap();
        }

        preseed_chromium_profile(dir.path()).unwrap();

        for name in ["SingletonCookie", "SingletonLock", "SingletonSocket"] {
            assert!(!dir.path().join(name).exists(), "{name} should be removed");
        }
    }
}
