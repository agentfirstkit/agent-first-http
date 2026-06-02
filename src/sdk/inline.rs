//! Inline ephemeral host support for CLI/SDK convenience.
//!
//! The returned `Client` owns an `InlineHost` guard. Dropping the last clone
//! sends listener shutdown, aborts the server task, drops the browser handle,
//! and therefore removes the ephemeral profile tempdir.

use crate::sdk::client::Client;
use crate::sdk::endpoint::Endpoint;
use crate::shared::error::{Error, ErrorCode};

#[cfg(feature = "host")]
use std::sync::Arc;

#[cfg(feature = "host")]
#[derive(Clone)]
pub(crate) struct InlineHost {
    inner: Arc<InlineHostInner>,
}

#[cfg(feature = "host")]
struct InlineHostInner {
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
    pub(crate) fn lazy() -> Self {
        Self {
            inner: Arc::new(InlineHostInner {
                launched: tokio::sync::Mutex::new(None),
            }),
        }
    }

    pub(crate) async fn launch_now() -> Result<Self, Error> {
        let host = Self::lazy();
        let _ = host.endpoint().await?;
        Ok(host)
    }

    pub(crate) async fn endpoint(&self) -> Result<Endpoint, Error> {
        let mut guard = self.inner.launched.lock().await;
        if let Some(launched) = guard.as_ref() {
            return Ok(launched.endpoint.clone());
        }
        let launched = launch_inline_host().await?;
        let endpoint = launched.endpoint.clone();
        *guard = Some(launched);
        Ok(endpoint)
    }

    pub(crate) async fn is_started(&self) -> bool {
        self.inner.launched.lock().await.is_some()
    }

    #[cfg(test)]
    async fn from_state_for_tests(state: crate::host::listener::AppState) -> Result<Self, Error> {
        let host = Self::lazy();
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
        let inline = InlineHost::launch_now().await?;
        let endpoint = inline.endpoint().await?;
        let client = Client::connect(&endpoint.cdp_ws_url())?.with_inline_host(inline);
        Ok(client)
    }

    pub(crate) async fn inline_ephemeral_lazy() -> Result<Self, Error> {
        let inline = InlineHost::lazy();
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
async fn launch_inline_host() -> Result<LaunchedInlineHost, Error> {
    use crate::host::bootstrap::{
        BrowserChoice, DisplayMode, HealthPublic, HostArgs, ProfileChoice, Takeover,
    };
    use crate::host::listener::AppState;

    let args = HostArgs {
        listen: "tcp:127.0.0.1:0".into(),
        profile: ProfileChoice::Ephemeral,
        display: DisplayMode::Headless,
        takeover: Takeover::Off,
        display_quality: 100,
        browser: BrowserChoice::Auto,
        browser_bin: None,
        token: None,
        ops_enabled: false,
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
