//! Inline ephemeral host support for CLI/SDK convenience.
//!
//! The returned `Client` owns an `InlineHost` guard. Dropping the last clone
//! sends listener shutdown, aborts the server task, drops the browser handle,
//! and therefore removes the ephemeral profile tempdir.

use crate::sdk::client::Client;
use crate::sdk::endpoint::Endpoint;
use crate::shared::error::{Error, ErrorCode};

#[cfg(feature = "host")]
use std::path::PathBuf;
#[cfg(feature = "host")]
use std::sync::Arc;

#[cfg(feature = "host")]
use crate::host::bootstrap::BrowserChoice;

/// Browser configuration for an inline ephemeral host.
///
/// `Default` keeps the historical behavior — `BrowserChoice::Auto` with no
/// explicit binary, i.e. auto-discovery. Set `browser_bin` to point at a
/// specific browser when auto-discovery can't find one (e.g. a non-standard
/// install location).
#[cfg(feature = "host")]
#[derive(Debug, Clone, Default)]
pub struct InlineConfig {
    pub browser: BrowserChoice,
    pub browser_bin: Option<PathBuf>,
}

#[cfg(feature = "host")]
#[derive(Clone)]
pub(crate) struct InlineHost {
    inner: Arc<InlineHostInner>,
}

#[cfg(feature = "host")]
struct InlineHostInner {
    config: InlineConfig,
    launched: tokio::sync::Mutex<Option<LaunchedInlineHost>>,
}

#[cfg(feature = "host")]
struct LaunchedInlineHost {
    endpoint: Endpoint,
    _guard: InlineHostGuard,
}

#[cfg(feature = "host")]
struct InlineHostGuard {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
    _state: crate::host::listener::AppState,
}

#[cfg(feature = "host")]
impl Drop for InlineHostGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.task.abort();
    }
}

#[cfg(feature = "host")]
impl InlineHost {
    pub(crate) fn lazy(config: InlineConfig) -> Self {
        Self {
            inner: Arc::new(InlineHostInner {
                config,
                launched: tokio::sync::Mutex::new(None),
            }),
        }
    }

    pub(crate) async fn launch_now(config: InlineConfig) -> Result<Self, Error> {
        let host = Self::lazy(config);
        let _ = host.endpoint().await?;
        Ok(host)
    }

    pub(crate) async fn endpoint(&self) -> Result<Endpoint, Error> {
        let mut guard = self.inner.launched.lock().await;
        if let Some(launched) = guard.as_ref() {
            return Ok(launched.endpoint.clone());
        }
        let launched = launch_inline_host(&self.inner.config).await?;
        let endpoint = launched.endpoint.clone();
        *guard = Some(launched);
        Ok(endpoint)
    }

    pub(crate) async fn is_started(&self) -> bool {
        self.inner.launched.lock().await.is_some()
    }

    #[cfg(test)]
    async fn from_state_for_tests(state: crate::host::listener::AppState) -> Result<Self, Error> {
        let host = Self::lazy(InlineConfig::default());
        let launched = serve_state(state).await?;
        *host.inner.launched.lock().await = Some(launched);
        Ok(host)
    }
}

#[cfg(not(feature = "host"))]
#[derive(Clone)]
pub(crate) struct InlineHost;

#[cfg(not(feature = "host"))]
impl InlineHost {
    pub(crate) async fn endpoint(&self) -> Result<Endpoint, Error> {
        Err(Error::new(
            ErrorCode::RenderUnavailable,
            "Client::inline_ephemeral requires the `host` feature",
        ))
    }

    pub(crate) async fn is_started(&self) -> bool {
        false
    }
}

#[cfg(feature = "host")]
impl Client {
    /// Spawn a private `afhttp host` listener bound to a random local
    /// port, launch a chromium backend, and connect to it. The host runs
    /// in the same process as a tokio task and is shut down when the
    /// last clone of the returned `Client` is dropped.
    pub async fn inline_ephemeral() -> Result<Self, Error> {
        Self::inline_ephemeral_with(InlineConfig::default()).await
    }

    /// Like [`inline_ephemeral`](Self::inline_ephemeral) but with explicit
    /// browser configuration (backend choice and/or binary path). Use this when
    /// auto-discovery can't find a browser on the host.
    pub async fn inline_ephemeral_with(config: InlineConfig) -> Result<Self, Error> {
        let inline = InlineHost::launch_now(config).await?;
        let endpoint = inline.endpoint().await?;
        let client = Client::connect(&endpoint.cdp_ws_url())?.with_inline_host(inline);
        Ok(client)
    }

    pub(crate) async fn inline_ephemeral_lazy(config: InlineConfig) -> Result<Self, Error> {
        let inline = InlineHost::lazy(config);
        Ok(Client::connect("ws://127.0.0.1:0")?.with_inline_host(inline))
    }
}

#[cfg(not(feature = "host"))]
impl Client {
    /// Not available without the `host` feature.
    pub async fn inline_ephemeral() -> Result<Self, Error> {
        Err(Error::new(
            ErrorCode::RenderUnavailable,
            "Client::inline_ephemeral requires the `host` feature",
        ))
    }
}

#[cfg(feature = "host")]
async fn launch_inline_host(config: &InlineConfig) -> Result<LaunchedInlineHost, Error> {
    use crate::host::bootstrap::{
        install_rustls_provider, DisplayMode, HealthPublic, HostArgs, ProfileChoice, Takeover,
    };
    use crate::host::listener::AppState;

    // The host side builds a reqwest client (CDP /json fetch) before the SDK
    // client's own provider guard runs; install it up front so SDK callers of
    // inline_ephemeral_with don't panic.
    install_rustls_provider();

    let args = HostArgs {
        listen: "tcp:127.0.0.1:0".into(),
        profile: ProfileChoice::Ephemeral,
        display: DisplayMode::Headless,
        takeover: Takeover::Off,
        display_quality: 100,
        browser: config.browser.clone(),
        browser_bin: config.browser_bin.clone(),
        token: None,
        takeover_enabled: false,
        health_enabled: true,
        health_public: HealthPublic::Off,
        engine_envs: Vec::new(),
        browser_args: Vec::new(),
        proxy: None,
        recent_requests_cap: 0,
    };
    let state = AppState::launch(&args).await?;
    serve_state(state).await
}

#[cfg(feature = "host")]
async fn serve_state(state: crate::host::listener::AppState) -> Result<LaunchedInlineHost, Error> {
    use tokio::net::TcpListener;

    use crate::host::listener::build_router;

    let app = build_router(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| Error::new(ErrorCode::IoError, format!("inline_ephemeral bind: {e}")))?;
    let addr = listener.local_addr().map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("inline_ephemeral local_addr: {e}"),
        )
    })?;
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    Ok(LaunchedInlineHost {
        endpoint: Endpoint::parse(&format!("ws://{addr}"))?,
        _guard: InlineHostGuard {
            shutdown: Some(tx),
            task,
            _state: state,
        },
    })
}

#[cfg(all(test, feature = "host"))]
mod tests {
    use std::sync::Arc;

    use crate::host::bootstrap::HealthPublic;
    use crate::host::browser::BrowserHandle;
    use crate::host::listener::test_state;

    use super::*;

    #[tokio::test]
    async fn inline_guard_drop_closes_port_and_removes_ephemeral_profile() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let tempdir = tempfile::Builder::new()
            .prefix("afhttp-inline-test-")
            .tempdir()
            .expect("tempdir");
        let profile_path = tempdir.path().to_path_buf();
        let state = test_state(None, HealthPublic::Off)
            .with_default_browser(Arc::new(BrowserHandle::synthetic_ephemeral(tempdir)));
        let inline = InlineHost::from_state_for_tests(state)
            .await
            .expect("inline state");
        let endpoint = inline.endpoint().await.expect("endpoint");
        let base = endpoint.http_base();
        let ok = reqwest::Client::new()
            .get(format!("{base}/health"))
            .send()
            .await
            .expect("health");
        assert!(ok.status().is_success());

        drop(inline);
        for _ in 0..20 {
            if !profile_path.exists()
                && reqwest::Client::new()
                    .get(format!("{base}/health"))
                    .send()
                    .await
                    .is_err()
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(
            !profile_path.exists(),
            "ephemeral profile should be removed: {}",
            profile_path.display()
        );
        assert!(
            reqwest::Client::new()
                .get(format!("{base}/health"))
                .send()
                .await
                .is_err(),
            "inline listener should stop accepting connections"
        );
    }
}
