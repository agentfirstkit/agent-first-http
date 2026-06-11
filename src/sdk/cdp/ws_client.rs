//! Async CDP WebSocket client.
//!
//! Each `Connection` owns one WebSocket to the host's `/cdp` endpoint.
//! Sent commands are tagged with monotonically increasing ids; replies and
//! events are demuxed by the reader task into pending-request channels and
//! a broadcast event stream respectively.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::{
    self,
    client::IntoClientRequest,
    handshake::client::generate_key,
    http::{header, Request, Uri},
};
use tokio_tungstenite::WebSocketStream;

use crate::sdk::endpoint::Endpoint;
use crate::shared::error::{Error, ErrorCode};

type ReplySender = oneshot::Sender<Result<Value, CdpRemoteError>>;
type PendingMap = Arc<Mutex<HashMap<i64, ReplySender>>>;

/// One CDP connection.
pub struct Connection {
    tx: mpsc::UnboundedSender<OutMsg>,
    pending: PendingMap,
    events_tx: broadcast::Sender<CdpEvent>,
    next_id: AtomicI64,
    _reader: JoinHandle<()>,
    _writer: JoinHandle<()>,
}

enum OutMsg {
    Text(String),
    Close,
}

#[derive(Debug, Clone)]
pub struct CdpEvent {
    pub method: String,
    pub session_id: Option<String>,
    pub params: Value,
}

#[derive(Debug, Clone)]
pub struct CdpRemoteError {
    pub code: i64,
    pub message: String,
}

impl std::fmt::Display for CdpRemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CDP error {}: {}", self.code, self.message)
    }
}

impl Connection {
    /// Open a CDP connection from a parsed endpoint. `profile`, when set, is
    /// sent as `?profile=` so the host switches its active profile.
    pub async fn connect_endpoint(
        endpoint: &Endpoint,
        token: Option<&str>,
        profile: Option<&str>,
    ) -> Result<Self, Error> {
        match endpoint {
            #[cfg(unix)]
            Endpoint::Unix { path } => Self::connect_unix(path, token, profile).await,
            _ => {
                let base = endpoint.cdp_ws_url();
                let url = match profile {
                    Some(p) => append_query_pairs(&base, &[("profile", p)])?,
                    None => base,
                };
                Self::connect(&url, token).await
            }
        }
    }

    /// Open a CDP connection to `ws://endpoint/cdp` (or `wss://`),
    /// attaching the optional bearer token via the `?token_secret=` query
    /// parameter (the `_secret` suffix lets AFDATA redaction scrub it).
    pub async fn connect(endpoint_ws_url: &str, token: Option<&str>) -> Result<Self, Error> {
        // Append ?token_secret= if needed.
        let url = match token {
            Some(t) => append_query_pairs(endpoint_ws_url, &[("token_secret", t)])?,
            None => endpoint_ws_url.to_string(),
        };
        let request = build_ws_request(&url)?;
        let uri: Uri = url
            .parse()
            .map_err(|e| Error::new(ErrorCode::InvalidEndpoint, format!("CDP url {url:?}: {e}")))?;
        let secure = uri
            .scheme_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("wss"));
        if !secure {
            // Plaintext ws:// (all local CDP, and ws:// remote hosts): connect the
            // TCP stream directly and run the handshake with client_async. We
            // avoid connect_async because, with the rustls-tls-native-roots
            // feature, it builds a TLS connector and loads the OS root-cert store
            // even for ws:// — wasted work for every CDP connect, and stack-heavy
            // enough to overflow Windows' 1 MiB main-thread stack.
            let host = uri.host().ok_or_else(|| {
                Error::new(
                    ErrorCode::InvalidEndpoint,
                    format!("CDP url has no host: {url:?}"),
                )
            })?;
            let port = uri.port_u16().unwrap_or(80);
            let stream = tokio::net::TcpStream::connect((host, port))
                .await
                .map_err(|e| {
                    Error::new(
                        ErrorCode::HostUnreachable,
                        format!(
                            "CDP connect {}: {e}",
                            agent_first_data::redact_url_secrets(&url)
                        ),
                    )
                })?;
            let (ws, _resp) = tokio_tungstenite::client_async(request, stream)
                .await
                .map_err(|e| cdp_connect_error("CDP websocket", &url, e))?;
            return Ok(Self::from_ws(ws));
        }
        // wss:// — keep the TLS-capable connector.
        let (ws, _resp) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| cdp_connect_error("CDP connect", &url, e))?;
        Ok(Self::from_ws(ws))
    }

    #[cfg(unix)]
    async fn connect_unix(
        path: &std::path::Path,
        token: Option<&str>,
        profile: Option<&str>,
    ) -> Result<Self, Error> {
        let mut pairs: Vec<(&str, &str)> = Vec::new();
        if let Some(t) = token {
            pairs.push(("token_secret", t));
        }
        if let Some(p) = profile {
            pairs.push(("profile", p));
        }
        let url = if pairs.is_empty() {
            "ws://localhost/cdp".to_string()
        } else {
            append_query_pairs("ws://localhost/cdp", &pairs)?
        };
        let request = build_ws_request(&url)?;
        let stream = tokio::net::UnixStream::connect(path).await.map_err(|e| {
            Error::new(
                ErrorCode::HostUnreachable,
                format!("CDP connect unix:{}: {e}", path.display()),
            )
        })?;
        let (ws, _resp) = tokio_tungstenite::client_async(request, stream)
            .await
            .map_err(|e| {
                Error::new(
                    ErrorCode::HostUnreachable,
                    format!("CDP websocket over unix:{}: {e}", path.display()),
                )
            })?;
        Ok(Self::from_ws(ws))
    }

    fn from_ws<S>(ws: WebSocketStream<S>) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (mut sink, mut stream) = ws.split();

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, _events_rx) = broadcast::channel::<CdpEvent>(256);
        let (tx, mut rx) = mpsc::unbounded_channel::<OutMsg>();

        let pending_w = pending.clone();
        let events_w = events_tx.clone();
        let reader = tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                match msg {
                    tungstenite::Message::Text(t) => {
                        if let Ok(v) = serde_json::from_str::<Value>(t.as_str()) {
                            dispatch(v, &pending_w, &events_w).await;
                        }
                    }
                    tungstenite::Message::Binary(_)
                    | tungstenite::Message::Ping(_)
                    | tungstenite::Message::Pong(_) => {}
                    tungstenite::Message::Close(_) | tungstenite::Message::Frame(_) => break,
                }
            }
            // On close, fail all pending requests so callers stop waiting.
            let mut map = pending_w.lock().await;
            for (_, sender) in map.drain() {
                let _ = sender.send(Err(CdpRemoteError {
                    code: -1,
                    message: "CDP connection closed".into(),
                }));
            }
        });

        let writer = tokio::spawn(async move {
            while let Some(out) = rx.recv().await {
                let msg = match out {
                    OutMsg::Text(t) => tungstenite::Message::Text(t.as_str().into()),
                    OutMsg::Close => {
                        let _ = sink.send(tungstenite::Message::Close(None)).await;
                        break;
                    }
                };
                if sink.send(msg).await.is_err() {
                    break;
                }
            }
        });

        Self {
            tx,
            pending,
            events_tx,
            next_id: AtomicI64::new(1),
            _reader: reader,
            _writer: writer,
        }
    }

    /// Subscribe to events for the lifetime of this connection.
    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.events_tx.subscribe()
    }

    /// Send a CDP command and await the result. `session_id` is `Some` when
    /// the call is scoped to a flattened session (after Target.attachToTarget).
    pub async fn send<P: Serialize>(
        &self,
        method: &str,
        params: &P,
        session_id: Option<&str>,
    ) -> Result<Value, Error> {
        self.send_inner(method, params, session_id, None).await
    }

    /// Send a CDP command and fail with `cdp_timeout` if the browser does not
    /// answer within `timeout`. Unlike wrapping [`Self::send`] externally, this
    /// removes the pending reply slot on timeout.
    pub async fn send_timeout<P: Serialize>(
        &self,
        method: &str,
        params: &P,
        session_id: Option<&str>,
        timeout: Duration,
    ) -> Result<Value, Error> {
        self.send_inner(method, params, session_id, Some(timeout))
            .await
    }

    async fn send_inner<P: Serialize>(
        &self,
        method: &str,
        params: &P,
        session_id: Option<&str>,
        timeout: Option<Duration>,
    ) -> Result<Value, Error> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let body = match session_id {
            Some(sid) => serde_json::json!({
                "id": id,
                "method": method,
                "params": params,
                "sessionId": sid,
            }),
            None => serde_json::json!({
                "id": id,
                "method": method,
                "params": params,
            }),
        };
        let serialized = serde_json::to_string(&body).map_err(|e| {
            Error::new(
                ErrorCode::InternalError,
                format!("CDP send: serialize {method}: {e}"),
            )
        })?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.pending.lock().await.insert(id, resp_tx);
        self.tx
            .send(OutMsg::Text(serialized))
            .map_err(|_| Error::new(ErrorCode::CdpUnavailable, "CDP writer closed before send"))?;
        let received = if let Some(timeout) = timeout {
            match tokio::time::timeout(timeout, resp_rx).await {
                Ok(value) => value,
                Err(_) => {
                    self.pending.lock().await.remove(&id);
                    return Err(Error::new(
                        ErrorCode::CdpTimeout,
                        format!("{method}: CDP reply timed out after {timeout:?}"),
                    ));
                }
            }
        } else {
            resp_rx.await
        };
        let value = received
            .map_err(|_| Error::new(ErrorCode::CdpUnavailable, "CDP reader closed"))?
            .map_err(|e| Error::new(ErrorCode::CdpError, e.to_string()))?;
        Ok(value)
    }

    /// Wait for a CDP event matching `predicate` (true = matches).
    /// Returns `cdp_timeout` if `timeout` elapses first.
    pub async fn wait_event<F>(
        &self,
        timeout: Duration,
        mut predicate: F,
    ) -> Result<CdpEvent, Error>
    where
        F: FnMut(&CdpEvent) -> bool,
    {
        let mut rx = self.events_tx.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(Error::new(ErrorCode::CdpTimeout, "wait_event: timed out"));
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(ev)) if predicate(&ev) => return Ok(ev),
                Ok(Ok(_)) => continue,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => {
                    return Err(Error::new(
                        ErrorCode::CdpUnavailable,
                        "wait_event: events channel closed",
                    ));
                }
                Err(_) => {
                    return Err(Error::new(ErrorCode::CdpTimeout, "wait_event: timed out"));
                }
            }
        }
    }

    pub fn close(&self) {
        let _ = self.tx.send(OutMsg::Close);
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.tx.send(OutMsg::Close);
    }
}

async fn dispatch(msg: Value, pending: &PendingMap, events: &broadcast::Sender<CdpEvent>) {
    if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
        let mut map = pending.lock().await;
        if let Some(sender) = map.remove(&id) {
            if let Some(err) = msg.get("error") {
                let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
                let message = err
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let _ = sender.send(Err(CdpRemoteError { code, message }));
            } else {
                let result = msg.get("result").cloned().unwrap_or(Value::Null);
                let _ = sender.send(Ok(result));
            }
        }
    } else if let Some(method) = msg.get("method").and_then(|v| v.as_str()) {
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let session_id = msg
            .get("sessionId")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let _ = events.send(CdpEvent {
            method: method.to_string(),
            session_id,
            params,
        });
    }
}

fn append_query_pairs(url: &str, pairs: &[(&str, &str)]) -> Result<String, Error> {
    let mut parsed = url::Url::parse(url)
        .map_err(|e| Error::new(ErrorCode::InvalidEndpoint, format!("CDP url {url:?}: {e}")))?;
    {
        let mut query = parsed.query_pairs_mut();
        for (key, value) in pairs {
            query.append_pair(key, value);
        }
    }
    Ok(parsed.to_string())
}

fn build_ws_request(url: &str) -> Result<Request<()>, Error> {
    // tokio_tungstenite::connect_async accepts &str directly via IntoClientRequest,
    // but we go through the explicit Request type so we can attach headers later.
    // The bearer token is already baked into the URL as `?token_secret=` by the
    // caller; a bearer header would also work, but axum's WebSocketUpgrade
    // ignores it for the upgrade handshake.
    let uri: Uri = url
        .parse()
        .map_err(|e| Error::new(ErrorCode::InvalidEndpoint, format!("CDP url {url:?}: {e}")))?;
    let host = uri.authority().map(|a| a.as_str()).unwrap_or("localhost");
    let req = Request::builder()
        .method("GET")
        .uri(url)
        .header(header::HOST, host)
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_VERSION, "13")
        .header(header::SEC_WEBSOCKET_KEY, generate_key())
        .body(())
        .map_err(|e| Error::new(ErrorCode::InternalError, format!("CDP build request: {e}")))?;
    req.into_client_request().map_err(|e| {
        Error::new(
            ErrorCode::InternalError,
            format!("CDP into_client_request: {e}"),
        )
    })
}

fn cdp_connect_error(context: &str, url: &str, err: tungstenite::Error) -> Error {
    let redacted = agent_first_data::redact_url_secrets(url);
    if let tungstenite::Error::Http(resp) = err {
        let status = resp.status();
        if let Some(bytes) = resp.body().as_ref() {
            if let Ok(remote) = serde_json::from_slice::<Error>(bytes) {
                return Error::new(
                    remote.error_code,
                    format!("{context} {redacted}: HTTP {status}: {}", remote.detail),
                )
                .with_retryable(remote.retryable);
            }
            let body = String::from_utf8_lossy(bytes).trim().to_string();
            if !body.is_empty() {
                return Error::new(
                    ErrorCode::HostUnreachable,
                    format!("{context} {redacted}: HTTP {status}: {body}"),
                );
            }
        }
        return Error::new(
            ErrorCode::HostUnreachable,
            format!("{context} {redacted}: HTTP {status}"),
        );
    }
    Error::new(
        ErrorCode::HostUnreachable,
        format!("{context} {redacted}: {err}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_pairs_are_percent_encoded() {
        let url = append_query_pairs("ws://localhost:9222/cdp", &[("token", "a+b&c%20")]).unwrap();
        assert_eq!(url, "ws://localhost:9222/cdp?token=a%2Bb%26c%2520");
    }

    // The bearer travels as `?token_secret=`; AFDATA redaction must scrub it
    // before the failed-connect URL reaches the error envelope.
    #[tokio::test]
    async fn connect_error_redacts_token_secret() {
        // Port 1 refuses fast, so we exercise the map_err path deterministically.
        let err = Connection::connect("ws://127.0.0.1:1/cdp", Some("supersecret"))
            .await
            .err()
            .expect("connect to closed port must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("token_secret=***"),
            "token not redacted: {msg}"
        );
        assert!(!msg.contains("supersecret"), "raw token leaked: {msg}");
    }

    #[test]
    fn cdp_http_error_includes_profile_switch_body() {
        let body = serde_json::to_vec(&Error::new(
            ErrorCode::ProfileLocked,
            "profile switch to \"contabo.com\" failed: profile contabo.com already locked",
        ))
        .unwrap();
        let resp = tokio_tungstenite::tungstenite::http::Response::builder()
            .status(503)
            .body(Some(body))
            .unwrap();
        let err = cdp_connect_error(
            "CDP websocket",
            "ws://127.0.0.1:9222/cdp?profile=contabo.com&token_secret=supersecret",
            tungstenite::Error::Http(Box::new(resp)),
        );
        assert_eq!(err.error_code, ErrorCode::ProfileLocked);
        assert!(err.detail.contains("contabo.com"), "{}", err.detail);
        assert!(err.detail.contains("token_secret=***"), "{}", err.detail);
        assert!(!err.detail.contains("supersecret"), "{}", err.detail);
    }
}
