//! Navigation request issuance: plain `Page.navigate` for GET, or a
//! `Fetch.enable` intercept that reissues the document request with a custom
//! method and body for non-GET / body-bearing fetches. The same intercept
//! adds the caller's credential headers to requests for the target's origin
//! and to no other.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;
use url::Url;

use crate::sdk::cdp::ws_client::{CdpEvent, Connection};
use crate::sdk::fetch::deadline::FetchDeadline;
use crate::sdk::fetch::pipeline::request_opts::BodyPayload;
use crate::shared::error::{Error, ErrorCode};

use super::cdp_send;

/// Bound on one `Fetch.continueRequest` sent by the intercept. A paused
/// request that is not continued waits until the session detaches.
const CONTINUE_TIMEOUT: Duration = Duration::from_secs(5);

/// The caller's headers, split by scope (see
/// `PreparedRequestOptions::cdp_headers_by_scope`).
pub(super) struct NavigationHeaders<'a> {
    /// Sent everywhere through `Network.setExtraHTTPHeaders`. Listed here
    /// too so a request whose headers the intercept replaces keeps them.
    pub(super) everywhere: &'a BTreeMap<String, String>,
    /// Sent only to the requested URL's origin.
    pub(super) origin_only: &'a [(String, String)],
}

/// Keeps the request intercept running for the rest of the fetch. Dropping
/// it stops the intercept task; detaching the session (which every fetch
/// path does before it returns) disables `Fetch` and releases anything
/// still paused.
pub(super) struct RequestIntercept {
    task: JoinHandle<()>,
}

impl Drop for RequestIntercept {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct MainOverride {
    method: String,
    post_data_b64: Option<String>,
    form: bool,
    reply: oneshot::Sender<Result<(), Error>>,
}

/// Navigate to `url` using `method`. A GET without origin-scoped headers is
/// a plain `Page.navigate`. Otherwise `Fetch.enable` intercepts requests: the
/// first Document request is reissued with the method and body, and requests
/// for the target's origin get the origin-scoped headers. The returned
/// intercept must live as long as the page may still make requests.
#[allow(clippy::too_many_arguments)]
pub(super) async fn navigate_with_method(
    conn: &Arc<Connection>,
    session_id: &str,
    url: &str,
    method: &str,
    body_payload: &BodyPayload,
    headers: NavigationHeaders<'_>,
    timeout: Duration,
    deadline: &FetchDeadline,
) -> Result<(Value, Option<RequestIntercept>), Error> {
    let is_get = method.eq_ignore_ascii_case("GET");
    let has_body = !matches!(body_payload, BodyPayload::None);
    let override_main = !is_get || has_body;
    // An opaque origin (data:, about:blank) has no same-origin requests to
    // scope to, so the headers go nowhere.
    let origin = Url::parse(url)
        .ok()
        .map(|u| u.origin())
        .filter(|origin| origin.is_tuple() && !headers.origin_only.is_empty());

    if !override_main && origin.is_none() {
        return plain_navigate(conn, session_id, url, deadline)
            .await
            .map(|v| (v, None));
    }

    let mut patterns = Vec::new();
    if override_main {
        patterns.push(json!({"resourceType": "Document", "requestStage": "Request"}));
    }
    if let Some(origin) = &origin {
        // Only the target's origin is paused; every other request flows
        // untouched. The handler still compares origins before adding
        // anything.
        patterns.push(json!({
            "urlPattern": format!("{}/*", origin.ascii_serialization()),
            "requestStage": "Request",
        }));
    }
    // Subscribe BEFORE Fetch.enable so a paused request cannot arrive ahead
    // of the listener.
    let events = conn.subscribe();
    cdp_send(
        conn,
        session_id,
        "Fetch.enable",
        &json!({ "patterns": patterns }),
        "navigate",
        deadline,
    )
    .await?;

    let (reply_tx, reply_rx) = oneshot::channel();
    let main = override_main.then(|| MainOverride {
        method: method.to_string(),
        post_data_b64: post_data_b64(body_payload),
        form: matches!(body_payload, BodyPayload::Form(_)),
        reply: reply_tx,
    });
    let intercept = RequestIntercept {
        task: tokio::spawn(run_intercept(
            conn.clone(),
            events,
            session_id.to_string(),
            main,
            InterceptScope {
                origin,
                everywhere: headers.everywhere.clone(),
                origin_only: headers.origin_only.to_vec(),
            },
        )),
    };

    if !override_main {
        let navigated = plain_navigate(conn, session_id, url, deadline).await?;
        return Ok((navigated, Some(intercept)));
    }
    let nav_params = json!({"url": url});
    let navigate = cdp_send(
        conn,
        session_id,
        "Page.navigate",
        &nav_params,
        "navigate",
        deadline,
    );
    let reissued = async {
        match tokio::time::timeout(timeout, reply_rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(Error::new(
                ErrorCode::CdpUnavailable,
                "navigate: request intercept ended before the document request",
            )),
            Err(_) => Err(Error::new(ErrorCode::CdpTimeout, "wait_event: timed out")),
        }
    };
    let (nav_result, reissued) = tokio::join!(navigate, reissued);
    reissued?;
    let navigated = nav_result?;
    // With nothing left to scope, the intercept disabled Fetch itself.
    Ok((navigated, Some(intercept)))
}

async fn plain_navigate(
    conn: &Connection,
    session_id: &str,
    url: &str,
    deadline: &FetchDeadline,
) -> Result<Value, Error> {
    let mut navigate_params = json!({"url": url});
    if let Ok(frame_tree) = cdp_send(
        conn,
        session_id,
        "Page.getFrameTree",
        &json!({}),
        "navigate",
        deadline,
    )
    .await
        && let Some(frame_id) = frame_tree
            .pointer("/frameTree/frame/id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        && let Some(obj) = navigate_params.as_object_mut()
    {
        obj.insert("frameId".into(), json!(frame_id));
    }
    cdp_send(
        conn,
        session_id,
        "Page.navigate",
        &navigate_params,
        "navigate",
        deadline,
    )
    .await
}

fn post_data_b64(body_payload: &BodyPayload) -> Option<String> {
    use base64::Engine;
    match body_payload {
        BodyPayload::None => None,
        BodyPayload::Bytes(b) => Some(base64::engine::general_purpose::STANDARD.encode(b)),
        BodyPayload::Form(fields) => {
            let encoded = fields
                .iter()
                .map(|(k, v)| format!("{}={}", urlencoding_simple(k), urlencoding_simple(v)))
                .collect::<Vec<_>>()
                .join("&");
            Some(base64::engine::general_purpose::STANDARD.encode(encoded.as_bytes()))
        }
    }
}

struct InterceptScope {
    /// The target's origin when there are origin-only headers to add.
    origin: Option<url::Origin>,
    everywhere: BTreeMap<String, String>,
    origin_only: Vec<(String, String)>,
}

async fn run_intercept(
    conn: Arc<Connection>,
    mut events: broadcast::Receiver<CdpEvent>,
    session_id: String,
    mut main: Option<MainOverride>,
    scope: InterceptScope,
) {
    loop {
        let ev = match events.recv().await {
            Ok(ev) => ev,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if ev.method != "Fetch.requestPaused" || ev.session_id.as_deref() != Some(&session_id) {
            continue;
        }
        let is_document = ev.params.get("resourceType").and_then(Value::as_str) == Some("Document");
        let main_here = if is_document { main.take() } else { None };
        let (params, reply) = continue_params(&ev.params, main_here, &scope);
        if let Some(reply) = reply {
            // The reissued document request is answered in order, so the
            // caller learns whether its method and body went out.
            let sent = conn
                .send_timeout(
                    "Fetch.continueRequest",
                    &params,
                    Some(&session_id),
                    CONTINUE_TIMEOUT,
                )
                .await
                .map(|_| ());
            let _ = reply.send(sent);
            if scope.origin.is_none() {
                let _ = conn
                    .send_timeout(
                        "Fetch.disable",
                        &json!({}),
                        Some(&session_id),
                        CONTINUE_TIMEOUT,
                    )
                    .await;
                return;
            }
        } else {
            // Never block the loop on one reply: a backlog here pauses the
            // page and can overflow the event channel.
            let conn = conn.clone();
            let session_id = session_id.clone();
            tokio::spawn(async move {
                let _ = conn
                    .send_timeout(
                        "Fetch.continueRequest",
                        &params,
                        Some(&session_id),
                        CONTINUE_TIMEOUT,
                    )
                    .await;
            });
        }
    }
}

/// `Fetch.continueRequest` params for one paused request. Its `headers`
/// replace the request's own, so an override always starts from the headers
/// the request already carries.
fn continue_params(
    paused: &Value,
    main: Option<MainOverride>,
    scope: &InterceptScope,
) -> (Value, Option<oneshot::Sender<Result<(), Error>>>) {
    let request = &paused["request"];
    let mut params = json!({
        "requestId": paused.get("requestId").and_then(Value::as_str).unwrap_or(""),
    });
    let mut headers: Vec<(String, String)> = request
        .get("headers")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(name, value)| {
                    let value = value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_string);
                    (name.clone(), value)
                })
                .collect()
        })
        .unwrap_or_default();
    let mut replace_headers = false;
    let same_origin = scope.origin.as_ref().is_some_and(|origin| {
        request
            .get("url")
            .and_then(Value::as_str)
            .and_then(|url| Url::parse(url).ok())
            .is_some_and(|url| url.origin() == *origin)
    });
    if same_origin {
        for (name, value) in &scope.everywhere {
            set_header(&mut headers, name, value);
        }
        for (name, value) in &scope.origin_only {
            set_header(&mut headers, name, value);
        }
        replace_headers = true;
    }
    let mut reply = None;
    if let Some(main) = main {
        params["method"] = json!(main.method);
        if let Some(b64) = main.post_data_b64 {
            params["postData"] = json!(b64);
        }
        if main.form {
            for (name, value) in &scope.everywhere {
                set_header(&mut headers, name, value);
            }
            set_header(
                &mut headers,
                "Content-Type",
                "application/x-www-form-urlencoded",
            );
            replace_headers = true;
        }
        reply = Some(main.reply);
    }
    if replace_headers {
        params["headers"] = Value::Array(
            headers
                .into_iter()
                .map(|(name, value)| json!({"name": name, "value": value}))
                .collect(),
        );
    }
    (params, reply)
}

/// Set `name` to `value`, replacing any existing spelling of it.
fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
    headers.push((name.to_string(), value.to_string()));
}

fn urlencoding_simple(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            other => {
                out.push('%');
                out.push_str(&format!("{other:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(origin: Option<&str>) -> InterceptScope {
        InterceptScope {
            origin: origin.map(|o| Url::parse(o).unwrap().origin()),
            everywhere: BTreeMap::from([("X-Trace".to_string(), "t1".to_string())]),
            origin_only: vec![("authorization".to_string(), "Bearer secret".to_string())],
        }
    }

    fn paused(url: &str, headers: Value) -> Value {
        json!({"requestId": "r1", "resourceType": "Document", "request": {"url": url, "headers": headers}})
    }

    fn header<'a>(params: &'a Value, name: &str) -> Vec<&'a str> {
        params["headers"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|h| h["name"].as_str().unwrap().eq_ignore_ascii_case(name))
            .map(|h| h["value"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn form_post_keeps_the_request_headers_and_sets_one_content_type() {
        let (reply, _rx) = oneshot::channel();
        let main = MainOverride {
            method: "POST".into(),
            post_data_b64: Some("YT0x".into()),
            form: true,
            reply,
        };
        let (params, reply) = continue_params(
            &paused(
                "https://site.test/login",
                json!({"Accept-Language": "fr", "content-type": "text/plain", "Upgrade-Insecure-Requests": "1"}),
            ),
            Some(main),
            &scope(None),
        );
        assert!(reply.is_some());
        assert_eq!(params["method"], "POST");
        assert_eq!(params["postData"], "YT0x");
        assert_eq!(header(&params, "accept-language"), ["fr"]);
        assert_eq!(header(&params, "upgrade-insecure-requests"), ["1"]);
        assert_eq!(header(&params, "x-trace"), ["t1"]);
        assert_eq!(
            header(&params, "content-type"),
            ["application/x-www-form-urlencoded"]
        );
        // No origin scope: credentials are not added.
        assert!(header(&params, "authorization").is_empty());
    }

    #[test]
    fn credential_headers_go_to_the_target_origin_only() {
        let scope = scope(Some("https://site.test/start"));
        let (same, _) = continue_params(
            &paused("https://site.test/api", json!({"Accept": "*/*"})),
            None,
            &scope,
        );
        assert_eq!(header(&same, "authorization"), ["Bearer secret"]);
        assert_eq!(header(&same, "accept"), ["*/*"]);
        assert_eq!(header(&same, "x-trace"), ["t1"]);
        for other in [
            "https://cdn.test/app.js",
            "http://site.test/api",
            "https://site.test:8443/api",
            "https://site.test.evil.test/",
        ] {
            let (params, _) = continue_params(&paused(other, json!({})), None, &scope);
            assert_eq!(params, json!({"requestId": "r1"}), "{other}");
        }
    }
}
