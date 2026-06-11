use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use super::{
    apply_subprocess_env, ensure_download_dir, new_stderr_ring, pick_ephemeral_port,
    profile_launch::resolve_profile_dir, resolve_named_bin, stderr_tail_summary,
    wait_for_tcp_ready, BackendKeepalive, BrowserHandle,
};
use crate::host::bootstrap::{DisplayMode, HostArgs, ProfileChoice};
use crate::shared::error::{Error, ErrorCode};

/// Spawn `foxbridge` against the camoufox binary on a pre-reserved
/// ephemeral port. foxbridge exposes a chromium-style CDP WebSocket at
/// `ws://127.0.0.1:<PORT>`; it translates calls into Firefox's Juggler
/// protocol against the camoufox child it manages. The host treats the
/// whole stack as one opaque subprocess — same TCP-readiness polling
/// strategy as lightpanda, no stdout parsing.
pub(super) async fn launch(args: &HostArgs) -> Result<BrowserHandle, Error> {
    let profile = &args.profile;
    // Camoufox profile semantics differ from chromium's --user-data-dir
    // model (Firefox profile dirs have their own lock + lifecycle). Refuse
    // persistent until we've shipped explicit support so the isolation
    // invariant stays honest.
    if matches!(profile, ProfileChoice::Persistent(_)) {
        return Err(Error::new(
            ErrorCode::BackendUnsupported,
            "camoufox does not yet support persistent profiles; use `--profile -` for ephemeral",
        ));
    }
    let (profile_dir, ephemeral, profile_lock) =
        resolve_profile_dir(profile, args.browser.profile_backend_key())?;
    let download_dir = ensure_download_dir(&profile_dir).await?;

    // Two binaries: foxbridge (the CDP→Juggler proxy) and camoufox itself.
    let foxbridge_bin = resolve_named_bin("foxbridge", &args.browser_bin)?;
    let camoufox_bin = resolve_named_bin("camoufox", &None)?;

    let version = read_foxbridge_version(&foxbridge_bin).await;

    let port = pick_ephemeral_port().map_err(|e| {
        Error::new(
            ErrorCode::BrowserLaunchFailed,
            format!("could not reserve ephemeral port for foxbridge: {e}"),
        )
    })?;

    let mut cmd = Command::new(&foxbridge_bin);
    cmd.arg("--binary")
        .arg(&camoufox_bin)
        .arg("--port")
        .arg(port.to_string());
    if matches!(args.display, DisplayMode::Headless) {
        cmd.arg("--headless");
    }
    for raw in &args.browser_args {
        cmd.arg(raw);
    }
    cmd.kill_on_drop(true)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    apply_subprocess_env(&mut cmd, &args.engine_envs);

    let mut child = cmd.spawn().map_err(|e| {
        Error::new(
            ErrorCode::BrowserLaunchFailed,
            format!("spawn foxbridge {}: {e}", foxbridge_bin.display()),
        )
    })?;
    let process_id = child.id();
    let stderr_ring = new_stderr_ring(child.stderr.take());

    if let Err(e) = wait_for_tcp_ready(("127.0.0.1", port), Duration::from_secs(15)).await {
        drop(child);
        let stderr = stderr_tail_summary(&stderr_ring).await;
        let suffix = if stderr.is_empty() {
            String::new()
        } else {
            format!("; browser_stderr_tail={stderr}")
        };
        return Err(Error::new(
            ErrorCode::BrowserLaunchFailed,
            format!(
                "foxbridge backend={} profile={} display={:?} port={port} did not accept connections: {e}{suffix}",
                args.browser.profile_backend_key(),
                profile_dir.display(),
                args.display
            ),
        ));
    }

    Ok(BrowserHandle {
        ws_url: format!("ws://127.0.0.1:{port}"),
        family: "camoufox".to_string(),
        version,
        process_id,
        profile_path: profile_dir,
        download_dir,
        _ephemeral_dir: ephemeral,
        _profile_lock: profile_lock,
        _keepalive: BackendKeepalive::Subprocess { child },
        stderr_ring,
    })
}
async fn read_foxbridge_version(bin: &std::path::Path) -> String {
    let out = match Command::new(bin).arg("--version").output().await {
        Ok(o) => o,
        Err(_) => return "unknown".into(),
    };
    if !out.status.success() {
        return "unknown".into();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("unknown")
        .trim()
        .to_string()
}
