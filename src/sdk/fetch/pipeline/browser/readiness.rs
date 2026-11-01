//! Frame-aware mechanical readiness. A caller-declared region defines which
//! frames are ancillary; origin names never define that boundary.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::sdk::cdp::ws_client::Connection;
use crate::sdk::fetch::artifacts::collectors::{
    NetworkCollector, ReadinessNetwork, is_redirect_hop,
};
use crate::sdk::fetch::artifacts::network::NetworkEntryState;
use crate::sdk::fetch::deadline::FetchDeadline;
use crate::sdk::fetch::readiness::{PendingRequest, Readiness, ReadinessState, RequestScope};
use crate::shared::error::{Error, ErrorCode};
use crate::shared::time::duration_ms;

use super::wait::PageMetrics;

const FRAME_LIMIT: usize = 32;
const PENDING_LIMIT: usize = 100;
const METRICS_JS: &str = include_str!("../../../../../assets/readiness/metrics.js");

#[derive(Clone, Deserialize)]
struct Frame {
    id: String,
    url: String,
    #[serde(rename = "loaderId", default)]
    loader_id: String,
}

#[derive(Deserialize)]
struct FrameTree {
    frame: Frame,
    #[serde(rename = "childFrames", default)]
    children: Vec<FrameTree>,
}

#[derive(Clone, Deserialize)]
struct Metrics {
    #[serde(flatten)]
    page: PageMetrics,
    eligible: bool,
    document_ready: bool,
}

#[derive(Deserialize)]
struct Measurements {
    body: Metrics,
    selected: Metrics,
}

struct ScopedFrame {
    frame: Frame,
    scope: RequestScope,
    metrics: Option<Metrics>,
}

pub(super) struct Sample {
    pub(super) body: PageMetrics,
    pub(super) target: PageMetrics,
    pub(super) target_eligible: bool,
    pub(super) target_network_quiet: bool,
    pub(super) readiness: Readiness,
}

pub(super) async fn unknown_readiness(collector: &NetworkCollector, selector: &str) -> Readiness {
    let network = collector.readiness_snapshot("").await;
    let mut pending: Vec<_> = network
        .requests
        .into_iter()
        .filter(|r| {
            matches!(
                r.entry.state,
                NetworkEntryState::Pending | NetworkEntryState::Responded
            )
        })
        .map(|r| PendingRequest {
            request_id: r.entry.request_id,
            request_url: r.entry.request_url,
            resource_type: r.entry.resource_type,
            frame_id: r.entry.frame_id,
            frame_url: None,
            scope: RequestScope::Unknown,
            blocks_wait: true,
            waited_ms: duration_ms(r.waited),
        })
        .collect();
    pending.sort_by(|a, b| a.request_id.cmp(&b.request_id));
    let truncated = pending.len() > PENDING_LIMIT;
    pending.truncate(PENDING_LIMIT);
    Readiness {
        wait_satisfied: false,
        timed_out: true,
        main_document: ReadinessState::Unknown,
        target_content: ReadinessState::Unknown,
        ancillary: ReadinessState::Unknown,
        target_selector: selector.into(),
        target_dom_stable: false,
        target_text_stable: false,
        observation_complete: false,
        unobserved_frame_ids: Vec::new(),
        pending_requests: pending,
        pending_requests_truncated: truncated,
    }
}

pub(super) struct Sampler<'a> {
    conn: &'a Connection,
    session_id: &'a str,
    deadline: &'a FetchDeadline,
    selector: &'a str,
    strict: bool,
    contexts: BTreeMap<String, (String, i64)>,
}

impl<'a> Sampler<'a> {
    pub(super) fn new(
        conn: &'a Connection,
        session_id: &'a str,
        deadline: &'a FetchDeadline,
        selector: &'a str,
        strict: bool,
    ) -> Self {
        Self {
            conn,
            session_id,
            deadline,
            selector,
            strict,
            contexts: BTreeMap::new(),
        }
    }

    async fn send(&self, method: &str, params: Value) -> Result<Value, Error> {
        self.conn
            .send_timeout(
                method,
                &params,
                Some(self.session_id),
                self.deadline
                    .remaining("wait_readiness")?
                    .min(Duration::from_millis(200)),
            )
            .await
    }

    async fn measure(
        &self,
        selector: &str,
        context_id: Option<i64>,
    ) -> Result<Measurements, Error> {
        let expression = format!(
            "({METRICS_JS})({})",
            serde_json::to_string(selector)
                .map_err(|e| Error::new(ErrorCode::InternalError, e.to_string()))?
        );
        let mut params = json!({"expression": expression, "returnByValue": true});
        if let Some(context_id) = context_id {
            params["contextId"] = json!(context_id);
        }
        let result = self.send("Runtime.evaluate", params).await?;
        let value = &result["result"]["value"];
        if value["invalid_selector"].as_bool() == Some(true) {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "selector-stable requires valid CSS",
            ));
        }
        serde_json::from_value(value.clone())
            .map_err(|_| Error::new(ErrorCode::CdpError, "readiness metrics were unavailable"))
    }

    async fn owner_in_target(&self, frame_id: &str) -> Result<bool, Error> {
        let owner = self
            .send("DOM.getFrameOwner", json!({"frameId": frame_id}))
            .await?;
        let resolved = self
            .send(
                "DOM.resolveNode",
                json!({"backendNodeId": owner["backendNodeId"]}),
            )
            .await?;
        let object_id = resolved["object"]["objectId"]
            .as_str()
            .ok_or_else(|| Error::new(ErrorCode::CdpError, "frame owner could not be resolved"))?;
        let result = self.send("Runtime.callFunctionOn", json!({
            "objectId": object_id,
            "functionDeclaration": "function(selector) { const roots = Array.from(document.querySelectorAll(selector)); for (let node = this; node; node = node.parentNode || node.host) { if (roots.includes(node)) return true; } return false; }",
            "arguments": [{"value": self.selector}], "returnByValue": true
        })).await;
        let _ = self
            .send("Runtime.releaseObject", json!({"objectId": object_id}))
            .await;
        result?["result"]["value"].as_bool().ok_or_else(|| {
            Error::new(
                ErrorCode::CdpError,
                "frame owner scope could not be inspected",
            )
        })
    }

    async fn frame_metrics(&mut self, frame: &Frame) -> Result<Metrics, Error> {
        let context_id = match self.contexts.get(&frame.id) {
            Some((loader, context_id)) if *loader == frame.loader_id => *context_id,
            _ => {
                let result = self
                    .send(
                        "Page.createIsolatedWorld",
                        json!({
                            "frameId": frame.id, "worldName": "afhttp-readiness"
                        }),
                    )
                    .await?;
                let context_id = result["executionContextId"].as_i64().ok_or_else(|| {
                    Error::new(ErrorCode::CdpError, "frame execution context unavailable")
                })?;
                self.contexts
                    .insert(frame.id.clone(), (frame.loader_id.clone(), context_id));
                context_id
            }
        };
        match self.measure("body", Some(context_id)).await {
            Ok(metrics) => Ok(metrics.selected),
            Err(error) => {
                self.contexts.remove(&frame.id);
                Err(error)
            }
        }
    }

    pub(super) async fn sample(
        &mut self,
        collector: &NetworkCollector,
        idle_for: Duration,
    ) -> Result<Sample, Error> {
        let tree = self.send("Page.getFrameTree", json!({})).await?;
        let tree: FrameTree = serde_json::from_value(tree["frameTree"].clone())
            .map_err(|_| Error::new(ErrorCode::CdpError, "readiness frame tree unavailable"))?;
        let measurements = self.measure(self.selector, None).await?;
        let mut frames = vec![ScopedFrame {
            frame: tree.frame.clone(),
            scope: RequestScope::Target,
            metrics: Some(measurements.selected),
        }];
        let mut queue: std::collections::VecDeque<_> = tree
            .children
            .into_iter()
            .map(|child| (child, None))
            .collect();
        let mut unobserved = Vec::new();
        while let Some((child, parent_scope)) = queue.pop_front() {
            if frames.len() >= FRAME_LIMIT {
                unobserved.push(child.frame.id);
                continue;
            }
            let scope = match parent_scope {
                Some(scope) => scope,
                None if self.strict => RequestScope::Target,
                None => match self.owner_in_target(&child.frame.id).await {
                    Ok(true) => RequestScope::Target,
                    Ok(false) => RequestScope::Ancillary,
                    Err(_) => RequestScope::Unknown,
                },
            };
            let metrics = if scope == RequestScope::Target {
                self.frame_metrics(&child.frame).await.ok()
            } else {
                None
            };
            if scope == RequestScope::Unknown
                || (scope == RequestScope::Target && metrics.is_none())
            {
                unobserved.push(child.frame.id.clone());
            }
            queue.extend(child.children.into_iter().map(|node| (node, Some(scope))));
            frames.push(ScopedFrame {
                frame: child.frame,
                scope,
                metrics,
            });
        }
        let network = collector.readiness_snapshot(&tree.frame.id).await;
        // OOPIF documents can appear in Network events while being absent
        // from this renderer's Page.getFrameTree. Resolve their owner node
        // instead of treating an unknown frame as an optional third party.
        for request in &network.requests {
            let entry = &request.entry;
            let Some(frame_id) = entry.frame_id.as_ref() else {
                continue;
            };
            if entry.resource_type != "Document" || frames.iter().any(|f| f.frame.id == *frame_id) {
                continue;
            }
            if frames.len() >= FRAME_LIMIT {
                unobserved.push(frame_id.clone());
                continue;
            }
            let scope = if self.strict {
                RequestScope::Target
            } else {
                match self.owner_in_target(frame_id).await {
                    Ok(true) => RequestScope::Target,
                    Ok(false) => RequestScope::Ancillary,
                    Err(_) => RequestScope::Unknown,
                }
            };
            let frame = Frame {
                id: frame_id.clone(),
                url: entry.request_url.clone(),
                loader_id: entry.loader_id.clone().unwrap_or_default(),
            };
            let metrics = if scope == RequestScope::Target {
                self.frame_metrics(&frame).await.ok()
            } else {
                None
            };
            if scope == RequestScope::Unknown
                || (scope == RequestScope::Target && metrics.is_none())
            {
                unobserved.push(frame_id.clone());
            }
            frames.push(ScopedFrame {
                frame,
                scope,
                metrics,
            });
        }
        Ok(assess(
            frames,
            network,
            measurements.body.page,
            self.selector,
            self.strict,
            idle_for,
            unobserved,
        ))
    }
}

fn assess(
    frames: Vec<ScopedFrame>,
    network: ReadinessNetwork,
    body: PageMetrics,
    selector: &str,
    strict: bool,
    idle_for: Duration,
    unobserved: Vec<String>,
) -> Sample {
    let root = &frames[0].frame;
    let scopes: BTreeMap<_, _> = frames.iter().map(|f| (f.frame.id.as_str(), f)).collect();
    let mut target_network_quiet = network.complete;
    let mut required_pending = false;
    let mut ancillary_ready = network.complete;
    let mut pending = Vec::new();
    let mut main_document = ReadinessState::Unknown;
    for observed in &network.requests {
        let entry = &observed.entry;
        let frame = entry.frame_id.as_deref().and_then(|id| scopes.get(id));
        let scope =
            if entry.resource_type == "Document" && entry.frame_id.as_deref() == Some(&root.id) {
                RequestScope::MainDocument
            } else {
                frame.map_or(RequestScope::Unknown, |f| f.scope)
            };
        let unfinished = matches!(
            entry.state,
            NetworkEntryState::Pending | NetworkEntryState::Responded
        );
        let required = scope != RequestScope::Ancillary;
        required_pending |= required && unfinished;
        if required && (unfinished || observed.idle_for < idle_for) {
            target_network_quiet = false;
        }
        if !required && (unfinished || entry.state == NetworkEntryState::Failed) {
            ancillary_ready = false;
        }
        // A redirect hop shares the loader of the request that replaced
        // it; only the live hop speaks for the main document.
        if scope == RequestScope::MainDocument
            && entry.loader_id.as_deref() == Some(&root.loader_id)
            && !is_redirect_hop(entry)
        {
            main_document = if entry.state == NetworkEntryState::Finished {
                ReadinessState::Ready
            } else {
                ReadinessState::Incomplete
            };
        }
        if unfinished {
            pending.push(PendingRequest {
                request_id: entry.request_id.clone(),
                request_url: entry.request_url.clone(),
                resource_type: entry.resource_type.clone(),
                frame_id: entry.frame_id.clone(),
                frame_url: frame.map(|f| f.frame.url.clone()),
                scope,
                blocks_wait: strict || required,
                waited_ms: duration_ms(observed.waited),
            });
        }
    }
    // A failed required Document can never turn into readiness just because
    // its request left the inflight set. Subresource failures remain warnings
    // in the network artifact; content readiness is still mechanical.
    let failed_target_document = network.requests.iter().any(|request| {
        request.entry.resource_type == "Document"
            && request.entry.state == NetworkEntryState::Failed
            && request
                .entry
                .frame_id
                .as_deref()
                .and_then(|id| scopes.get(id))
                .is_none_or(|frame| {
                    frame.scope != RequestScope::Ancillary
                        && request.entry.loader_id.as_deref() == Some(&frame.frame.loader_id)
                })
    });
    let observation_complete = network.complete && unobserved.is_empty();
    let target_frames: Vec<_> = frames
        .iter()
        .filter(|f| f.scope == RequestScope::Target)
        .collect();
    let mut eligible =
        observation_complete && !failed_target_document && main_document == ReadinessState::Ready;
    let mut text_len = 0;
    let mut node_count = 0;
    let mut text_parts = Vec::new();
    let mut dom_parts = Vec::new();
    for frame in target_frames {
        if let Some(metrics) = &frame.metrics {
            eligible &= metrics.document_ready && (strict || metrics.eligible);
            text_len += metrics.page.text_len;
            node_count += metrics.page.node_count;
            text_parts.push((frame.frame.id.as_str(), metrics.page.text_hash));
            dom_parts.push((frame.frame.id.as_str(), metrics.page.dom_hash));
        } else {
            eligible = false;
        }
    }
    eligible &= strict || text_len > 0;
    pending.sort_by(|a, b| (!a.blocks_wait, &a.request_id).cmp(&(!b.blocks_wait, &b.request_id)));
    let pending_requests_truncated = pending.len() > PENDING_LIMIT;
    pending.truncate(PENDING_LIMIT);
    Sample {
        body,
        target: PageMetrics {
            text_len,
            text_hash: combine_hash(&text_parts),
            node_count,
            dom_hash: combine_hash(&dom_parts),
        },
        target_eligible: eligible,
        target_network_quiet,
        readiness: Readiness {
            wait_satisfied: false,
            timed_out: false,
            main_document,
            target_content: if observation_complete || required_pending {
                ReadinessState::Incomplete
            } else {
                ReadinessState::Unknown
            },
            ancillary: if !network.complete {
                ReadinessState::Unknown
            } else if ancillary_ready {
                ReadinessState::Ready
            } else {
                ReadinessState::Incomplete
            },
            target_selector: selector.to_string(),
            target_dom_stable: false,
            target_text_stable: false,
            observation_complete,
            unobserved_frame_ids: unobserved,
            pending_requests: pending,
            pending_requests_truncated,
        },
    }
}

fn combine_hash(parts: &[(&str, u32)]) -> u32 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    parts.hash(&mut hasher);
    hasher.finish() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::fetch::artifacts::collectors::ObservedRequest;
    use crate::sdk::fetch::artifacts::network::{NetworkEntry, NetworkTiming};

    fn frame(id: &str, scope: RequestScope) -> ScopedFrame {
        ScopedFrame {
            frame: Frame {
                id: id.into(),
                url: "https://example.test/".into(),
                loader_id: id.into(),
            },
            scope,
            metrics: Some(Metrics {
                page: PageMetrics {
                    text_len: 10,
                    text_hash: 1,
                    node_count: 2,
                    dom_hash: 1,
                },
                eligible: true,
                document_ready: true,
            }),
        }
    }

    fn request(id: &str, frame_id: &str, state: NetworkEntryState) -> ObservedRequest {
        ObservedRequest {
            entry: NetworkEntry {
                request_id: id.into(),
                state,
                redirect_from_request_id: None,
                frame_id: Some(frame_id.into()),
                loader_id: Some(frame_id.into()),
                resource_type: "Document".into(),
                request_url:
                    "https://user:password-canary@example.test/?api_key=key-canary&safe=ok".into(),
                method: "GET".into(),
                initiator: None,
                status: Some(200),
                mime_type: None,
                request_headers: BTreeMap::new(),
                response_headers: BTreeMap::new(),
                request_post_data_present: false,
                request_post_data_size_bytes: None,
                body_file: None,
                timing: NetworkTiming::default(),
                failure: None,
                hints: BTreeMap::new(),
            },
            waited: Duration::from_secs(2),
            idle_for: Duration::from_secs(2),
        }
    }

    #[test]
    fn loss_and_unobserved_frames_cannot_establish_target_readiness() {
        for (complete, unobserved) in [(false, vec![]), (true, vec!["missing".into()])] {
            let root = frame("root", RequestScope::Target);
            let sample = assess(
                vec![root],
                ReadinessNetwork {
                    requests: vec![request("main", "root", NetworkEntryState::Finished)],
                    complete,
                },
                PageMetrics {
                    text_len: 10,
                    text_hash: 1,
                    node_count: 2,
                    dom_hash: 1,
                },
                "#prices",
                false,
                Duration::ZERO,
                unobserved,
            );
            assert!(!sample.target_eligible);
            assert_eq!(sample.readiness.target_content, ReadinessState::Unknown);
        }
    }

    #[test]
    fn diagnostics_keep_blockers_before_the_limit_with_page_urls_intact() {
        let mut requests = vec![request("main", "root", NetworkEntryState::Finished)];
        requests.extend(
            (0..PENDING_LIMIT)
                .map(|index| request(&format!("a-{index}"), "rating", NetworkEntryState::Pending)),
        );
        requests.push(request("z-price", "price", NetworkEntryState::Pending));
        let sample = assess(
            vec![
                frame("root", RequestScope::Target),
                frame("price", RequestScope::Target),
                frame("rating", RequestScope::Ancillary),
            ],
            ReadinessNetwork {
                requests,
                complete: true,
            },
            PageMetrics {
                text_len: 10,
                text_hash: 1,
                node_count: 2,
                dom_hash: 1,
            },
            "#prices",
            false,
            Duration::ZERO,
            vec![],
        );
        assert!(!sample.target_network_quiet);
        assert!(sample.readiness.pending_requests_truncated);
        assert_eq!(sample.readiness.pending_requests[0].request_id, "z-price");
        assert_eq!(sample.readiness.pending_requests[0].waited_ms, 2000);
        let serialized = serde_json::to_string(&sample.readiness).unwrap();
        // Page URLs are captured data; the CLI redacts them when it emits.
        assert!(serialized.contains("api_key=key-canary&safe=ok"));
    }
}
