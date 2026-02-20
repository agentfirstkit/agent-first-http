use crate::types::*;
use crate::App;
use base64::Engine;
use futures::{SinkExt, StreamExt};
use reqwest::header::HeaderMap;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

pub async fn open(
    app: &Arc<App>,
    id: String,
    tag: Option<String>,
    url: String,
    merged_headers: HeaderMap,
    _opts: ResolvedOptions,
    cancel: CancellationToken,
) {
    let start = Instant::now();

    // Warn when TLS settings from config are present — tokio-tungstenite uses its own
    // TLS stack (rustls + native roots) and does not inherit afh's reqwest TLS config.
    // This warning is treated as a `request` log category event.
    {
        let config = app.config.read().await;
        let tls = &config.tls;
        let tls_customized = tls.insecure
            || tls.cacert_pem.is_some()
            || tls.cacert_file.is_some()
            || tls.cert_pem.is_some()
            || tls.cert_file.is_some()
            || tls.key_pem_secret.is_some()
            || tls.key_file.is_some();
        let request_log_enabled = config.log.iter().any(|c| c == "request");

        if tls_customized && request_log_enabled {
            let _ = app.writer.try_send(Output::Log {
                event: "websocket_tls_config_ignored".to_string(),
                fields: {
                    let mut f = HashMap::new();
                    f.insert("id".to_string(), Value::String(id.clone()));
                    f.insert(
                        "message".to_string(),
                        Value::String(
                            "custom TLS config (insecure/cacert/cert/key) is not applied to \
                             WebSocket connections; WebSocket TLS uses system roots only"
                                .to_string(),
                        ),
                    );
                    f
                },
            });
        }
    }

    // Build tungstenite request with merged headers
    let request = match build_request(&url, &merged_headers) {
        Ok(r) => r,
        Err(e) => {
            emit_error(
                app,
                Some(id.clone()),
                tag,
                ErrorInfo::invalid_request(e),
                start,
            )
            .await;
            cleanup(app, &id).await;
            return;
        }
    };

    // Connect (cancel-aware)
    let connect_result = tokio::select! {
        result = tokio_tungstenite::connect_async(request) => result,
        _ = cancel.cancelled() => {
            emit_error(app, Some(id.clone()), tag, ErrorInfo::cancelled(), start).await;
            cleanup(app, &id).await;
            return;
        }
    };

    let (ws_stream, response) = match connect_result {
        Ok(pair) => pair,
        Err(e) => {
            emit_error(app, Some(id.clone()), tag, classify_error(&e), start).await;
            cleanup(app, &id).await;
            return;
        }
    };

    // Emit chunk_start with 101 and upgrade response headers
    let resp_headers = match headers_to_map(response.headers()) {
        Ok(h) => h,
        Err(e) => {
            drop(ws_stream);
            emit_error(
                app,
                Some(id.clone()),
                tag,
                ErrorInfo::invalid_response(e),
                start,
            )
            .await;
            cleanup(app, &id).await;
            return;
        }
    };
    let _ = app
        .writer
        .send(Output::ChunkStart {
            id: id.clone(),
            tag: tag.clone(),
            status: 101,
            headers: resp_headers,
            content_length_bytes: None,
        })
        .await;

    // Per-connection channel for outgoing messages
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<WsCommand>();
    {
        let mut ws_conns = app.ws_connections.write().await;
        ws_conns.insert(id.clone(), cmd_tx);
    }

    let (mut write, mut read) = ws_stream.split();
    let mut chunk_count: u32 = 0;

    loop {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        chunk_count += 1;
                        let data_str = text.to_string();
                        // If parse_json: try to parse and re-serialize — keeps number/bool types.
                        // Either way, data is a String in ChunkData (consistent with HTTP chunked).
                        let _ = app.writer.send(Output::ChunkData {
                            id: id.clone(),
                            data: Some(data_str),
                            data_base64: None,
                        }).await;
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        chunk_count += 1;
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes[..]);
                        let _ = app.writer.send(Output::ChunkData {
                            id: id.clone(),
                            data: None,
                            data_base64: Some(b64),
                        }).await;
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        // Server closed or stream ended — normal shutdown
                        break;
                    }
                    Some(Ok(_)) => {
                        // Ping / Pong / Frame — handled by tungstenite automatically
                    }
                    Some(Err(e)) => {
                        emit_error(
                            app,
                            Some(id.clone()),
                            tag.clone(),
                            ErrorInfo::chunk_disconnected(e),
                            start,
                        ).await;
                        cleanup(app, &id).await;
                        return;
                    }
                }
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(WsCommand::Send { data, data_base64 }) => {
                        match build_message(data, data_base64) {
                            Ok(msg) => {
                                if let Err(e) = write.send(msg).await {
                                    emit_error(
                                        app,
                                        Some(id.clone()),
                                        tag.clone(),
                                        ErrorInfo::chunk_disconnected(e),
                                        start,
                                    ).await;
                                    cleanup(app, &id).await;
                                    return;
                                }
                            }
                            Err(e) => {
                                // Bad message — report error, keep connection alive
                                emit_error(
                                    app,
                                    Some(id.clone()),
                                    tag.clone(),
                                    ErrorInfo::invalid_request(e),
                                    start,
                                ).await;
                            }
                        }
                    }
                    Some(WsCommand::Close) | None => {
                        let _ = write.send(Message::Close(None)).await;
                        break;
                    }
                }
            }
            _ = cancel.cancelled() => {
                // cancel command — send graceful close frame
                let _ = write.send(Message::Close(None)).await;
                break;
            }
        }
    }

    cleanup(app, &id).await;
    let _ = app
        .writer
        .send(Output::ChunkEnd {
            id: id.clone(),
            tag: tag.clone(),
            body_file: None,
            trace: Trace {
                duration_ms: start.elapsed().as_millis() as u64,
                http_version: Some("ws".to_string()),
                remote_addr: None,
                sent_bytes: None,
                received_bytes: None,
                redirects: None,
                chunks: Some(chunk_count),
            },
        })
        .await;
}

fn build_request(
    url: &str,
    headers: &HeaderMap,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, String> {
    // Parse the URI and use IntoClientRequest to get a properly-initialised
    // WebSocket request (with Upgrade, Connection, Sec-WebSocket-Key, etc.)
    // then layer our custom headers on top.
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let uri: tokio_tungstenite::tungstenite::http::Uri =
        url.parse().map_err(|e| format!("invalid ws url: {e}"))?;
    let mut request = uri
        .into_client_request()
        .map_err(|e| format!("build websocket request: {e}"))?;
    for (name, value) in headers {
        request.headers_mut().insert(name, value.clone());
    }
    Ok(request)
}

fn build_message(data: Option<Value>, data_base64: Option<String>) -> Result<Message, String> {
    match (data, data_base64) {
        (Some(v), None) => {
            let text = if let Some(s) = v.as_str() {
                s.to_string()
            } else {
                serde_json::to_string(&v).map_err(|e| format!("serialize data: {e}"))?
            };
            Ok(Message::Text(text.into()))
        }
        (None, Some(b64)) => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&b64)
                .map_err(|e| format!("decode data_base64: {e}"))?;
            Ok(Message::Binary(bytes.into()))
        }
        (None, None) => Err("send requires data or data_base64".to_string()),
        _ => Err("data and data_base64 are mutually exclusive".to_string()),
    }
}

fn classify_error(e: &tokio_tungstenite::tungstenite::Error) -> ErrorInfo {
    let msg = e.to_string().to_lowercase();
    if msg.contains("dns") || msg.contains("resolve") || msg.contains("name") {
        ErrorInfo {
            error_code: "dns_failed",
            error: e.to_string(),
            retryable: true,
        }
    } else if msg.contains("tls") || msg.contains("ssl") || msg.contains("certificate") {
        ErrorInfo {
            error_code: "tls_error",
            error: e.to_string(),
            retryable: false,
        }
    } else if msg.contains("timeout") {
        ErrorInfo {
            error_code: "connect_timeout",
            error: e.to_string(),
            retryable: true,
        }
    } else {
        ErrorInfo {
            error_code: "connect_refused",
            error: e.to_string(),
            retryable: true,
        }
    }
}

fn headers_to_map(
    headers: &tokio_tungstenite::tungstenite::http::HeaderMap,
) -> Result<HashMap<String, Value>, String> {
    let mut map = HashMap::new();
    for (name, value) in headers {
        let k = name.as_str().to_lowercase();
        let v = value
            .to_str()
            .map_err(|_| format!("server sent non-ASCII bytes in header '{k}'"))?;
        map.insert(k, Value::String(v.to_string()));
    }
    Ok(map)
}

async fn emit_error(
    app: &App,
    id: Option<String>,
    tag: Option<String>,
    info: ErrorInfo,
    start: Instant,
) {
    let _ = app
        .writer
        .send(make_error(
            id,
            tag,
            info,
            Trace::error_only(start.elapsed().as_millis() as u64),
        ))
        .await;
}

async fn cleanup(app: &Arc<App>, id: &str) {
    app.in_flight.write().await.remove(id);
    app.ws_connections.write().await.remove(id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio_tungstenite::tungstenite;

    #[test]
    fn build_request_accepts_valid_ws_url_and_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-test", "ok".parse().expect("header value"));
        let req = build_request("ws://example.com/socket", &headers).expect("request");
        assert_eq!(req.uri().to_string(), "ws://example.com/socket");
        assert_eq!(
            req.headers().get("x-test").and_then(|v| v.to_str().ok()),
            Some("ok")
        );
    }

    #[test]
    fn build_request_rejects_invalid_url() {
        let headers = HeaderMap::new();
        let err = build_request("ws://exa mple.com", &headers).expect_err("invalid url");
        assert!(err.contains("invalid ws url") || err.contains("build websocket request"));
    }

    #[test]
    fn build_message_text_json_binary_and_errors() {
        match build_message(Some(Value::String("hi".to_string())), None).expect("text") {
            Message::Text(t) => assert_eq!(t.to_string(), "hi"),
            other => panic!("unexpected message: {other:?}"),
        }

        match build_message(Some(json!({"a":1})), None).expect("json text") {
            Message::Text(t) => assert_eq!(t.to_string(), r#"{"a":1}"#),
            other => panic!("unexpected message: {other:?}"),
        }

        match build_message(None, Some("aGk=".to_string())).expect("binary") {
            Message::Binary(b) => assert_eq!(&b[..], b"hi"),
            other => panic!("unexpected message: {other:?}"),
        }

        let err = build_message(None, None).expect_err("missing data");
        assert!(err.contains("requires data"));
        let err = build_message(Some(Value::Null), Some("aA==".to_string())).expect_err("both");
        assert!(err.contains("mutually exclusive"));
        let err = build_message(None, Some("%%%".to_string())).expect_err("bad b64");
        assert!(err.contains("decode data_base64"));
    }

    #[test]
    fn classify_error_maps_messages() {
        let dns = tungstenite::Error::Io(std::io::Error::other("dns resolve failure for host"));
        let info = classify_error(&dns);
        assert_eq!(info.error_code, "dns_failed");
        assert!(info.retryable);

        let tls = tungstenite::Error::Io(std::io::Error::other("certificate verify failed"));
        let info = classify_error(&tls);
        assert_eq!(info.error_code, "tls_error");
        assert!(!info.retryable);

        let timeout =
            tungstenite::Error::Io(std::io::Error::new(std::io::ErrorKind::TimedOut, "timeout"));
        let info = classify_error(&timeout);
        assert_eq!(info.error_code, "connect_timeout");
        assert!(info.retryable);

        let other = tungstenite::Error::Io(std::io::Error::other("connection reset"));
        let info = classify_error(&other);
        assert_eq!(info.error_code, "connect_refused");
        assert!(info.retryable);
    }

    #[test]
    fn headers_to_map_lowercases_and_rejects_invalid() {
        let mut headers = tungstenite::http::HeaderMap::new();
        headers.insert("X-Test", "value".parse().expect("header value"));
        let mapped = headers_to_map(&headers).expect("map");
        assert_eq!(
            mapped.get("x-test"),
            Some(&Value::String("value".to_string()))
        );

        let mut bad = tungstenite::http::HeaderMap::new();
        bad.insert(
            "X-Bad",
            tungstenite::http::HeaderValue::from_bytes(&[0xFF]).expect("header bytes"),
        );
        assert!(headers_to_map(&bad).is_err());
    }
}
