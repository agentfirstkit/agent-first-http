use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use super::{
    apply_subprocess_env, ensure_download_dir, new_stderr_ring, pick_ephemeral_port,
    profile_launch::resolve_profile_dir, resolve_browser_bin, wait_for_tcp_ready, BackendKeepalive,
    BrowserHandle,
};
use crate::host::bootstrap::{HostArgs, ProfileChoice};
use crate::shared::error::{Error, ErrorCode};

/// Spawn `lightpanda serve` on a pre-bound ephemeral port and return a
/// `BrowserHandle` whose `ws_url` points at the CDP endpoint. Lightpanda
/// exposes CDP at `ws://host:port` directly, not at the chromium-style
/// `/devtools/browser/<id>` path. We pre-bind the port ourselves (then
/// hand it to lightpanda) instead of parsing stdout because lightpanda's
/// log writer truncates the address line when not attached to a TTY.
pub(super) async fn launch(args: &HostArgs) -> Result<BrowserHandle, Error> {
    let profile = &args.profile;
    // Persistent profiles aren't supported yet — lightpanda's storage model
    // is in-memory only. Refuse loudly instead of silently ignoring the flag.
    if matches!(profile, ProfileChoice::Persistent(_)) {
        return Err(Error::new(
            ErrorCode::BackendUnsupported,
            "lightpanda does not support persistent profiles; use `--profile -` for ephemeral",
        ));
    }
    let (profile_dir, ephemeral, profile_lock) = resolve_profile_dir(profile)?;
    let download_dir = ensure_download_dir(&profile_dir).await?;
    let bin = resolve_browser_bin(args)?;

    let version = read_lightpanda_version(&bin).await;

    let port = pick_ephemeral_port().map_err(|e| {
        Error::new(
            ErrorCode::BrowserLaunchFailed,
            format!("could not reserve ephemeral port for lightpanda: {e}"),
        )
    })?;

    let mut cmd = Command::new(&bin);
    cmd.arg("serve")
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string());
    if let Some(url) = args.proxy.as_deref() {
        cmd.arg("--http_proxy").arg(url);
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
            format!("spawn lightpanda {}: {e}", bin.display()),
        )
    })?;
    let process_id = child.id();
    let stderr_ring = new_stderr_ring(child.stderr.take());

    if let Err(e) = wait_for_tcp_ready(("127.0.0.1", port), Duration::from_secs(10)).await {
        // Best-effort cleanup before reporting; `child` is dropped on return
        // which sends SIGKILL via kill_on_drop, but we hurry it along.
        drop(child);
        return Err(Error::new(
            ErrorCode::BrowserLaunchFailed,
            format!("lightpanda did not accept connections on port {port}: {e}"),
        ));
    }

    Ok(BrowserHandle {
        ws_url: format!("ws://127.0.0.1:{port}"),
        family: "lightpanda".to_string(),
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
async fn read_lightpanda_version(bin: &std::path::Path) -> String {
    // Lightpanda uses a subcommand (`lightpanda version`), not `--version`.
    let out = match Command::new(bin).arg("version").output().await {
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
