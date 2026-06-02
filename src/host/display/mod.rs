//! In-container real-display takeover supervision.
//!
//! KasmVNC is kept as an external GPL process: afhttp only starts `Xvnc`,
//! waits for its X display + localhost web listener, and reverse-proxies the
//! web client from the authenticated host listener.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncBufReadExt;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::host::browser::{pick_ephemeral_port, resolve_named_bin, wait_for_tcp_ready};
use crate::shared::error::{Error, ErrorCode};

/// Virtual framebuffer geometry for the takeover display. No window manager
/// runs on the X display, so the headful browser window is pinned to this size
/// (see `AppState::launch`) to fill the framebuffer.
pub const DISPLAY_WIDTH: u16 = 1280;
pub const DISPLAY_HEIGHT: u16 = 720;

/// Running KasmVNC display. Cloning the surrounding `Arc` keeps the process
/// alive until the listener state is dropped.
pub struct DisplayHandle {
    pub display: String,
    pub web_port: u16,
    /// Whether a window manager is running on the display. With one, the
    /// browser is kept maximized so the client can use `resize=remote` (the
    /// framebuffer tracks the browser window exactly); without one, the client
    /// must fall back to `resize=scale` (letterboxed).
    pub window_manager: bool,
    _rfb_port: u16,
    child: Mutex<Child>,
    wm_child: Option<Mutex<Child>>,
    stderr_task: Option<JoinHandle<()>>,
}

impl Drop for DisplayHandle {
    fn drop(&mut self) {
        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
        if let Some(wm) = &self.wm_child {
            if let Ok(mut child) = wm.try_lock() {
                let _ = child.start_kill();
            }
        }
        if let Ok(mut child) = self.child.try_lock() {
            let _ = child.start_kill();
        }
    }
}

/// Launch KasmVNC's `Xvnc` and wait until both the X display socket and the
/// embedded web client are reachable on localhost.
pub async fn launch_kasmvnc() -> Result<Arc<DisplayHandle>, Error> {
    #[cfg(not(unix))]
    {
        return Err(Error::new(
            ErrorCode::BackendUnsupported,
            "KasmVNC display takeover is only supported on Unix-like container hosts",
        ));
    }

    #[cfg(unix)]
    {
        launch_kasmvnc_unix().await
    }
}

#[cfg(unix)]
async fn launch_kasmvnc_unix() -> Result<Arc<DisplayHandle>, Error> {
    let bin = resolve_kasmvnc_bin()?;
    let web_root = resolve_kasmvnc_web_root()?;
    let display_num = pick_display_number()?;
    let display = format!(":{display_num}");
    let web_port = pick_ephemeral_port().map_err(|e| {
        Error::new(
            ErrorCode::BrowserLaunchFailed,
            format!("could not reserve KasmVNC web port: {e}"),
        )
    })?;
    let rfb_port = pick_ephemeral_port().map_err(|e| {
        Error::new(
            ErrorCode::BrowserLaunchFailed,
            format!("could not reserve KasmVNC VNC port: {e}"),
        )
    })?;

    let mut cmd = Command::new(&bin);
    cmd.arg(&display)
        .arg("-geometry")
        .arg(format!("{DISPLAY_WIDTH}x{DISPLAY_HEIGHT}"))
        .arg("-depth")
        .arg("24")
        .arg("-interface")
        .arg("127.0.0.1")
        .arg("-rfbport")
        .arg(rfb_port.to_string())
        .arg("-websocketPort")
        .arg(web_port.to_string())
        .arg("-httpd")
        .arg(web_root)
        .arg("-sslOnly=0")
        .arg("-SecurityTypes")
        .arg("None")
        .arg("-disableBasicAuth")
        .arg("-PublicIP")
        .arg("127.0.0.1")
        .arg("-Log")
        .arg("*:stderr:30")
        .kill_on_drop(true)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        Error::new(
            ErrorCode::BrowserLaunchFailed,
            format!("spawn KasmVNC Xvnc {}: {e}", bin.display()),
        )
    })?;
    let stderr_task = child.stderr.take().map(|stderr| {
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while matches!(lines.next_line().await, Ok(Some(_))) {}
        })
    });

    if let Err(e) = wait_for_x_display(display_num, Duration::from_secs(10)).await {
        let _ = child.start_kill();
        return Err(Error::new(
            ErrorCode::BrowserLaunchFailed,
            format!("KasmVNC display {display} did not become ready: {e}"),
        ));
    }
    if let Err(e) = wait_for_tcp_ready(("127.0.0.1", web_port), Duration::from_secs(10)).await {
        let _ = child.start_kill();
        return Err(Error::new(
            ErrorCode::BrowserLaunchFailed,
            format!("KasmVNC web client did not accept connections on port {web_port}: {e}"),
        ));
    }

    // Start a minimal window manager so the headful browser is auto-maximized
    // and tracks framebuffer-size changes (enables `resize=remote` dynamic
    // fit). Optional: if no WM binary is on PATH the display still works, the
    // client just falls back to scaled rendering.
    let wm_child = spawn_window_manager(&display);

    Ok(Arc::new(DisplayHandle {
        display,
        web_port,
        window_manager: wm_child.is_some(),
        _rfb_port: rfb_port,
        child: Mutex::new(child),
        wm_child: wm_child.map(Mutex::new),
        stderr_task,
    }))
}

/// Spawn a lightweight window manager (matchbox or openbox) on `display` to
/// keep the single browser window maximized. Returns `None` if none is on
/// PATH — the takeover still works, just without dynamic resize.
#[cfg(unix)]
fn spawn_window_manager(display: &str) -> Option<Child> {
    for (bin, args) in [
        ("matchbox-window-manager", &["-use_titlebar", "no"][..]),
        ("openbox", &[][..]),
    ] {
        if resolve_named_bin(bin, &None).is_err() {
            continue;
        }
        match Command::new(bin)
            .args(args)
            .env("DISPLAY", display)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => return Some(child),
            Err(_) => continue,
        }
    }
    None
}

fn resolve_kasmvnc_bin() -> Result<PathBuf, Error> {
    if let Ok(raw) = std::env::var("AFHTTP_KASMVNC_BIN") {
        let path = PathBuf::from(raw);
        if path.exists() {
            return Ok(path);
        }
    }
    resolve_named_bin("Xvnc", &None).or_else(|_| resolve_named_bin("kasmvncserver", &None))
}

fn resolve_kasmvnc_web_root() -> Result<PathBuf, Error> {
    if let Ok(raw) = std::env::var("AFHTTP_KASMVNC_WEB_ROOT") {
        let path = PathBuf::from(raw);
        if path.exists() {
            return Ok(path);
        }
    }
    for candidate in [
        "/usr/share/kasmvnc/www",
        "/usr/local/share/kasmvnc/www",
        "/opt/kasmvnc/share/kasmvnc/www",
    ] {
        let path = PathBuf::from(candidate);
        if path.exists() {
            return Ok(path);
        }
    }
    Err(Error::new(
        ErrorCode::BrowserLaunchFailed,
        "could not find KasmVNC web root; set AFHTTP_KASMVNC_WEB_ROOT",
    ))
}

#[cfg(unix)]
fn pick_display_number() -> Result<u16, Error> {
    for display in 90..200 {
        let socket = x_socket_path(display);
        if !socket.exists() {
            return Ok(display);
        }
    }
    Err(Error::new(
        ErrorCode::BrowserLaunchFailed,
        "could not find a free X display number for KasmVNC",
    ))
}

#[cfg(unix)]
async fn wait_for_x_display(display: u16, timeout: Duration) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    let socket = x_socket_path(display);
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("timed out after {timeout:?}"));
        }
        if UnixStream::connect(&socket).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(unix)]
fn x_socket_path(display: u16) -> PathBuf {
    Path::new("/tmp/.X11-unix").join(format!("X{display}"))
}

/// Minimal state used by the listener reverse proxy. Tests can construct this
/// without spawning KasmVNC; production keeps the process alive via `_handle`.
#[derive(Clone)]
pub struct DisplayProxyState {
    pub display: String,
    pub web_addr: SocketAddr,
    /// A window manager is running, so the client can use `resize=remote`
    /// (dynamic framebuffer fit) rather than the letterboxed `resize=scale`.
    pub window_manager: bool,
    /// Image quality 0-100 seeded onto the client (see `host::bootstrap`).
    pub quality: u8,
    _handle: Option<Arc<DisplayHandle>>,
}

impl DisplayProxyState {
    pub fn new(handle: Arc<DisplayHandle>, quality: u8) -> Self {
        Self {
            display: handle.display.clone(),
            web_addr: SocketAddr::from(([127, 0, 0, 1], handle.web_port)),
            window_manager: handle.window_manager,
            quality,
            _handle: Some(handle),
        }
    }

    #[cfg(any(test, feature = "host"))]
    pub fn for_tests(web_port: u16) -> Self {
        Self {
            display: ":99".to_string(),
            web_addr: SocketAddr::from(([127, 0, 0, 1], web_port)),
            window_manager: false,
            quality: 100,
            _handle: None,
        }
    }
}
