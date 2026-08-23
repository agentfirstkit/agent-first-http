//! In-container human-takeover supervision.
//!
//! Takeover providers are process-level adapters behind `/takeover/panel`, each
//! a concrete screen-share method selected by `--takeover-provider` (parallel
//! to how `--browser` selects an engine). Add one by giving
//! [`TakeoverProviderKind`] a variant and a `match` arm in [`launch_provider`]
//! and [`TakeoverProxyState::proxy`]. KasmVNC is the only provider today and is
//! kept as an external GPL process: afhttp only starts `Xvnc`, waits for its X
//! display + localhost web listener, and reverse-proxies the web client from
//! the authenticated host listener.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt};
// KasmVNC takeover is Unix-only (Xvnc + a unix-socket control channel); these
// imports are used solely by the `#[cfg(unix)]` launch path below. On Windows
// `launch_kasmvnc_provider()` returns BackendUnsupported, so the rest of
// afhttp — fetch, host, takeover-panel screencast — still builds and runs.
#[cfg(unix)]
use tokio::io::AsyncBufReadExt;
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::process::Child;
#[cfg(unix)]
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite;

use crate::host::bootstrap::TakeoverProviderKind;
use crate::host::browser::{pick_ephemeral_port, resolve_named_bin, wait_for_tcp_ready};
use crate::shared::error::{Error, ErrorCode};

const APP_ICON: &str = include_str!("app-icon.svg");

/// Virtual framebuffer geometry for the takeover display. No window manager
/// runs on the X display, so the headful browser window is pinned to this size
/// (see `AppState::launch`) to fill the framebuffer.
pub const DISPLAY_WIDTH: u16 = 1280;
pub const DISPLAY_HEIGHT: u16 = 720;

/// Running KasmVNC display. Cloning the surrounding `Arc` keeps the process
/// alive until the listener state is dropped.
pub struct ProviderHandle {
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

impl Drop for ProviderHandle {
    fn drop(&mut self) {
        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
        if let Some(wm) = &self.wm_child
            && let Ok(mut child) = wm.try_lock()
        {
            let _ = child.start_kill();
        }
        if let Ok(mut child) = self.child.try_lock() {
            let _ = child.start_kill();
        }
    }
}

/// Launch a display provider and return the runtime state used by `/takeover/panel`.
pub async fn launch_provider(
    provider: TakeoverProviderKind,
    quality: u8,
) -> Result<TakeoverProxyState, Error> {
    match provider {
        TakeoverProviderKind::KasmVnc => Ok(TakeoverProxyState::new(
            provider,
            launch_kasmvnc_provider().await?,
            quality,
        )),
    }
}

/// Launch KasmVNC's `Xvnc` and wait until both the X display socket and the
/// embedded web client are reachable on localhost.
pub async fn launch_kasmvnc_provider() -> Result<Arc<ProviderHandle>, Error> {
    #[cfg(not(unix))]
    {
        return Err(Error::new(
            ErrorCode::BackendUnsupported,
            "KasmVNC display provider is only supported on Unix-like container hosts",
        ));
    }

    #[cfg(unix)]
    {
        launch_kasmvnc_unix().await
    }
}

#[cfg(unix)]
async fn launch_kasmvnc_unix() -> Result<Arc<ProviderHandle>, Error> {
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

    Ok(Arc::new(ProviderHandle {
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
pub struct TakeoverProxyState {
    pub provider: TakeoverProviderKind,
    pub display: String,
    pub web_addr: SocketAddr,
    /// A window manager is running, so the client can use `resize=remote`
    /// (dynamic framebuffer fit) rather than the letterboxed `resize=scale`.
    pub window_manager: bool,
    /// Image quality 0-100 seeded onto the client (see `host::bootstrap`).
    pub quality: u8,
    _handle: Option<Arc<ProviderHandle>>,
}

impl TakeoverProxyState {
    pub fn new(provider: TakeoverProviderKind, handle: Arc<ProviderHandle>, quality: u8) -> Self {
        Self {
            provider,
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
            provider: TakeoverProviderKind::KasmVnc,
            display: ":99".to_string(),
            web_addr: SocketAddr::from(([127, 0, 0, 1], web_port)),
            window_manager: false,
            quality: 100,
            _handle: None,
        }
    }

    /// Proxy an authenticated `/takeover/panel` request to the active provider.
    pub async fn proxy(
        self,
        ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
        uri: Uri,
        request: axum::extract::Request,
    ) -> Response {
        match self.provider {
            TakeoverProviderKind::KasmVnc => proxy_kasmvnc(self, ws, uri, request).await,
        }
    }
}

async fn proxy_kasmvnc(
    display: TakeoverProxyState,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
    uri: Uri,
    request: axum::extract::Request,
) -> Response {
    // This is the Provider identity consumed by AFUI's desktop window. It is
    // answered by afhttp rather than forwarded into the third-party display
    // client, and it remains behind the same handoff credential as the panel.
    if uri.path() == format!("/takeover/panel/{}", agent_first_ui::APP_ICON_PATH) && ws.is_err() {
        // Validated rather than served raw: the bytes reach a browser as an
        // application identity, and the one check that they are an SVG at all
        // belongs to whoever owns that meaning.
        let icon = match agent_first_ui::UiAppIcon::from_svg(APP_ICON) {
            Ok(icon) => icon,
            Err(error) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
            }
        };
        return (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "image/svg+xml"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            icon.as_svg().to_owned(),
        )
            .into_response();
    }
    // noVNC builds its WebSocket URL from the *origin root* plus its `path`
    // setting, ignoring the path of the page it is running on — so without help
    // the client connects to `/websockify` and 404s. `path` therefore has to
    // name wherever this panel is publicly served from, and the landing page is
    // where that gets seeded.
    // `resize=remote` makes the framebuffer (and the WM-maximized browser)
    // track the client window for an exact fit; without a window manager fall
    // back to `resize=scale` (letterboxed) since the browser window can't
    // follow a framebuffer resize on its own.
    let resize = if display.window_manager {
        "remote"
    } else {
        "scale"
    };
    let is_landing = matches!(uri.path(), "/takeover/panel" | "/takeover/panel/");
    let q = uri.query().unwrap_or("");
    let has_path = q.split('&').any(|p| p.starts_with("path="));
    // Match the exact preferred value, not just presence, so a client that
    // cached `resize=scale` from before a window manager was available gets
    // upgraded to `resize=remote` on its next landing hit.
    let want_resize = format!("resize={resize}");
    let resize_ok = q.split('&').any(|p| p == want_resize);
    if is_landing && ws.is_err() && !(has_path && resize_ok) {
        return landing_bootstrap(resize, display.quality, q);
    }

    let upstream_path = display_upstream_path_and_query(&uri);
    if let Ok(ws) = ws {
        let upstream = format!("ws://{}{}", display.web_addr, upstream_path);
        // KasmVNC's websockify speaks the `binary` WebSocket subprotocol and
        // closes the stream if it isn't negotiated. Echo it back to the
        // browser (it always offers `binary`) so noVNC accepts the socket;
        // the upstream leg requests it in `forward_display_ws`.
        return ws
            .protocols(["binary"])
            .on_upgrade(move |socket| forward_kasmvnc_ws(socket, upstream));
    }

    forward_display_http(display.web_addr, upstream_path, request).await
}

/// The landing page's own bootstrap: a page rather than a redirect, because
/// only the browser knows where this panel is being served from.
///
/// This used to be a `307` to `?path=takeover/panel/websockify`, which assumed
/// the panel is always reached at afhttp's own absolute path. It is not.
/// `afui session serve` frames the panel at `/s/<credential>/` on a different
/// listener so a phone can reach it, and the request that arrives here still
/// says `/takeover/panel/` — nothing in it says otherwise, because that proxy
/// deliberately forwards no `Origin`, no `Referer` and no forwarded-prefix
/// header: where a Provider UI sits is not the Provider's business. A `path`
/// seeded here would point noVNC's WebSocket at a path the framing listener has
/// never heard of, and the canvas would never connect.
///
/// So the prefix is read where it is actually known. `location.pathname` is the
/// panel's public base in whatever browser is showing it, and one line turns
/// that into the `path` setting. Delivered directly it produces exactly the
/// value that used to be hardcoded, so nothing about the window case changes;
/// delivered through any prefix at all, it produces that prefix. The trailing
/// slash is normalized here too (axum's `{*path}` wildcard never matches the
/// bare `/takeover/panel/`).
fn landing_bootstrap(resize: &str, quality: u8, query: &str) -> Response {
    // Everything the client is seeded with except `path`, which is the
    // browser's to compute. Stale `path`/`resize` and the one-time handoff
    // query are dropped, so a client that cached a URL from an older build is
    // rebuilt canonically rather than kept on it.
    let mut settings = format!("&resize={resize}{}", kasmvnc_quality_params(quality));
    for pair in query.split('&') {
        if pair.is_empty()
            || pair.starts_with("path=")
            || pair.starts_with("resize=")
            || pair.starts_with("handoff_secret=")
            || pair.starts_with("handoff=")
        {
            continue;
        }
        settings.push('&');
        settings.push_str(pair);
    }
    let body = format!(
        "<!doctype html>\n<meta charset=\"utf-8\">\n<title>afhttp takeover</title>\n\
         <noscript>The takeover panel needs JavaScript.</noscript>\n\
         <script>\n(function () {{\n\
         \x20 var path = location.pathname;\n\
         \x20 var base = path.charAt(path.length - 1) === '/' ? path : path + '/';\n\
         \x20 var websockify = base.replace(/^\\/+/, '') + 'websockify';\n\
         \x20 location.replace(base + '?path=' + encodeURIComponent(websockify) + {settings});\n\
         }})();\n</script>\n",
        settings = js_string(&settings)
    );
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

/// One JavaScript string literal, safe to paste into a `<script>` element.
///
/// The tail of the query is whatever the caller put there, so it is encoded
/// rather than concatenated. JSON string syntax is a subset of JavaScript's,
/// which handles quotes, backslashes and control characters; `<` is escaped on
/// top of that so no value can close the element it is written inside.
fn js_string(value: &str) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|_| "\"\"".to_string())
        .replace('<', "\\u003c")
}

/// KasmVNC client quality settings seeded on the display panel (the client's
/// values override the server's). The `pct` (0-100, from
/// `--takeover-quality-percent`) maps to KasmVNC's 0-9 JPEG quality tiers.
/// Static/idle content can always climb to tier 9 (`dynamic_quality_max`);
/// `pct` sets the floor for moving content. Regardless of `pct` we stop the
/// client's default 960x540 "video mode" downscale (`max_video_resolution`),
/// which is what blurs detailed images like captcha challenges while they
/// load/animate.
fn kasmvnc_quality_params(pct: u8) -> String {
    let level = (u32::from(pct.min(100)) * 9 + 50) / 100; // 0-9, rounded
    format!(
        "&quality={level}&dynamic_quality_min={level}&dynamic_quality_max=9\
         &jpeg_video_quality={level}&webp_video_quality={level}\
         &max_video_resolution_x=3840&max_video_resolution_y=2160"
    )
}

async fn forward_display_http(
    upstream: std::net::SocketAddr,
    path_and_query: String,
    request: axum::extract::Request,
) -> Response {
    let (parts, body) = request.into_parts();
    let url = format!("http://{upstream}{path_and_query}");
    let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes())
        .unwrap_or(reqwest::Method::GET);
    let body = match to_bytes(body, 64 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("display proxy: could not read request body: {e}"),
            )
                .into_response();
        }
    };

    let client = reqwest::Client::new();
    let mut upstream_req = client.request(method, url).body(body);
    for (name, value) in &parts.headers {
        if should_forward_header(name) {
            upstream_req = upstream_req.header(name, value);
        }
    }
    upstream_req = upstream_req.header(header::HOST.as_str(), upstream.to_string());

    let resp = match upstream_req.send().await {
        Ok(resp) => resp,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("display proxy: upstream request failed: {e}"),
            )
                .into_response();
        }
    };

    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut headers = HeaderMap::new();
    for (name, value) in resp.headers() {
        if should_forward_response_header(name) {
            headers.insert(name.clone(), value.clone());
        }
    }
    let bytes = match resp.bytes().await {
        Ok(bytes) => bytes,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("display proxy: upstream body failed: {e}"),
            )
                .into_response();
        }
    };

    (status, headers, Body::from(bytes)).into_response()
}

async fn forward_kasmvnc_ws(client: WebSocket, upstream_url: String) {
    use tungstenite::client::IntoClientRequest;
    let (mut client_tx, mut client_rx) = client.split();
    // Request KasmVNC's `binary` subprotocol on the upstream leg; without it
    // websockify refuses to bridge the RFB stream (101 then immediate close).
    let upstream_req = match upstream_url.as_str().into_client_request() {
        Ok(mut req) => {
            req.headers_mut().insert(
                tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL,
                tungstenite::http::HeaderValue::from_static("binary"),
            );
            // KasmVNC rejects WS upgrades that lack an `Origin` header (404),
            // so synthesize one matching the upstream authority. The browser's
            // own Origin points at afhttp's listener, not KasmVNC, so it can't
            // be forwarded verbatim.
            if let Some(authority) = req.uri().authority().map(|a| a.to_string())
                && let Ok(origin) =
                    tungstenite::http::HeaderValue::from_str(&format!("http://{authority}"))
            {
                req.headers_mut()
                    .insert(tungstenite::http::header::ORIGIN, origin);
            }
            req
        }
        Err(_) => {
            let _ = client_tx.close().await;
            return;
        }
    };
    let upstream_stream = match tokio_tungstenite::connect_async(upstream_req).await {
        Ok((stream, _resp)) => stream,
        Err(_) => {
            let _ = client_tx.close().await;
            return;
        }
    };
    let (mut upstream_tx, mut upstream_rx) = upstream_stream.split();

    let c2u = async {
        while let Some(Ok(msg)) = client_rx.next().await {
            let outbound = match msg {
                Message::Text(t) => tungstenite::Message::Text(t.as_str().into()),
                Message::Binary(b) => tungstenite::Message::Binary(b.to_vec().into()),
                Message::Ping(p) => tungstenite::Message::Ping(p.to_vec().into()),
                Message::Pong(p) => tungstenite::Message::Pong(p.to_vec().into()),
                Message::Close(_) => break,
            };
            if upstream_tx.send(outbound).await.is_err() {
                break;
            }
        }
        let _ = upstream_tx.send(tungstenite::Message::Close(None)).await;
    };
    let u2c = async {
        while let Some(Ok(msg)) = upstream_rx.next().await {
            let inbound = match msg {
                tungstenite::Message::Text(t) => Message::Text(t.as_str().into()),
                tungstenite::Message::Binary(b) => Message::Binary(b.to_vec().into()),
                tungstenite::Message::Ping(p) => Message::Ping(p.to_vec().into()),
                tungstenite::Message::Pong(p) => Message::Pong(p.to_vec().into()),
                tungstenite::Message::Close(_) => break,
                tungstenite::Message::Frame(_) => continue,
            };
            if client_tx.send(inbound).await.is_err() {
                break;
            }
        }
        let _ = client_tx.close().await;
    };
    tokio::pin!(c2u);
    tokio::pin!(u2c);
    tokio::select! {
        _ = &mut c2u => {},
        _ = &mut u2c => {},
    }
}

fn display_upstream_path_and_query(uri: &Uri) -> String {
    let suffix = uri
        .path()
        .strip_prefix("/takeover/panel")
        .filter(|s| !s.is_empty())
        .unwrap_or("/");
    let path = if suffix.starts_with('/') {
        suffix.to_string()
    } else {
        format!("/{suffix}")
    };
    let Some(query) = uri.query() else {
        return path;
    };
    let filtered: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            !pair.starts_with("handoff_secret=")
                && !pair.starts_with("handoff=")
                && !pair.starts_with("token_secret=")
        })
        .collect();
    if filtered.is_empty() {
        path
    } else {
        format!("{path}?{}", filtered.join("&"))
    }
}

fn should_forward_header(name: &header::HeaderName) -> bool {
    !matches!(
        name.as_str().to_ascii_lowercase().as_str(),
        "host"
            | "connection"
            | "upgrade"
            | "content-length"
            | "sec-websocket-key"
            | "sec-websocket-version"
            | "sec-websocket-protocol"
            | "sec-websocket-extensions"
            | "authorization"
            | "cookie"
    )
}

fn should_forward_response_header(name: &header::HeaderName) -> bool {
    !matches!(
        name.as_str().to_ascii_lowercase().as_str(),
        "connection" | "transfer-encoding" | "upgrade" | "content-length"
    )
}

#[cfg(test)]
mod tests {
    use super::{js_string, kasmvnc_quality_params};

    #[test]
    fn a_seeded_setting_cannot_close_the_script_it_is_written_in() {
        // The tail of the landing query is whatever reached the listener, so
        // the one thing it must never be able to do is escape the element.
        let escaped = js_string("&x=</script><img src=x onerror=alert(1)>");
        assert!(!escaped.contains("</script>"), "{escaped}");
        assert!(
            escaped.starts_with('"') && escaped.ends_with('"'),
            "{escaped}"
        );
        assert_eq!(js_string("&resize=scale"), "\"&resize=scale\"");
        assert_eq!(js_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }

    #[test]
    fn quality_pct_maps_to_kasmvnc_tiers() {
        assert!(kasmvnc_quality_params(100).contains("&quality=9&"));
        assert!(kasmvnc_quality_params(0).contains("&quality=0&"));
        assert!(kasmvnc_quality_params(50).contains("&quality=5&"));
        // Clamps above 100.
        assert!(kasmvnc_quality_params(200).contains("&quality=9&"));
        // Never downscales, regardless of quality.
        assert!(kasmvnc_quality_params(10).contains("max_video_resolution_x=3840"));
    }
}
