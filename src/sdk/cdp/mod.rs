//! Raw CDP client: a WebSocket transport with flattened-session
//! multiplexing for multi-attach.

pub mod session;
pub mod ws_client;

use serde_json::Value;

use crate::sdk::client::Client;
use crate::shared::error::{Error, ErrorCode};
use crate::shared::ids::TabId;

/// How long a raw CDP call may wait for the connection, the tab attach, and
/// the command's reply together. Matches the default whole-fetch budget.
pub const DEFAULT_CDP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Builder for `Client::cdp(method)`.
pub struct CdpBuilder {
    pub(crate) client: Client,
    pub(crate) method: String,
    pub(crate) tab: Option<TabId>,
    pub(crate) params: Value,
    pub(crate) wait: Option<(String, std::time::Duration)>,
    pub(crate) timeout: std::time::Duration,
}

impl CdpBuilder {
    pub fn new(client: Client, method: impl Into<String>) -> Self {
        Self {
            client,
            method: method.into(),
            tab: None,
            params: Value::Object(Default::default()),
            wait: None,
            timeout: DEFAULT_CDP_TIMEOUT,
        }
    }

    #[must_use]
    pub fn tab(mut self, tab: TabId) -> Self {
        self.tab = Some(tab);
        self
    }

    #[must_use]
    pub fn params(mut self, p: Value) -> Self {
        self.params = p;
        self
    }

    /// Bound connecting, attaching, and the command's reply. A
    /// [`Self::wait_for`] event wait has its own timeout on top of this.
    #[must_use]
    pub fn timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[must_use]
    pub fn wait_for(mut self, event: impl Into<String>, timeout: std::time::Duration) -> Self {
        self.wait = Some((event.into(), timeout));
        self
    }

    /// Execute the CDP method on the Client's cached `/cdp` connection,
    /// optionally waiting for a follow-up event.
    pub async fn send(self) -> Result<Value, Error> {
        // A browser that stopped answering must end the call with an error,
        // not hold it forever.
        let deadline = tokio::time::Instant::now() + self.timeout;
        let method = self.method.clone();
        let budget = self.timeout;
        let timed_out = move |stage: &str| {
            Error::new(
                ErrorCode::CdpTimeout,
                format!(
                    "CDP {method}: no reply within {}ms while {stage}",
                    budget.as_millis()
                ),
            )
        };
        let conn = tokio::time::timeout_at(deadline, self.client.cdp_connection())
            .await
            .map_err(|_| timed_out("connecting"))??;

        // If --tab was provided, attach to it via Target.attachToTarget so
        // the call lands on the right session. Without --tab we issue
        // browser-scoped methods (Browser.getVersion, Target.*, etc.).
        let session_id = if let Some(tab) = self.tab.as_ref() {
            Some(
                tokio::time::timeout_at(deadline, session::attach_to_target(&conn, tab.as_str()))
                    .await
                    .map_err(|_| timed_out("attaching to the tab"))??,
            )
        } else {
            None
        };

        let outcome = async {
            // Subscribe before sending: a command can emit the awaited event
            // ahead of its own response.
            let events = self.wait.as_ref().map(|_| conn.subscribe());
            let result = tokio::time::timeout_at(
                deadline,
                conn.send(&self.method, &self.params, session_id.as_deref()),
            )
            .await
            .map_err(|_| timed_out("waiting for the reply"))??;

            if let (Some((event, timeout)), Some(events)) = (self.wait, events) {
                // On a tab, only that tab's event: the connection carries
                // every attached session's events, and another tab's load
                // would otherwise end the wait early.
                let tab_session = session_id.clone();
                let _ev = ws_client::Connection::wait_event_on(events, timeout, move |ev| {
                    ev.method == event && (tab_session.is_none() || ev.session_id == tab_session)
                })
                .await?;
            }
            Ok(result)
        }
        .await;

        if let Some(sid) = session_id.as_deref() {
            let _ = conn
                .send_timeout(
                    "Target.detachFromTarget",
                    &serde_json::json!({"sessionId": sid}),
                    None,
                    std::time::Duration::from_secs(2),
                )
                .await;
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    /// A browser that accepts connections and never answers ends the call
    /// with `cdp_timeout`, whether it stalls the handshake, the tab attach, or
    /// the command's reply.
    #[tokio::test]
    async fn a_reply_that_never_arrives_ends_in_cdp_timeout() {
        for (complete_handshake, tab) in [(false, false), (true, false), (true, true)] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let peer = tokio::spawn(async move {
                let mut held = Vec::new();
                while let Ok((stream, _)) = listener.accept().await {
                    if complete_handshake {
                        if let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await {
                            tokio::spawn(async move { while ws.next().await.is_some() {} });
                        }
                    } else {
                        held.push(stream);
                    }
                }
            });
            let client = crate::sdk::Client::connect(&format!("ws://{addr}")).unwrap();
            let mut call = client
                .cdp("Browser.getVersion")
                .timeout(std::time::Duration::from_millis(300));
            if tab {
                call = call.tab(crate::shared::ids::TabId::new("tab"));
            }
            let started = std::time::Instant::now();
            let error = tokio::time::timeout(std::time::Duration::from_secs(5), call.send())
                .await
                .expect("the call returned instead of waiting forever")
                .unwrap_err();
            assert_eq!(
                error.error_code,
                crate::shared::error::ErrorCode::CdpTimeout,
                "handshake={complete_handshake} tab={tab}: {error:?}"
            );
            assert!(started.elapsed() < std::time::Duration::from_secs(3));
            peer.abort();
        }
    }

    #[tokio::test]
    async fn wait_for_sees_an_event_sent_ahead_of_the_command_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(message)) = ws.next().await {
                let Message::Text(text) = message else {
                    continue;
                };
                let request: serde_json::Value = serde_json::from_str(&text).unwrap();
                let event = serde_json::json!({"method": "Page.lifecycleEvent", "params": {}});
                let response = serde_json::json!({"id": request["id"], "result": {}});
                for frame in [event, response] {
                    ws.send(Message::Text(frame.to_string().into()))
                        .await
                        .unwrap();
                }
            }
        });
        let client = crate::sdk::Client::connect(&format!("ws://{addr}")).unwrap();
        client
            .cdp("Page.setLifecycleEventsEnabled")
            .wait_for("Page.lifecycleEvent", std::time::Duration::from_secs(2))
            .send()
            .await
            .unwrap();
    }
}
