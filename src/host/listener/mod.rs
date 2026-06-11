//! Axum listener: serves the CDP-over-WS proxy, `/health`, `/capabilities`,
//! `/profile`, and (when enabled) the `/takeover/*` routes, all behind the
//! bearer-token middleware.

pub mod capabilities;
pub mod cdp_proxy;
pub mod diagnostics;
pub mod health;
pub mod profile;
pub mod recent_requests;
pub mod takeover_routes;

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime};

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::Extension;
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::Mutex as AsyncMutex;

use crate::host::bootstrap::{BrowserChoice, HealthPublic, HostArgs, ProfileChoice, Takeover};
use crate::host::browser::BrowserHandle;
use crate::host::takeover::TakeoverProxyState;
use crate::sdk::fetch::DEFAULT_NETWORK_BODY_MAX_BYTES;
use crate::shared::error::{Error, ErrorCode};

const TAKEOVER_HANDOFF_DEFAULT_TTL_S: u64 = 900;
const TAKEOVER_HANDOFF_MIN_TTL_S: u64 = 60;
const TAKEOVER_HANDOFF_MAX_TTL_S: u64 = 3600;
const TAKEOVER_HANDOFF_SCOPE: &str = "takeover";
const TAKEOVER_HANDOFF_COOKIE: &str = "afhttp_handoff";

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

/// Everything needed to relaunch the host browser under a different profile at
/// runtime: the original host args plus the resolved display string (set when a
/// real-display takeover provider is active, so the relaunched browser
/// re-attaches to the same KasmVNC display).
struct LaunchContext {
    args: HostArgs,
    display: Option<String>,
}

/// Shared application state held by the listener. Cheap to clone (Arc).
#[derive(Clone)]
pub struct AppState {
    pub started_at: Instant,
    pub token: Option<String>,
    pub takeover_enabled: bool,
    pub health_enabled: bool,
    pub health_public: HealthPublic,
    /// The currently-active browser/profile identity. One host serves one
    /// active profile at a time, but it can be switched at runtime via
    /// [`AppState::ensure_profile`] (the browser is relaunched under the new
    /// profile). Behind a lock so all request handlers see swaps.
    profile: Arc<RwLock<Option<ProfileEntry>>>,
    /// Serializes profile switches so two concurrent switch requests can't race
    /// on relaunch + lock handoff.
    switch_lock: Arc<AsyncMutex<()>>,
    /// Context for relaunching the browser on a switch. `None` for hosts that
    /// cannot switch (e.g. test states built without a real launch).
    launch_ctx: Option<Arc<LaunchContext>>,
    /// Recent-requests ring. `None` = feature disabled.
    pub recent_requests: Option<recent_requests::RecentRequests>,
    /// Real-display takeover proxy target and provider process keepalive.
    pub takeover: Option<TakeoverProxyState>,
    /// Short-lived URL capabilities for browser handoff into `/takeover/*`.
    handoffs: Arc<RwLock<HashMap<String, HandoffEntry>>>,
}

impl AppState {
    /// Snapshot the currently-active browser/profile identity. Returns an owned
    /// clone (cheap — the heavy browser handle is an `Arc`) so callers don't
    /// hold the profile lock; an in-flight request that holds this snapshot
    /// keeps its browser alive even if the host switches profiles underneath.
    pub fn get_profile(&self) -> Option<ProfileEntry> {
        self.profile
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Ensure the active profile is `name`, relaunching the browser under it if
    /// the host is currently bound to a different profile. Serialized so
    /// concurrent switches can't race. The previous profile's browser (and its
    /// on-disk profile lock) is torn down once the last in-flight request
    /// holding it finishes; an in-progress human takeover on the old profile is
    /// ended by the switch.
    pub async fn ensure_profile(&self, name: &str) -> Result<(), Error> {
        if self.get_profile().is_some_and(|p| p.name == name) {
            return Ok(());
        }
        crate::sdk::profile::paths::validate_name(name)?;
        let Some(ctx) = self.launch_ctx.clone() else {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "this host does not support runtime profile switching",
            ));
        };
        let _switch = self.switch_lock.lock().await;
        // Re-check under the switch lock: another switch may have landed first.
        if self.get_profile().is_some_and(|p| p.name == name) {
            return Ok(());
        }
        let choice = ProfileChoice::Persistent(name.to_string());
        let handle = launch_browser(&ctx.args, ctx.display.as_deref(), &choice).await?;
        let entry = ProfileEntry {
            kind: "persistent".to_string(),
            name: name.to_string(),
            handle,
        };
        // Swap in the new entry; drop the old one outside the write lock so its
        // browser/lock teardown (which waits on remaining Arc holders) doesn't
        // block readers.
        let old = self
            .profile
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .replace(entry);
        drop(old);
        Ok(())
    }
}

/// Build browser args for `profile` — injecting the takeover display when one
/// is active — and launch the browser subprocess. Shared by the initial launch
/// and runtime profile switches.
async fn launch_browser(
    args: &HostArgs,
    display: Option<&str>,
    profile: &ProfileChoice,
) -> Result<Arc<BrowserHandle>, Error> {
    let mut browser_args = args.clone();
    browser_args.profile = profile.clone();
    if let Some(display) = display {
        browser_args.display = crate::host::bootstrap::DisplayMode::Headful;
        browser_args
            .engine_envs
            .push(("DISPLAY".to_string(), display.to_string()));
        // The display provider may have no window manager, so pin the headful
        // window to fill the framebuffer. Chromium-family only; camoufox and
        // lightpanda don't take these flags. chromiumoxide's `.arg()` prepends
        // `--`, so pass these without leading dashes.
        if !matches!(
            args.browser,
            BrowserChoice::Camoufox | BrowserChoice::Lightpanda
        ) {
            browser_args
                .browser_args
                .push("window-position=0,0".to_string());
            browser_args.browser_args.push(format!(
                "window-size={},{}",
                crate::host::takeover::DISPLAY_WIDTH,
                crate::host::takeover::DISPLAY_HEIGHT
            ));
        }
    }
    Ok(Arc::new(crate::host::browser::launch(&browser_args).await?))
}

#[derive(Clone, Copy)]
struct AuthInfo {
    authenticated: bool,
}

#[derive(Clone)]
struct HandoffEntry {
    expires_at: Instant,
    expires_at_system: SystemTime,
    scope: &'static str,
}

#[derive(Clone)]
struct HandoffAuth {
    token: String,
    remaining_s: u64,
    secure_cookie: bool,
}

#[derive(Debug, Deserialize)]
struct HandoffRequest {
    #[serde(default)]
    ttl_s: Option<u64>,
    #[serde(default)]
    tab_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct HandoffResponse {
    takeover_url: String,
    takeover_url_expires_at_rfc3339: String,
    takeover_url_ttl_s: u64,
    takeover_url_scope: &'static str,
}

impl AppState {
    /// Launch the host browser and return a complete, ready-to-serve state.
    pub async fn launch(args: &HostArgs) -> Result<Self, Error> {
        let takeover = if let Takeover::On { provider } = args.takeover {
            if matches!(args.browser, BrowserChoice::Lightpanda) {
                return Err(Error::new(
                    ErrorCode::BackendUnsupported,
                    "display takeover requires a rendered browser; lightpanda has no display",
                ));
            }
            Some(crate::host::takeover::launch_provider(provider, args.display_quality).await?)
        } else {
            None
        };

        let (kind, name) = match &args.profile {
            ProfileChoice::Ephemeral => ("ephemeral".to_string(), "default".to_string()),
            ProfileChoice::Persistent(n) => ("persistent".to_string(), n.clone()),
        };
        let display = takeover.as_ref().map(|d| d.display.clone());
        let handle = launch_browser(args, display.as_deref(), &args.profile).await?;
        let entry = ProfileEntry { kind, name, handle };

        // Start recent-request subscriber if cap > 0.
        let recent = if args.recent_requests_cap > 0 {
            let ring = recent_requests::RecentRequests::new(args.recent_requests_cap);
            recent_requests::spawn_subscriber(entry.ws_url().to_string(), ring.clone());
            Some(ring)
        } else {
            None
        };

        Ok(Self {
            started_at: Instant::now(),
            token: args.token.clone(),
            takeover_enabled: args.takeover_enabled,
            health_enabled: args.health_enabled,
            health_public: args.health_public,
            profile: Arc::new(RwLock::new(Some(entry))),
            switch_lock: Arc::new(AsyncMutex::new(())),
            launch_ctx: Some(Arc::new(LaunchContext {
                args: args.clone(),
                display,
            })),
            recent_requests: recent,
            takeover,
            handoffs: Arc::new(RwLock::new(HashMap::new())),
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
    let _ = writeln!(handle, "{}", agent_first_data::output_json(&payload));
}

/// `--listen` target. `Tcp` works everywhere; `Unix` is `cfg(unix)` only.
pub enum ListenAddr {
    Tcp(SocketAddr),
    #[cfg(unix)]
    Unix(std::path::PathBuf),
}

impl AppState {
    fn issue_takeover_handoff(
        &self,
        ttl_s: u64,
        _tab_id: Option<String>,
    ) -> (String, HandoffEntry) {
        self.prune_expired_handoffs();
        let token = uuid::Uuid::new_v4().to_string();
        let ttl = Duration::from_secs(ttl_s);
        let entry = HandoffEntry {
            expires_at: Instant::now() + ttl,
            expires_at_system: SystemTime::now() + ttl,
            scope: TAKEOVER_HANDOFF_SCOPE,
        };
        self.handoffs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(token.clone(), entry.clone());
        (token, entry)
    }

    fn takeover_handoff_auth(&self, req: &axum::extract::Request) -> Option<HandoffAuth> {
        let token = query_handoff(req).or_else(|| cookie_handoff(req))?;
        let now = Instant::now();
        let entry = {
            let guard = self.handoffs.read().unwrap_or_else(|e| e.into_inner());
            guard.get(&token).cloned()
        }?;
        if entry.expires_at <= now || entry.scope != TAKEOVER_HANDOFF_SCOPE {
            self.handoffs
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&token);
            return None;
        }
        let remaining_s = entry
            .expires_at
            .saturating_duration_since(now)
            .as_secs()
            .max(1);
        Some(HandoffAuth {
            token,
            remaining_s,
            secure_cookie: request_is_https(req.headers(), req.uri()),
        })
    }

    fn prune_expired_handoffs(&self) {
        let now = Instant::now();
        self.handoffs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, entry| entry.expires_at > now);
    }
}

async fn takeover_handoff_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    axum::Json(body): axum::Json<HandoffRequest>,
) -> Response {
    if !state.takeover_enabled || state.takeover.is_none() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "code": "error",
                "error_code": "backend_unsupported",
                "error": "display takeover is not enabled",
                "retryable": false,
            })),
        )
            .into_response();
    }

    let ttl_s = body.ttl_s.unwrap_or(TAKEOVER_HANDOFF_DEFAULT_TTL_S);
    if !(TAKEOVER_HANDOFF_MIN_TTL_S..=TAKEOVER_HANDOFF_MAX_TTL_S).contains(&ttl_s) {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
            "code": "error",
            "error_code": "invalid_argument",
            "error": format!(
                "takeover handoff ttl_s must be {TAKEOVER_HANDOFF_MIN_TTL_S}-{TAKEOVER_HANDOFF_MAX_TTL_S}, got {ttl_s}"
            ),
            "retryable": false,
        })))
        .into_response();
    }

    let (token, entry) = state.issue_takeover_handoff(ttl_s, body.tab_id);
    let mut takeover_url = match takeover_panel_url_from_request(&headers, &uri) {
        Ok(url) => url,
        Err(err) => {
            let body = serde_json::json!({
                "code": "error",
                "error_code": err.error_code.as_str(),
                "error": err.detail,
                "retryable": err.retryable,
            });
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response();
        }
    };
    takeover_url
        .query_pairs_mut()
        .append_pair("handoff", &token);
    Json(HandoffResponse {
        takeover_url: takeover_url.to_string(),
        takeover_url_expires_at_rfc3339: humantime::format_rfc3339_seconds(entry.expires_at_system)
            .to_string(),
        takeover_url_ttl_s: ttl_s,
        takeover_url_scope: entry.scope,
    })
    .into_response()
}

fn takeover_panel_url_from_request(headers: &HeaderMap, uri: &Uri) -> Result<url::Url, Error> {
    let scheme = request_scheme(headers, uri);
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| Error::new(ErrorCode::InvalidEndpoint, "takeover handoff: missing Host"))?;
    url::Url::parse(&format!("{scheme}://{host}/takeover/panel")).map_err(|e| {
        Error::new(
            ErrorCode::InvalidEndpoint,
            format!("takeover handoff URL from host {host:?}: {e}"),
        )
    })
}

fn request_is_https(headers: &HeaderMap, uri: &Uri) -> bool {
    request_scheme(headers, uri) == "https"
}

fn request_scheme<'a>(headers: &'a HeaderMap, uri: &'a Uri) -> &'a str {
    if let Some(scheme) = uri.scheme_str() {
        return scheme;
    }
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|v| *v == "https")
        .unwrap_or("http")
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
        .route("/cdp", axum::routing::get(self::cdp_proxy::handler))
        .route(
            "/takeover/handoff",
            axum::routing::post(takeover_handoff_handler),
        );
    if state.takeover_enabled {
        router = router
            .route(
                "/takeover/panel",
                axum::routing::any(self::takeover_routes::display_proxy),
            )
            // The bare `/takeover/panel/` (the redirect target) must have its own
            // route: axum's `{*path}` wildcard does not match an empty segment,
            // so without this the display landing page 404s and only deep
            // asset/ws paths resolve.
            .route(
                "/takeover/panel/",
                axum::routing::any(self::takeover_routes::display_proxy),
            )
            .route(
                "/takeover/panel/{*path}",
                axum::routing::any(self::takeover_routes::display_proxy),
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
/// token was configured. Takeover display URLs use short-lived `handoff=...`
/// capabilities instead of embedding the long-lived host token.
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
        let supplied = supplied_long_token(&request, true);
        match supplied {
            Some(t) if constant_time_eq(t.as_bytes(), expected.as_bytes()) => {
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

    if is_takeover_display_path(&path) {
        if let Some(t) = bearer_token(&request) {
            if constant_time_eq(t.as_bytes(), expected.as_bytes()) {
                request.extensions_mut().insert(AuthInfo {
                    authenticated: true,
                });
                return next.run(request).await;
            }
        }
        if let Some(handoff) = state.takeover_handoff_auth(&request) {
            request.extensions_mut().insert(AuthInfo {
                authenticated: true,
            });
            let mut response = next.run(request).await;
            set_takeover_handoff_cookie(
                &mut response,
                &handoff.token,
                handoff.remaining_s,
                handoff.secure_cookie,
            );
            return response;
        }
        return unauthorized();
    }

    let supplied = supplied_long_token(&request, true);
    match supplied {
        Some(t) if constant_time_eq(t.as_bytes(), expected.as_bytes()) => {
            request.extensions_mut().insert(AuthInfo {
                authenticated: true,
            });
            next.run(request).await
        }
        _ => unauthorized(),
    }
}

fn is_takeover_display_path(path: &str) -> bool {
    path.starts_with("/takeover/") && path != "/takeover/handoff"
}

fn supplied_long_token(req: &axum::extract::Request, accept_query: bool) -> Option<String> {
    bearer_token(req).or_else(|| accept_query.then(|| query_token(req)).flatten())
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

fn query_handoff(req: &axum::extract::Request) -> Option<String> {
    let q = req.uri().query()?;
    url::form_urlencoded::parse(q.as_bytes())
        .find(|(key, _)| key == "handoff")
        .map(|(_, value)| value.into_owned())
}

fn cookie_handoff(req: &axum::extract::Request) -> Option<String> {
    let h = req.headers().get(header::COOKIE)?;
    let s = h.to_str().ok()?;
    for cookie in cookie::Cookie::split_parse(s).flatten() {
        if cookie.name() == TAKEOVER_HANDOFF_COOKIE {
            return Some(cookie.value().to_string());
        }
    }
    None
}

fn set_takeover_handoff_cookie(response: &mut Response, token: &str, max_age_s: u64, secure: bool) {
    let max_age_s = max_age_s.min(i64::MAX as u64) as i64;
    let mut builder = cookie::Cookie::build((TAKEOVER_HANDOFF_COOKIE, token.to_string()))
        .path("/takeover")
        .max_age(cookie::time::Duration::seconds(max_age_s))
        .http_only(true)
        .same_site(cookie::SameSite::Lax);
    if secure {
        builder = builder.secure(true);
    }
    let cookie = builder.build();
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
        takeover_enabled: true,
        health_enabled: true,
        health_public,
        profile: Arc::new(RwLock::new(None)),
        switch_lock: Arc::new(AsyncMutex::new(())),
        launch_ctx: None,
        recent_requests: None,
        takeover: None,
        handoffs: Arc::new(RwLock::new(HashMap::new())),
    }
}

impl AppState {
    /// Test helper: populate the default profile slot with the given
    /// `BrowserHandle`. Replaces any existing default-profile entry.
    #[cfg(any(test, feature = "host"))]
    pub fn with_default_browser(self, handle: Arc<BrowserHandle>) -> Self {
        let entry = ProfileEntry {
            kind: "ephemeral".to_string(),
            name: "default".to_string(),
            handle,
        };
        *self.profile.write().unwrap_or_else(|e| e.into_inner()) = Some(entry);
        self
    }

    /// Test helper: attach a fake display provider upstream listener.
    #[cfg(any(test, feature = "host"))]
    pub fn with_takeover_for_tests(mut self, web_port: u16) -> Self {
        self.takeover = Some(TakeoverProxyState::for_tests(web_port));
        self
    }

    /// Test helper: insert a synthetic persistent profile with a given path,
    /// backed by a fake-profile `BrowserHandle` that carries the path.
    /// Used by cookie-jar isolation tests that only exercise the HTTP path
    /// and don't need a real browser subprocess.
    #[cfg(any(test, feature = "host"))]
    pub fn with_persistent_profile(
        self,
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
        *self.profile.write().unwrap_or_else(|e| e.into_inner()) = Some(entry);
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
