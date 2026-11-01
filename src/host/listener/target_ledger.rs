//! What one `/cdp` connection does to the browser's set of page targets,
//! read off the frames it forwards.
//!
//! The proxy stays transparent: nothing is rewritten or withheld. But it is
//! the one party that sees every driver's traffic, so it is the one that can
//! say which tabs a connection opened and never closed. When the connection
//! ends, those tabs are orphans. No driver will ever close them, and the host
//! must not wait for the whole browser to fall silent before reclaiming them:
//! a killed fetch leaves its tab behind while every other driver carries on,
//! and a browser that is never quiet would otherwise keep the leftovers of
//! every dead driver for good.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

/// Responses this large are never `Target.createTarget` replies, so they are
/// forwarded without being parsed: a screenshot or a captured body must not
/// cost a JSON parse on the proxy's hot path.
const MAX_INSPECTED_RESPONSE_BYTES: usize = 16 * 1024;

/// A change in which targets this connection holds.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Holding {
    /// The connection now holds this target: it created or attached to it.
    Took(String),
    /// The connection closed this target.
    Closed(String),
}

#[derive(Default)]
pub(crate) struct TargetLedger {
    /// `Target.createTarget` requests awaiting their reply, by request id.
    pending_creates: HashMap<u64, ()>,
    /// Targets this connection created, or attached to, and has not closed.
    owned: HashSet<String>,
}

impl TargetLedger {
    /// A frame on its way to the browser. Reports a target the connection has
    /// just started holding by attaching to it, or has closed.
    pub(crate) fn observe_request(&mut self, frame: &str) -> Option<Holding> {
        let message: Value = serde_json::from_str(frame).ok()?;
        let method = message.get("method")?.as_str()?;
        match method {
            "Target.createTarget" => {
                if let Some(id) = message.get("id").and_then(Value::as_u64) {
                    self.pending_creates.insert(id, ());
                }
                None
            }
            "Target.closeTarget" => {
                let target_id = message["params"]["targetId"].as_str()?;
                self.owned
                    .remove(target_id)
                    .then(|| Holding::Closed(target_id.to_string()))
            }
            "Target.attachToTarget" => {
                let target_id = message["params"]["targetId"].as_str()?.to_string();
                self.owned
                    .insert(target_id.clone())
                    .then_some(Holding::Took(target_id))
            }
            _ => None,
        }
    }

    /// A frame on its way back to the client. Reports a target the connection
    /// has just created.
    pub(crate) fn observe_response(&mut self, frame: &str) -> Option<Holding> {
        if self.pending_creates.is_empty() || frame.len() > MAX_INSPECTED_RESPONSE_BYTES {
            return None;
        }
        let message = serde_json::from_str::<Value>(frame).ok()?;
        let id = message.get("id").and_then(Value::as_u64)?;
        // An error reply settles the request too: nothing was created.
        self.pending_creates.remove(&id)?;
        let target_id = message["result"]["targetId"].as_str()?.to_string();
        self.owned
            .insert(target_id.clone())
            .then_some(Holding::Took(target_id))
    }

    /// The targets left behind when the connection ends.
    pub(crate) fn into_owned(self) -> HashSet<String> {
        self.owned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(ledger: &TargetLedger) -> Vec<&str> {
        let mut owned: Vec<&str> = ledger.owned.iter().map(String::as_str).collect();
        owned.sort_unstable();
        owned
    }

    #[test]
    fn a_created_target_is_owned_until_the_same_connection_closes_it() {
        let mut ledger = TargetLedger::default();
        ledger.observe_request(
            r#"{"id":1,"method":"Target.createTarget","params":{"url":"about:blank"}}"#,
        );
        assert!(
            owned(&ledger).is_empty(),
            "nothing is owned before the browser replies"
        );
        ledger.observe_response(r#"{"id":1,"result":{"targetId":"T1"}}"#);
        assert_eq!(owned(&ledger), ["T1"]);
        ledger.observe_request(
            r#"{"id":2,"method":"Target.closeTarget","params":{"targetId":"T1"}}"#,
        );
        assert!(owned(&ledger).is_empty());
    }

    #[test]
    fn replies_are_matched_by_id_and_errors_create_nothing() {
        let mut ledger = TargetLedger::default();
        ledger.observe_request(
            r#"{"id":7,"method":"Target.createTarget","params":{"url":"about:blank"}}"#,
        );
        ledger.observe_request(
            r#"{"id":8,"method":"Target.createTarget","params":{"url":"about:blank"}}"#,
        );
        // Unrelated replies, and replies to other methods, change nothing.
        ledger.observe_response(r#"{"id":3,"result":{"targetId":"NOT-OURS"}}"#);
        ledger.observe_response(
            r#"{"method":"Target.targetCreated","params":{"targetInfo":{"targetId":"EVENT"}}}"#,
        );
        ledger.observe_response(r#"{"id":8,"error":{"code":-32000,"message":"no"}}"#);
        ledger.observe_response(r#"{"id":7,"result":{"targetId":"T7"}}"#);
        assert_eq!(owned(&ledger), ["T7"]);
        assert!(
            ledger.pending_creates.is_empty(),
            "every reply settles its request"
        );
    }

    #[test]
    fn attaching_adopts_a_target_and_reports_it() {
        let mut ledger = TargetLedger::default();
        let adopted = ledger.observe_request(
            r#"{"id":1,"method":"Target.attachToTarget","params":{"targetId":"T9","flatten":true}}"#,
        );
        assert_eq!(adopted, Some(Holding::Took("T9".into())));
        // Attaching again is not a second hold.
        assert!(
            ledger
                .observe_request(
                    r#"{"id":3,"method":"Target.attachToTarget","params":{"targetId":"T9"}}"#
                )
                .is_none()
        );
        assert_eq!(owned(&ledger), ["T9"]);
        assert!(
            ledger
                .observe_request(
                    r#"{"id":2,"method":"Page.navigate","params":{"url":"https://a.test/"}}"#
                )
                .is_none()
        );
        assert_eq!(ledger.into_owned().into_iter().collect::<Vec<_>>(), ["T9"]);
    }

    #[test]
    fn oversized_and_malformed_frames_are_forwarded_without_inspection() {
        let mut ledger = TargetLedger::default();
        ledger.observe_request(r#"{"id":1,"method":"Target.createTarget","params":{}}"#);
        let huge = format!(
            r#"{{"id":1,"result":{{"targetId":"T1","padding":"{}"}}}}"#,
            "x".repeat(MAX_INSPECTED_RESPONSE_BYTES)
        );
        ledger.observe_response(&huge);
        ledger.observe_response("not json");
        assert!(ledger.observe_request("not json").is_none());
        assert!(owned(&ledger).is_empty());
        assert_eq!(
            ledger.pending_creates.len(),
            1,
            "the real reply is still awaited"
        );
    }
}
