//! Fetch pipeline: HTTP-only fast path, browser-backed render path
//! (`--render none|auto|always`), and the fetch artifacts
//! (`architecture.md §8`).

pub mod artifacts;
pub(crate) mod deadline;
pub(crate) mod page_classification;
pub mod pipeline;
pub mod result;
pub mod wait;
pub mod writer;

pub type FetchCookie = cookie::Cookie<'static>;
pub use cookie::SameSite as FetchCookieSameSite;
pub use pipeline::{NetworkBodies, RenderMode};
pub use result::{FetchError, FetchResult, PageKind};
pub use wait::Wait;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use crate::sdk::client::Client;
use crate::shared::artifacts::Artifact;
use crate::shared::error::Error;
use crate::shared::ids::TabId;

/// Default per-network-response body capture cap: 10 MiB.
pub const DEFAULT_NETWORK_BODY_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// Builder for `Client::fetch(...).send().await`.
#[derive(Clone)]
pub struct FetchBuilder {
    pub(crate) client: Client,
    pub(crate) url: String,
    pub(crate) render: RenderMode,
    pub(crate) wait: Wait,
    pub(crate) timeout: Duration,
    pub(crate) want: BTreeSet<Artifact>,
    pub(crate) tab: Option<TabId>,
    /// Keep a freshly-opened ("new") target open after the fetch instead of
    /// closing it, so a human can take the tab over. No effect when an explicit
    /// `tab` is reused (those are already left open).
    pub(crate) keep_tab_open: bool,
    pub(crate) request: RequestOptions,
    pub(crate) out_dir: Option<PathBuf>,
    pub(crate) readiness: ReadinessOptions,
    pub(crate) network: NetworkCapture,
    pub(crate) retry: RetryOptions,
    pub(crate) http: HttpOptions,
    pub(crate) cookie_jar: CookieJarOptions,
}

/// Browser-path readiness tuning: how long `--wait auto` waits for network
/// quiet and DOM/text stability, plus the main-document observation cap.
#[derive(Clone)]
pub(crate) struct ReadinessOptions {
    pub(crate) idle_ms: u64,
    pub(crate) stable_ms: u64,
    pub(crate) min_text_bytes: u64,
    /// Upper bound, in milliseconds, on how long the browser path waits for
    /// the main document network event before falling back to capturing
    /// artifacts with `main_request_observed: false`. Default 500ms suits
    /// well-behaved pages on fast networks; slow networks may need more.
    pub(crate) observe_main_wait_ms: u64,
}

/// Network-capture knobs: response-body capture mode/cap, header redaction,
/// and WebSocket/SSE frame capture.
#[derive(Clone)]
pub(crate) struct NetworkCapture {
    pub(crate) bodies: NetworkBodies,
    pub(crate) body_max_bytes: u64,
    pub(crate) redact: bool,
    /// Capture WebSocket frame payloads to `network-bodies/<id>.frames.jsonl`.
    pub(crate) capture_ws: bool,
    /// Capture SSE event payloads to `network-bodies/<id>.frames.jsonl`.
    pub(crate) capture_sse: bool,
}

/// Retry policy. The fetch is retried only when the error carries
/// `retryable: true`.
#[derive(Clone)]
pub(crate) struct RetryOptions {
    /// Number of additional attempts after the first one. `0` (the default)
    /// keeps the single-attempt behavior.
    pub(crate) attempts: u32,
    /// Fixed delay between retry attempts, in milliseconds. Retry
    /// orchestration beyond a fixed interval is the agent's job; the tool
    /// just gives it the primitive.
    pub(crate) backoff_ms: u64,
}

/// HTTP fast-path transport options: upstream proxy, extra trust anchors,
/// TLS verification, and the response-body size cap.
#[derive(Clone)]
pub(crate) struct HttpOptions {
    /// Per-fetch upstream proxy for the HTTP fast path. The SDK builds a
    /// dedicated reqwest client when this (or `ca_cert` / `tls_insecure`) is
    /// set so the per-Client default reqwest is not contaminated. `None`
    /// keeps the default direct connection.
    pub(crate) proxy: Option<String>,
    /// Path to a PEM file containing extra root certificates to trust for
    /// this fetch's HTTP path. Useful for self-signed targets or corporate
    /// MITM CAs without weakening the global trust store.
    pub(crate) ca_cert: Option<PathBuf>,
    /// Disable TLS certificate verification for the HTTP path. The agent
    /// must opt in explicitly — this is dangerous and the CLI help says so.
    pub(crate) tls_insecure: bool,
    /// Upper bound, in bytes, on the HTTP fast path's response body before
    /// the pipeline stops accumulating and emits a `network_body_truncated`
    /// warning instead. Default 1 GiB — generous enough that normal pages
    /// and downloads never trip it, low enough that a pathological multi-GB
    /// download cannot OOM the host. `0` disables the cap entirely.
    pub(crate) max_response_bytes: u64,
}

/// Cookie-jar selection for the fetch.
#[derive(Clone)]
pub(crate) struct CookieJarOptions {
    /// Explicit cookie-jar path override. Normally the pipeline derives
    /// `<profile>/cookies.jar.json` from the host's `GET /profile` — setting
    /// this tells the pipeline to use the given path instead. The override
    /// must canonicalize to the host's profile directory or the pipeline
    /// rejects with `invalid_argument`.
    pub(crate) path: Option<PathBuf>,
    pub(crate) warning: Option<String>,
    /// Opt out of the cookie jar entirely for this fetch. Useful for agents
    /// that want a clean request even when the host has a persistent profile
    /// (e.g. recon traffic that should not carry authenticated session
    /// cookies).
    pub(crate) disabled: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RequestOptions {
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) user_agent: Option<String>,
    pub(crate) cookies: Vec<FetchCookie>,
    pub(crate) evaluate_after_wait: Vec<String>,
    /// HTTP method. `None` = GET (default). Uppercase recommended; the
    /// pipeline normalises before sending.
    pub(crate) method: Option<String>,
    /// Raw request body bytes (mutually exclusive with `form`).
    pub(crate) body: Option<Vec<u8>>,
    /// Form fields sent as `application/x-www-form-urlencoded` (mutually
    /// exclusive with `body`).
    pub(crate) form: Vec<(String, String)>,
}

impl FetchBuilder {
    pub(crate) fn new(client: Client, url: String) -> Self {
        Self {
            client,
            url,
            render: RenderMode::Auto,
            wait: Wait::Auto,
            timeout: Duration::from_secs(30),
            want: Artifact::ALL.iter().copied().collect(),
            tab: None,
            keep_tab_open: false,
            request: RequestOptions::default(),
            out_dir: None,
            readiness: ReadinessOptions {
                idle_ms: 800,
                stable_ms: 500,
                min_text_bytes: 32,
                observe_main_wait_ms: 500,
            },
            network: NetworkCapture {
                bodies: NetworkBodies::Off,
                body_max_bytes: DEFAULT_NETWORK_BODY_MAX_BYTES,
                redact: true,
                capture_ws: false,
                capture_sse: false,
            },
            retry: RetryOptions {
                attempts: 0,
                backoff_ms: 250,
            },
            http: HttpOptions {
                proxy: None,
                ca_cert: None,
                tls_insecure: false,
                max_response_bytes: 1_073_741_824,
            },
            cookie_jar: CookieJarOptions {
                path: None,
                warning: None,
                disabled: false,
            },
        }
    }

    #[must_use]
    pub fn render(mut self, mode: RenderMode) -> Self {
        self.render = mode;
        self
    }

    #[must_use]
    pub fn wait(mut self, w: Wait) -> Self {
        self.wait = w;
        self
    }

    #[must_use]
    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }

    #[must_use]
    pub fn readiness_idle_ms(mut self, ms: u64) -> Self {
        self.readiness.idle_ms = ms;
        self
    }

    #[must_use]
    pub fn readiness_stable_ms(mut self, ms: u64) -> Self {
        self.readiness.stable_ms = ms;
        self
    }

    #[must_use]
    pub fn readiness_min_text_bytes(mut self, bytes: u64) -> Self {
        self.readiness.min_text_bytes = bytes;
        self
    }

    #[must_use]
    pub fn want<I: IntoIterator<Item = Artifact>>(mut self, items: I) -> Self {
        self.want = items.into_iter().collect();
        self
    }

    #[must_use]
    pub fn tab(mut self, tab: TabId) -> Self {
        self.tab = Some(tab);
        self
    }

    /// Keep a freshly-opened target open after the fetch (for human takeover).
    #[must_use]
    pub fn keep_tab_open(mut self, keep: bool) -> Self {
        self.keep_tab_open = keep;
        self
    }

    #[must_use]
    pub fn network_bodies(mut self, mode: NetworkBodies) -> Self {
        self.network.bodies = mode;
        self
    }

    #[must_use]
    pub fn network_body_max_bytes(mut self, n: u64) -> Self {
        self.network.body_max_bytes = n;
        self
    }

    #[must_use]
    pub fn network_redact(mut self, on: bool) -> Self {
        self.network.redact = on;
        self
    }

    /// Add a request header. `User-Agent` is normalized to
    /// [`Self::user_agent`] at send time so browser fetches use the CDP UA
    /// override instead of a plain extra header.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.request.headers.push((name.into(), value.into()));
        self
    }

    /// Add multiple request headers.
    #[must_use]
    pub fn headers<I, K, V>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.request
            .headers
            .extend(headers.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    /// Override the browser/client user agent.
    #[must_use]
    pub fn user_agent(mut self, value: impl Into<String>) -> Self {
        self.request.user_agent = Some(value.into());
        self
    }

    /// Add a request cookie as a `name=value` pair.
    #[must_use]
    pub fn cookie(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.request
            .cookies
            .push(cookie::Cookie::new(name.into(), value.into()));
        self
    }

    /// Add a full cookie, including optional Domain/Path/Secure/HttpOnly/
    /// SameSite/Max-Age/Expires attributes.
    #[must_use]
    pub fn cookie_full(mut self, cookie: FetchCookie) -> Self {
        self.request.cookies.push(cookie);
        self
    }

    /// Add multiple request cookies as `name=value` pairs.
    #[must_use]
    pub fn cookies<I, K, V>(mut self, cookies: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.request.cookies.extend(
            cookies
                .into_iter()
                .map(|(k, v)| cookie::Cookie::new(k.into(), v.into())),
        );
        self
    }

    /// Evaluate JavaScript after the configured wait condition and before
    /// artifact capture. Only browser-backed fetches can execute scripts.
    #[must_use]
    pub fn evaluate_after_wait(mut self, js: impl Into<String>) -> Self {
        self.request.evaluate_after_wait.push(js.into());
        self
    }

    #[must_use]
    pub fn out_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.out_dir = Some(dir.into());
        self
    }

    /// Override the cookie-jar path. The default — derived from the host's
    /// `GET /profile` — places the jar at `<profile-dir>/cookies.jar.json`,
    /// which is the only path the isolation invariant permits. This
    /// override exists for tests and forensic tooling; the pipeline
    /// canonicalizes the given path and rejects it with `invalid_argument`
    /// if it doesn't match the host's profile directory.
    #[must_use]
    pub fn cookie_jar(mut self, path: impl Into<PathBuf>) -> Self {
        self.cookie_jar.path = Some(path.into());
        self
    }

    /// Opt out of cookie-jar persistence for this fetch. The request goes
    /// out without any session cookies the jar might hold, and the
    /// response's `Set-Cookie` headers are not merged back.
    #[must_use]
    pub fn no_cookie_jar(mut self) -> Self {
        self.cookie_jar.disabled = true;
        self
    }

    /// Upper bound on the browser-path wait for the main document
    /// network event, in milliseconds. Default 500ms is tuned for
    /// well-behaved pages on fast networks; raise for slow networks
    /// or low-end machines.
    #[must_use]
    pub fn observe_main_wait_ms(mut self, ms: u64) -> Self {
        self.readiness.observe_main_wait_ms = ms;
        self
    }

    /// Upper bound on the HTTP-path response body, in bytes. Default
    /// 1 GiB. `0` disables the cap entirely. When the cap is hit, the
    /// fetch returns successfully with a `network_body_truncated`
    /// warning and the prefix bytes that were collected.
    #[must_use]
    pub fn max_response_bytes(mut self, bytes: u64) -> Self {
        self.http.max_response_bytes = bytes;
        self
    }

    /// Number of additional attempts after the first. `0` (default)
    /// keeps the single-attempt behavior. Retries only fire when the
    /// pipeline error has `retryable: true`.
    #[must_use]
    pub fn retry(mut self, n: u32) -> Self {
        self.retry.attempts = n;
        self
    }

    /// Fixed delay between retry attempts, in milliseconds.
    #[must_use]
    pub fn backoff_ms(mut self, ms: u64) -> Self {
        self.retry.backoff_ms = ms;
        self
    }

    /// Per-fetch upstream proxy URL for the HTTP path. Format:
    /// `http://user:pass@host:port` or `socks5://host:port`. The SDK
    /// never honors `HTTP_PROXY`/`HTTPS_PROXY` from the environment;
    /// this method is the only way to route an HTTP-path fetch
    /// through a proxy.
    #[must_use]
    pub fn proxy(mut self, url: impl Into<String>) -> Self {
        self.http.proxy = Some(url.into());
        self
    }

    /// Path to a PEM file containing extra root CAs to trust for
    /// this fetch's HTTP path. Stacks on top of the platform trust
    /// store; does not replace it.
    #[must_use]
    pub fn ca_cert(mut self, path: impl Into<PathBuf>) -> Self {
        self.http.ca_cert = Some(path.into());
        self
    }

    /// Disable TLS certificate verification for this fetch's HTTP
    /// path. Dangerous — leaves the connection open to MITM. Use
    /// only against known-self-signed staging environments.
    #[must_use]
    pub fn tls_insecure(mut self, on: bool) -> Self {
        self.http.tls_insecure = on;
        self
    }

    /// HTTP method. Defaults to `GET`. Pass `"POST"`, `"PUT"`, etc.
    #[must_use]
    pub fn method(mut self, m: impl Into<String>) -> Self {
        self.request.method = Some(m.into());
        self
    }

    /// Raw request body. Mutually exclusive with [`Self::form_field`].
    /// Sets the body bytes as-is; add `Content-Type` via
    /// [`Self::header`] when needed.
    #[must_use]
    pub fn body(mut self, data: impl Into<Vec<u8>>) -> Self {
        self.request.body = Some(data.into());
        self
    }

    /// Add a form field. Mutually exclusive with [`Self::body`]. The
    /// pipeline sends the fields as `application/x-www-form-urlencoded`
    /// and sets the content-type header automatically.
    #[must_use]
    pub fn form_field(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.request.form.push((key.into(), value.into()));
        self
    }

    /// Capture WebSocket frame payloads to
    /// `network-bodies/<request_id>.frames.jsonl` during the browser path.
    #[must_use]
    pub fn capture_ws(mut self, on: bool) -> Self {
        self.network.capture_ws = on;
        self
    }

    /// Capture SSE event payloads to
    /// `network-bodies/<request_id>.frames.jsonl` during the browser path.
    #[must_use]
    pub fn capture_sse(mut self, on: bool) -> Self {
        self.network.capture_sse = on;
        self
    }

    /// Execute the fetch, with retries when configured. Retries only
    /// fire for errors carrying `retryable: true`; any other error
    /// short-circuits immediately.
    pub async fn send(self) -> Result<FetchResult, Error> {
        self.send_detailed().await.map_err(FetchError::into_error)
    }

    /// Execute the fetch and preserve the fetch trace on failure.
    ///
    /// This is what the CLI uses to emit `code: "error"` envelopes with a
    /// fetch-local `trace` without adding trace fields to the global `Error`.
    pub async fn send_detailed(self) -> Result<FetchResult, FetchError> {
        if self.retry.attempts == 0 {
            return execute_once_with_timeout(self).await;
        }
        let max_attempts = self.retry.attempts.saturating_add(1);
        let delay = std::time::Duration::from_millis(self.retry.backoff_ms);
        let mut attempt: u32 = 0;
        loop {
            match execute_once_with_timeout(self.clone()).await {
                Ok(r) => return Ok(r),
                Err(e) if e.retryable && attempt + 1 < max_attempts => {
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

async fn execute_once_with_timeout(builder: FetchBuilder) -> Result<FetchResult, FetchError> {
    let timeout = builder.timeout;
    let render_mode = builder.render.as_trace();
    let deadline = deadline::FetchDeadline::new(timeout, render_mode);
    match tokio::time::timeout(timeout, pipeline::execute(builder, deadline.clone())).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) => Err(FetchError::new(error, deadline.snapshot())),
        Err(_) => {
            let error = deadline.timeout_error();
            Err(FetchError::new(error, deadline.snapshot()))
        }
    }
}
