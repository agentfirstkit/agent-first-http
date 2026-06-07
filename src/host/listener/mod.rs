//! Axum listener: serves the CDP-over-WS proxy, `/health`, `/capabilities`,
//! `/profile`, and (when enabled) the `/ops/*` takeover routes, all behind the
//! bearer-token middleware.

pub mod capabilities;
pub mod cdp_proxy;
pub mod diagnostics;
pub mod health;
pub mod ops_routes;
pub mod profile;
pub mod recent_requests;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::Extension;
use axum::Router;
use tokio::net::TcpListener;

use crate::host::bootstrap::{BrowserChoice, HealthPublic, HostArgs, ProfileChoice, Takeover};
use crate::host::browser::BrowserHandle;
use crate::host::display::DisplayProxyState;
use crate::sdk::fetch::DEFAULT_NETWORK_BODY_MAX_BYTES;
use crate::shared::error::{Error, ErrorCode};

/// A single running profile with its browser handle.
#[derive(Clone)]
pub struct ProfileEntry {
    pub kind: String,
    pub name: String,
    pub handle: Arc<BrowserHandle>,
}

impl ProfileEntry {
    pub fn profile_path(&self) -> &std::path::PathBuf {
        &self.handle.profile_path
    }
    pub fn ws_url(&self) -> &str {
        &self.handle.ws_url
    }
}

/// Shared application state held by the listener. Cheap to clone (Arc).
#[derive(Clone)]
pub struct AppState {
    pub started_at: Instant,
    pub token: Option<String>,
    pub ops_enabled: bool,
    pub health_enabled: bool,
    pub health_public: HealthPublic,
    /// The single browser/profile identity bound to this host.
    pub profile: Option<ProfileEntry>,
    /// Recent-requests ring. `None` = feature disabled.
    pub recent_requests: Option<recent_requests::RecentRequests>,
    /// Real-display takeover proxy target and provider process keepalive.
    pub display_takeover: Option<DisplayProxyState>,
}

impl AppState {
    /// Return the single browser/profile identity bound to this host.
    pub fn get_profile(&self) -> Option<&ProfileEntry> {
        self.profile.as_ref()
    }
}

#[derive(Clone, Copy)]
struct AuthInfo {
    authenticated: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TokenSource {
    Bearer,
    Query,
    Cookie,
}

impl AppState {
    /// Launch the host browser and return a complete, ready-to-serve state.
    pub async fn launch(args: &HostArgs) -> Result<Self, Error> {
        let display_takeover = if let Takeover::Display { provider } = args.takeover {
            if matches!(args.browser, BrowserChoice::Lightpanda) {
                return Err(Error::new(
                    ErrorCode::BackendUnsupported,
                    "display takeover requires a rendered browser; lightpanda has no display",
                ));
            }
            Some(
                crate::host::display::launch_display_provider(provider, args.display_quality)
                    .await?,
            )
        } else {
            None
        };

        let name = match &args.profile {
            ProfileChoice::Ephemeral => "default".to_string(),
            ProfileChoice::Persistent(n) => n.clone(),
        };
        let kind = match &args.profile {
            ProfileChoice::Ephemeral => "ephemeral".to_string(),
            ProfileChoice::Persistent(_) => "persistent".to_string(),
        };
        let mut browser_args = args.clone();
        if let Some(display) = display_takeover.as_ref().map(|d| d.display.clone()) {
            browser_args.display = crate::host::bootstrap::DisplayMode::Headful;
            browser_args
                .engine_envs
                .push(("DISPLAY".to_string(), display));
            // The current display provider may have no window manager, so the headful
            // browser opens at its default size and floats in a corner of
            // the framebuffer. Pin the window to fill the display geometry
            // so the page uses the full width. Chromium-family only;
            // camoufox/lightpanda don't take these flags.
            if !matches!(
                args.browser,
                BrowserChoice::Camoufox | BrowserChoice::Lightpanda
            ) {
                // chromiumoxide's `.arg()` prepends `--`, so pass these
                // without leading dashes (a `--`-prefixed value becomes the
                // unrecognized `----window-size` and is silently ignored).
                browser_args
                    .browser_args
                    .push("window-position=0,0".to_string());
                browser_args.browser_args.push(format!(
                    "window-size={},{}",
                    crate::host::display::DISPLAY_WIDTH,
                    crate::host::display::DISPLAY_HEIGHT
                ));
            }
        }
        let handle = Arc::new(crate::host::browser::launch(&browser_args).await?);
        let profile = Some(ProfileEntry { kind, name, handle });

        // Start recent-request subscriber if cap > 0.
        let recent = if args.recent_requests_cap > 0 {
            let ring = recent_requests::RecentRequests::new(args.recent_requests_cap);
            if let Some(entry) = profile.as_ref() {
                recent_requests::spawn_subscriber(entry.ws_url().to_string(), ring.clone());
            }
            Some(ring)
        } else {
            None
        };

        Ok(Self {
            started_at: Instant::now(),
            token: args.token.clone(),
            ops_enabled: args.ops_enabled,
            health_enabled: args.health_enabled,
            health_public: args.health_public,
            profile,
            recent_requests: recent,
            display_takeover,
        })
    }

    /// Bind the listener and run the axum app until the shutdown signal
    /// fires. Supports `tcp:host:port` everywhere; `unix:/path/to.sock`
    /// only on `cfg(unix)`.
    pub async fn serve(self, listen_addr: &str) -> Result<(), Error> {
        let app = build_router(self.clone());
        match parse_listen(listen_addr)? {
            ListenAddr::Tcp(addr) => serve_tcp(app, addr).await,
            #[cfg(unix)]
            ListenAddr::Unix(path) => serve_unix(app, path).await,
        }
    }
}

async fn serve_tcp(app: Router, addr: SocketAddr) -> Result<(), Error> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| Error::new(ErrorCode::IoError, format!("listener bind {addr}: {e}")))?;
    let actual_addr = listener
        .local_addr()
        .map_err(|e| Error::new(ErrorCode::IoError, format!("local_addr: {e}")))?;
    emit_host_ready(&format!("tcp:{actual_addr}"));
    let shutdown = shutdown_signal();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|e| Error::new(ErrorCode::IoError, format!("listener serve: {e}")))
}

#[cfg(unix)]
async fn serve_unix(app: Router, path: std::path::PathBuf) -> Result<(), Error> {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::server::conn::auto;
    use tokio::net::UnixListener;
    use tower::Service;

    // Best-effort: remove a stale socket file so binding doesn't fail with
    // EADDRINUSE after a crashed host left one behind.
    let _ = std::fs::remove_file(&path);

    let listener = UnixListener::bind(&path).map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("UnixListener bind {}: {e}", path.display()),
        )
    })?;
    emit_host_ready(&format!("unix:{}", path.display()));

    let path_for_cleanup = path.clone();
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    let mut make_service = app.into_make_service();

    loop {
        let accepted = tokio::select! {
            biased;
            _ = &mut shutdown => break,
            r = listener.accept() => r,
        };
        let (stream, _peer) = match accepted {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        let tower_service = match futures::future::poll_fn(|cx| {
            <_ as Service<axum::http::Request<axum::body::Body>>>::poll_ready(&mut make_service, cx)
        })
        .await
        {
            Ok(()) => make_service.call(()).await.map_err(|_| ()).ok(),
            Err(_) => None,
        };
        let Some(tower_service) = tower_service else {
            continue;
        };
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let hyper_service = hyper::service::service_fn(
                move |req: axum::http::Request<hyper::body::Incoming>| {
                    let mut tower_service = tower_service.clone();
                    async move {
                        let (parts, body) = req.into_parts();
                        let req =
                            axum::http::Request::from_parts(parts, axum::body::Body::new(body));
                        <_ as Service<axum::http::Request<axum::body::Body>>>::call(
                            &mut tower_service,
                            req,
                        )
                        .await
                    }
                },
            );
            let _ = auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(io, hyper_service)
                .await;
        });
    }

    let _ = std::fs::remove_file(&path_for_cleanup);
    Ok(())
}

fn emit_host_ready(listen: &str) {
    use std::io::Write;
    let payload = serde_json::json!({
        "code": "host_ready",
        "listen": listen,
        "version": env!("CARGO_PKG_VERSION"),
    });
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let _ = writeln!(handle, "{payload}");
}

/// `--listen` target. `Tcp` works everywhere; `Unix` is `cfg(unix)` only.
pub enum ListenAddr {
    Tcp(SocketAddr),
    #[cfg(unix)]
    Unix(std::path::PathBuf),
}

pub fn build_router(state: AppState) -> Router {
    let mut router = Router::new()
        .route("/health", axum::routing::get(health_handler))
        .route("/capabilities", axum::routing::get(capabilities_handler))
        .route("/profile", axum::routing::get(self::profile::handler))
        .route(
            "/recent-requests",
            axum::routing::get(self::recent_requests::handler),
        )
        .route(
            "/diagnostics",
            axum::routing::get(self::diagnostics::handler),
        )
        .route("/cdp", axum::routing::get(self::cdp_proxy::handler));
    if state.ops_enabled {
        router = router
            .route(
                "/ops/screencast",
                axum::routing::get(self::ops_routes::screencast_entry),
            )
            .route(
                "/ops/screencast/assets/app.js",
                axum::routing::get(self::ops_routes::js),
            )
            .route(
                "/ops/screencast/assets/app.css",
                axum::routing::get(self::ops_routes::css),
            )
            .route(
                "/ops/screencast/ws",
                axum::routing::get(self::ops_routes::screencast_route),
            )
            .route(
                "/ops/screencast/input",
                axum::routing::get(self::ops_routes::input_route),
            )
            .route(
                "/ops/display",
                axum::routing::any(self::ops_routes::display_proxy),
            )
            // The bare `/ops/display/` (the redirect target) must have its own
            // route: axum's `{*path}` wildcard does not match an empty segment,
            // so without this the display landing page 404s and only deep
            // asset/ws paths resolve.
            .route(
                "/ops/display/",
                axum::routing::any(self::ops_routes::display_proxy),
            )
            .route(
                "/ops/display/{*path}",
                axum::routing::any(self::ops_routes::display_proxy),
            );
    }
    router
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state, token_middleware))
}

pub fn parse_listen(spec: &str) -> Result<ListenAddr, Error> {
    if let Some(rest) = spec.strip_prefix("tcp:") {
        let addr = rest.parse::<SocketAddr>().map_err(|e| {
            Error::new(
                ErrorCode::InvalidArgument,
                format!("--listen tcp:: invalid address {rest:?}: {e}"),
            )
        })?;
        Ok(ListenAddr::Tcp(addr))
    } else if let Some(path) = spec.strip_prefix("unix:") {
        #[cfg(unix)]
        {
            if path.is_empty() {
                return Err(Error::new(
                    ErrorCode::InvalidArgument,
                    "--listen unix: requires a path (e.g. unix:/run/afhttp.sock)",
                ));
            }
            Ok(ListenAddr::Unix(std::path::PathBuf::from(path)))
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Err(Error::new(
                ErrorCode::InvalidArgument,
                "--listen unix: is not supported on this platform; use tcp:host:port",
            ))
        }
    } else {
        Err(Error::new(
            ErrorCode::InvalidArgument,
            format!("--listen: expected tcp:host:port or unix:/path (got {spec:?})"),
        ))
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
}

/// Bearer-token middleware. `--health-public=minimal` allows GET /health
/// through without a token (returning the reduced payload); everything else
/// requires `Authorization: Bearer <token>` or `?token_secret=<token>` when a
/// token was configured.
async fn token_middleware(
    State(state): State<AppState>,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
    let Some(expected) = state.token.as_deref() else {
        request.extensions_mut().insert(AuthInfo {
            authenticated: false,
        });
        return next.run(request).await;
    };
    let path = request.uri().path().to_string();
    let public_health = path == "/health" && matches!(state.health_public, HealthPublic::Minimal);
    if public_health {
        let supplied = supplied_token(&request, false);
        match supplied {
            Some((t, _source)) if constant_time_eq(t.as_bytes(), expected.as_bytes()) => {
                request.extensions_mut().insert(AuthInfo {
                    authenticated: true,
                });
                return next.run(request).await;
            }
            Some(_) => return unauthorized(),
            None => {
                request.extensions_mut().insert(AuthInfo {
                    authenticated: false,
                });
                return next.run(request).await;
            }
        }
    }
    let supplied = supplied_token(&request, path.starts_with("/ops"));
    match supplied {
        Some((t, source)) if constant_time_eq(t.as_bytes(), expected.as_bytes()) => {
            request.extensions_mut().insert(AuthInfo {
                authenticated: true,
            });
            let mut response = next.run(request).await;
            if source == TokenSource::Query && path.starts_with("/ops") {
                set_ops_token_cookie(&mut response, expected);
            }
            response
        }
        _ => unauthorized(),
    }
}

fn supplied_token(
    req: &axum::extract::Request,
    accept_ops_cookie: bool,
) -> Option<(String, TokenSource)> {
    let token = bearer_token(req)
        .map(|t| (t, TokenSource::Bearer))
        .or_else(|| query_token(req).map(|t| (t, TokenSource::Query)));
    if accept_ops_cookie {
        token.or_else(|| cookie_token(req).map(|t| (t, TokenSource::Cookie)))
    } else {
        token
    }
}

fn bearer_token(req: &axum::extract::Request) -> Option<String> {
    let h = req.headers().get(header::AUTHORIZATION)?;
    let s = h.to_str().ok()?;
    let rest = s
        .strip_prefix("Bearer ")
        .or_else(|| s.strip_prefix("bearer "))?;
    Some(rest.to_string())
}

fn query_token(req: &axum::extract::Request) -> Option<String> {
    let q = req.uri().query()?;
    url::form_urlencoded::parse(q.as_bytes())
        .find(|(key, _)| key == "token_secret")
        .map(|(_, value)| value.into_owned())
}

fn cookie_token(req: &axum::extract::Request) -> Option<String> {
    let h = req.headers().get(header::COOKIE)?;
    let s = h.to_str().ok()?;
    for cookie in cookie::Cookie::split_parse(s).flatten() {
        if cookie.name() == "afhttp_token" {
            return Some(cookie.value().to_string());
        }
    }
    None
}

fn set_ops_token_cookie(response: &mut Response, token: &str) {
    let cookie = cookie::Cookie::build(("afhttp_token", token.to_string()))
        .path("/ops")
        .http_only(true)
        .same_site(cookie::SameSite::Lax)
        .build();
    if let Ok(value) = header::HeaderValue::from_str(&cookie.to_string()) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn unauthorized() -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::WWW_AUTHENTICATE,
        "Bearer realm=\"afhttp\"".parse().unwrap_or_else(|_| {
            "Bearer"
                .parse()
                .unwrap_or_else(|_| header::HeaderValue::from_static(""))
        }),
    );
    let body = serde_json::json!({
        "code": "error",
        "error_code": "invalid_argument",
        "error": "missing or invalid bearer token",
        "retryable": false,
    });
    (StatusCode::UNAUTHORIZED, headers, Json(body)).into_response()
}

async fn health_handler(
    State(state): State<AppState>,
    auth: Option<Extension<AuthInfo>>,
) -> Response {
    if !state.health_enabled {
        return (StatusCode::NOT_FOUND, "disabled").into_response();
    }
    let authenticated = auth
        .map(|Extension(info)| info.authenticated)
        .unwrap_or(false);
    let payload = self::health::build(&state, authenticated).await;
    Json(payload).into_response()
}

async fn capabilities_handler(State(state): State<AppState>) -> Response {
    let payload = self::capabilities::build(&state);
    Json(payload).into_response()
}

/// Public test helper. Used by tests/health_capabilities.rs.
pub fn router_for_tests(state: AppState) -> Router {
    build_router(state)
}

/// Construct an [`AppState`] for tests without needing a full [`HostArgs`].
/// The profile slot is empty — tests that need a browser must call
/// [`AppState::with_default_browser`] on the result.
pub fn test_state(token: Option<&str>, health_public: HealthPublic) -> AppState {
    AppState {
        started_at: Instant::now(),
        token: token.map(str::to_string),
        ops_enabled: true,
        health_enabled: true,
        health_public,
        profile: None,
        recent_requests: None,
        display_takeover: None,
    }
}

impl AppState {
    /// Test helper: populate the default profile slot with the given
    /// `BrowserHandle`. Replaces any existing default-profile entry.
    #[cfg(any(test, feature = "host"))]
    pub fn with_default_browser(mut self, handle: Arc<BrowserHandle>) -> Self {
        let entry = ProfileEntry {
            kind: "ephemeral".to_string(),
            name: "default".to_string(),
            handle,
        };
        self.profile = Some(entry);
        self
    }

    /// Test helper: attach a fake display provider upstream listener.
    #[cfg(any(test, feature = "host"))]
    pub fn with_display_takeover_for_tests(mut self, web_port: u16) -> Self {
        self.display_takeover = Some(DisplayProxyState::for_tests(web_port));
        self
    }

    /// Test helper: insert a synthetic persistent profile with a given path,
    /// backed by a fake-profile `BrowserHandle` that carries the path.
    /// Used by cookie-jar isolation tests that only exercise the HTTP path
    /// and don't need a real browser subprocess.
    #[cfg(any(test, feature = "host"))]
    pub fn with_persistent_profile(
        mut self,
        name: impl Into<String>,
        profile_path: std::path::PathBuf,
    ) -> Self {
        let n = name.into();
        let handle = Arc::new(BrowserHandle::synthetic(profile_path));
        let entry = ProfileEntry {
            kind: "persistent".to_string(),
            name: n.clone(),
            handle,
        };
        self.profile = Some(entry);
        self
    }
}

// Re-export the synthetic limits map so the capabilities builder can hand
// it back without importing serde_json everywhere.
pub(crate) fn default_limits() -> BTreeMap<String, serde_json::Value> {
    let mut m = BTreeMap::new();
    m.insert(
        "network_body_max_bytes_default".into(),
        serde_json::Value::from(DEFAULT_NETWORK_BODY_MAX_BYTES),
    );
    m
}
