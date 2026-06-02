//! Raw CDP client: a WebSocket transport with flattened-session
//! multiplexing for multi-attach.

pub mod session;
pub mod ws_client;

use serde_json::Value;

use crate::sdk::client::Client;
use crate::shared::error::Error;
use crate::shared::ids::TabId;

/// Builder for `Client::cdp(method)`.
pub struct CdpBuilder {
    pub(crate) client: Client,
    pub(crate) method: String,
    pub(crate) tab: Option<TabId>,
    pub(crate) params: Value,
    pub(crate) wait: Option<(String, std::time::Duration)>,
}

impl CdpBuilder {
    pub fn new(client: Client, method: impl Into<String>) -> Self {
        Self {
            client,
            method: method.into(),
            tab: None,
            params: Value::Object(Default::default()),
            wait: None,
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

    #[must_use]
    pub fn wait_for(mut self, event: impl Into<String>, timeout: std::time::Duration) -> Self {
        self.wait = Some((event.into(), timeout));
        self
    }

    /// Execute the CDP method on the Client's cached `/cdp` connection,
    /// optionally waiting for a follow-up event.
    pub async fn send(self) -> Result<Value, Error> {
        let conn = self.client.cdp_connection().await?;

        // If --tab was provided, attach to it via Target.attachToTarget so
        // the call lands on the right session. Without --tab we issue
        // browser-scoped methods (Browser.getVersion, Target.*, etc.).
        let session_id = if let Some(tab) = self.tab.as_ref() {
            Some(session::attach_to_target(&conn, tab.as_str()).await?)
        } else {
            None
        };

        let outcome = async {
            let result = conn
                .send(&self.method, &self.params, session_id.as_deref())
                .await?;

            if let Some((event, timeout)) = self.wait {
                let event_name = event.clone();
                let _ev = conn
                    .wait_event(timeout, move |ev| ev.method == event_name)
                    .await?;
            }
            Ok(result)
        }
        .await;

        if let Some(sid) = session_id.as_deref() {
            let _ = session::detach_from_target(&conn, sid).await;
        }
        outcome
    }
}
