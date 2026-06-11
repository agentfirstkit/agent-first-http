//! The SDK entry point.
//!
//! `Client::connect(endpoint)` does not open the WebSocket up front — CDP
//! connections are lazy and cached per Client. The Client holds the parsed
//! endpoint, the optional bearer token, an HTTP client for the `/health` +
//! `/capabilities` calls, and one reusable CDP connection.

use std::sync::Arc;
use std::sync::OnceLock;

use tokio::sync::Mutex;

use crate::sdk::cdp::ws_client::Connection;
use crate::sdk::endpoint::Endpoint;
use crate::shared::error::{Error, ErrorCode};

/// Installs the aws-lc-rs rustls provider once per process. reqwest's
/// `rustls-tls-no-provider` feature relies on the caller to do this; if it
/// isn't done the first `Client::builder().build()` call errors.
fn ensure_rustls_provider() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

/// Top-level client. Cheap to clone (everything inside is `Arc`-shared).
#[derive(Clone)]
pub struct Client {
    inner: Arc<ClientInner>,
}

pub(crate) struct ClientInner {
    pub(crate) endpoint: Endpoint,
    pub(crate) token: Option<String>,
    /// Optional profile selector passed to the host on the `/cdp` connection so
    /// the host switches its active profile (per-domain isolation).
    pub(crate) profile: Option<String>,
    pub(crate) http: reqwest::Client,
    pub(crate) hostless: bool,
    pub(crate) inline_host: Option<crate::sdk::inline::InlineHost>,
    cdp: Mutex<Option<Arc<Connection>>>,
    profile_info: Mutex<Option<Arc<crate::sdk::profile::info::ProfileInfo>>>,
}

impl Client {
    /// Parse an endpoint string and return a `Client` bound to it. No network
    /// I/O — that happens on the first fetch / health / capabilities call.
    pub fn connect(endpoint: &str) -> Result<Self, Error> {
        ensure_rustls_provider();
        let endpoint = Endpoint::parse(endpoint)?;
        let mut http_builder = reqwest::Client::builder()
            .user_agent(concat!("afhttp/", env!("CARGO_PKG_VERSION")))
            // Isolation invariant: never honor `HTTP_PROXY` / `HTTPS_PROXY`
            // from the environment. Per-fetch `--proxy-url` is the only opt-in.
            .no_proxy();
        #[cfg(unix)]
        if let Endpoint::Unix { path } = &endpoint {
            http_builder = http_builder.unix_socket(path.clone());
        }
        let http = http_builder.build().map_err(|e| {
            Error::new(
                ErrorCode::InternalError,
                format!("reqwest client build failed: {e}"),
            )
        })?;
        Ok(Self {
            inner: Arc::new(ClientInner {
                endpoint,
                token: None,
                profile: None,
                http,
                hostless: false,
                inline_host: None,
                cdp: Mutex::new(None),
                profile_info: Mutex::new(None),
            }),
        })
    }

    /// Build a lightweight client for HTTP-only fetches. It has no host
    /// endpoint and never probes `/profile`; browser-backed operations return
    /// a structured unavailable error.
    pub fn http_only() -> Result<Self, Error> {
        let mut client = Self::connect("ws://127.0.0.1:0")?;
        if let Some(inner) = Arc::get_mut(&mut client.inner) {
            inner.hostless = true;
        }
        Ok(client)
    }

    /// Attach a bearer token; sent as `Authorization: Bearer <token>` on
    /// HTTP requests and as `?token_secret=<token>` on the CDP WebSocket upgrade.
    #[must_use]
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        let token = token.into();
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.token = Some(token);
            inner.cdp = Mutex::new(None);
            inner.profile_info = Mutex::new(None);
        } else {
            // Cloned already; rebuild a fresh inner.
            let new = ClientInner {
                endpoint: self.inner.endpoint.clone(),
                token: Some(token),
                profile: self.inner.profile.clone(),
                http: self.inner.http.clone(),
                hostless: self.inner.hostless,
                inline_host: self.inner.inline_host.clone(),
                cdp: Mutex::new(None),
                profile_info: Mutex::new(None),
            };
            self.inner = Arc::new(new);
        }
        self
    }

    /// Bind this client to a host profile. The name is sent on the `/cdp`
    /// connection so the host switches its active profile before serving.
    #[must_use]
    pub fn with_profile(mut self, profile: impl Into<String>) -> Self {
        let profile = profile.into();
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.profile = Some(profile);
            inner.cdp = Mutex::new(None);
        } else {
            let new = ClientInner {
                endpoint: self.inner.endpoint.clone(),
                token: self.inner.token.clone(),
                profile: Some(profile),
                http: self.inner.http.clone(),
                hostless: self.inner.hostless,
                inline_host: self.inner.inline_host.clone(),
                cdp: Mutex::new(None),
                profile_info: Mutex::new(None),
            };
            self.inner = Arc::new(new);
        }
        self
    }

    /// Optional host profile selector.
    #[must_use]
    pub fn profile(&self) -> Option<&str> {
        self.inner.profile.as_deref()
    }

    /// The endpoint this client points at.
    #[must_use]
    pub fn endpoint(&self) -> &Endpoint {
        &self.inner.endpoint
    }

    /// Optional bearer token.
    #[must_use]
    pub fn token(&self) -> Option<&str> {
        self.inner.token.as_deref()
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }

    pub(crate) fn is_hostless(&self) -> bool {
        self.inner.hostless
    }

    pub(crate) fn has_inline_host(&self) -> bool {
        self.inner.inline_host.is_some()
    }

    pub(crate) async fn inline_host_started(&self) -> bool {
        match &self.inner.inline_host {
            Some(inline) => inline.is_started().await,
            None => false,
        }
    }

    #[cfg(feature = "host")]
    pub(crate) fn with_inline_host(mut self, inline_host: crate::sdk::inline::InlineHost) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.inline_host = Some(inline_host);
            inner.hostless = false;
            inner.cdp = Mutex::new(None);
            inner.profile_info = Mutex::new(None);
        } else {
            let new = ClientInner {
                endpoint: self.inner.endpoint.clone(),
                token: self.inner.token.clone(),
                profile: self.inner.profile.clone(),
                http: self.inner.http.clone(),
                hostless: false,
                inline_host: Some(inline_host),
                cdp: Mutex::new(None),
                profile_info: Mutex::new(None),
            };
            self.inner = Arc::new(new);
        }
        self
    }

    pub(crate) async fn effective_endpoint(&self) -> Result<Endpoint, Error> {
        if self.inner.hostless {
            return Err(Error::new(
                ErrorCode::RenderUnavailable,
                "this client has no afhttp host endpoint",
            ));
        }
        if let Some(inline) = &self.inner.inline_host {
            inline.endpoint().await
        } else {
            Ok(self.inner.endpoint.clone())
        }
    }

    /// Return the cached CDP connection, opening it lazily on first use.
    pub(crate) async fn cdp_connection(&self) -> Result<Arc<Connection>, Error> {
        let mut guard = self.inner.cdp.lock().await;
        if let Some(conn) = guard.as_ref() {
            return Ok(conn.clone());
        }
        let endpoint = self.effective_endpoint().await?;
        let conn =
            Arc::new(Connection::connect_endpoint(&endpoint, self.token(), self.profile()).await?);
        *guard = Some(conn.clone());
        Ok(conn)
    }

    /// Close the cached CDP connection, if one has been opened. The next
    /// `fetch` or `cdp` call reconnects lazily.
    pub async fn close(&self) {
        if let Some(conn) = self.inner.cdp.lock().await.take() {
            conn.close();
        }
    }

    /// Build a fetch request. Sending the request actually performs the
    /// fetch — `Client::fetch(...).send().await`.
    #[must_use]
    pub fn fetch(&self, url: impl Into<String>) -> crate::sdk::fetch::FetchBuilder {
        crate::sdk::fetch::FetchBuilder::new(self.clone(), url.into())
    }

    /// Fetch (and cache) the host's profile info from `GET /profile`. The
    /// pipeline calls this to derive the canonical cookie-jar path
    /// (`<profile>/cookies.jar.json`) and to validate any explicit
    /// `--cookie-jar` override against the host's actual profile dir, so
    /// agents cannot accidentally redirect another profile's session into
    /// their own jar.
    pub async fn profile_info(&self) -> Result<Arc<crate::sdk::profile::info::ProfileInfo>, Error> {
        let mut guard = self.inner.profile_info.lock().await;
        if let Some(info) = guard.as_ref() {
            return Ok(info.clone());
        }
        if self.inner.hostless {
            return Err(Error::new(
                ErrorCode::HostUnreachable,
                "GET /profile: no afhttp host endpoint configured",
            ));
        }
        let endpoint = self.effective_endpoint().await?;
        let base = endpoint.http_base();
        let url = profile_url(&base)?;
        let mut req = self.http().get(&url);
        if let Some(token) = self.token() {
            req = req.bearer_auth(token);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::new(ErrorCode::HostUnreachable, format!("GET {url}: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Error::new(
                ErrorCode::HostUnreachable,
                format!("GET {url}: status {status}"),
            ));
        }
        let info: crate::sdk::profile::info::ProfileInfo = resp.json().await.map_err(|e| {
            Error::new(
                ErrorCode::InternalError,
                format!("profile_info: decode response: {e}"),
            )
        })?;
        let arc = Arc::new(info);
        *guard = Some(arc.clone());
        Ok(arc)
    }
}

fn profile_url(base: &str) -> Result<String, Error> {
    let url = url::Url::parse(&format!("{base}/profile")).map_err(|e| {
        Error::new(
            ErrorCode::InvalidEndpoint,
            format!("profile URL from endpoint {base:?}: {e}"),
        )
    })?;
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_rejects_bad_endpoint() {
        let err = Client::connect("ftp://nope").err();
        assert!(err.is_some());
    }

    #[test]
    fn connect_accepts_ws() {
        let c = Client::connect("ws://localhost:9222").unwrap();
        assert!(matches!(c.endpoint(), Endpoint::Ws { .. }));
        assert!(c.token().is_none());
    }

    #[test]
    fn with_token_attaches() {
        let c = Client::connect("ws://localhost:9222")
            .unwrap()
            .with_token("secret");
        assert_eq!(c.token(), Some("secret"));
    }
}
