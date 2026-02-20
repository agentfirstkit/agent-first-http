use crate::chunked;
use crate::config::response_headers_to_map;
use crate::types::*;
use base64::Engine;
use futures::StreamExt;
use reqwest::header::HeaderMap;
use reqwest::header::{
    ACCEPT_ENCODING, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, PROXY_AUTHORIZATION,
};
use reqwest::Method;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::App;

#[allow(clippy::too_many_arguments)]
pub async fn execute_request(
    app: &Arc<App>,
    id: String,
    tag: Option<String>,
    method: String,
    url: String,
    headers: HashMap<String, serde_json::Value>,
    body: Option<serde_json::Value>,
    body_base64: Option<String>,
    body_file: Option<String>,
    body_multipart: Option<Vec<MultipartPart>>,
    body_urlencoded: Option<Vec<UrlencodedPart>>,
    options: RequestOptions,
) {
    let start = Instant::now();

    // Parse URL early so we can extract host for header merge
    let parsed_url = match reqwest::Url::parse(&url) {
        Ok(u) => u,
        Err(e) => {
            send_error(
                app,
                Some(id),
                tag,
                ErrorInfo::invalid_request(format!("bad url: {e}")),
                start,
            )
            .await;
            return;
        }
    };
    let url_host = parsed_url.host_str().map(|h| {
        if let Some(port) = parsed_url.port() {
            format!("{h}:{port}")
        } else {
            h.to_string()
        }
    });

    // Read config snapshot
    let (opts, merged_headers, request_concurrency_limit) = {
        let config = app.config.read().await;
        let opts = config.resolve(&options);
        let merged = match config.merged_headers(&headers, url_host.as_deref()) {
            Ok(h) => h,
            Err(e) => {
                send_error(app, Some(id), tag, ErrorInfo::invalid_request(e), start).await;
                return;
            }
        };
        (opts, merged, config.request_concurrency_limit)
    };

    // response_save_resume requires an explicit response_save_file path.
    // Without it, the auto-save path is id-based and changes every request — resume is impossible.
    if opts.response_save_resume && opts.response_save_file.is_none() {
        send_error(
            app,
            Some(id),
            tag,
            ErrorInfo::invalid_request("response_save_resume requires response_save_file"),
            start,
        )
        .await;
        return;
    }

    // Build the HTTP client — use a one-off client when per-request TLS is set,
    // otherwise clone the shared pooled client.
    let client = if let Some(ref tls_override) = options.tls {
        let config = app.config.read().await;
        match config.build_client_for_request(tls_override) {
            Ok(c) => c,
            Err(e) => {
                send_error(
                    app,
                    Some(id),
                    tag,
                    ErrorInfo::invalid_request(format!("tls: {e}")),
                    start,
                )
                .await;
                return;
            }
        }
    } else {
        app.client.read().await.clone()
    };

    // WebSocket upgrade — branch before building HTTP request
    if options.upgrade.as_deref() == Some("websocket") {
        let cancel = CancellationToken::new();
        match reserve_request_id(app, &id, &cancel, request_concurrency_limit).await {
            ReserveIdResult::Reserved => {}
            ReserveIdResult::Duplicate => {
                send_error(
                    app,
                    Some(id),
                    tag,
                    ErrorInfo::invalid_request(
                        "id already in use by an active request or websocket connection",
                    ),
                    start,
                )
                .await;
                return;
            }
            ReserveIdResult::Overloaded => {
                send_error(
                    app,
                    Some(id),
                    tag,
                    ErrorInfo::overloaded(format!(
                        "too many in-flight requests (limit={request_concurrency_limit})"
                    )),
                    start,
                )
                .await;
                return;
            }
        }
        app.request_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        crate::websocket::open(app, id, tag, url, merged_headers, opts, cancel).await;
        return;
    }

    // Parse method
    let method = match method.parse::<Method>() {
        Ok(m) => m,
        Err(e) => {
            send_error(
                app,
                Some(id),
                tag,
                ErrorInfo::invalid_request(format!("bad method: {e}")),
                start,
            )
            .await;
            return;
        }
    };

    // Reserve request slot before heavy body work so limit enforcement
    // happens before file reads/allocations.
    let cancel = CancellationToken::new();
    match reserve_request_id(app, &id, &cancel, request_concurrency_limit).await {
        ReserveIdResult::Reserved => {}
        ReserveIdResult::Duplicate => {
            send_error(
                app,
                Some(id),
                tag,
                ErrorInfo::invalid_request(
                    "id already in use by an active request or websocket connection",
                ),
                start,
            )
            .await;
            return;
        }
        ReserveIdResult::Overloaded => {
            send_error(
                app,
                Some(id),
                tag,
                ErrorInfo::overloaded(format!(
                    "too many in-flight requests (limit={request_concurrency_limit})"
                )),
                start,
            )
            .await;
            return;
        }
    }

    // Build request body and determine content-type default
    let (req_body, ct_default) = match build_body(
        body,
        body_base64,
        body_file,
        &body_multipart,
        body_urlencoded,
    ) {
        Ok(v) => v,
        Err(e) => {
            release_request_id(app, &id).await;
            send_error(app, Some(id), tag, ErrorInfo::invalid_request(e), start).await;
            return;
        }
    };

    // Build the reqwest::RequestBuilder — no per-request timeout (idle timeout used instead)
    let mut builder = client.request(method.clone(), parsed_url);

    // Set headers
    builder = builder.headers(merged_headers.clone());

    // Track implicitly added headers for logging
    let mut implicit_headers: Vec<(String, String)> = Vec::new();

    // Set default Content-Type if not explicitly provided
    if let Some(ref ct) = ct_default {
        if !merged_headers.contains_key(CONTENT_TYPE) {
            builder = builder.header(CONTENT_TYPE, &**ct);
            implicit_headers.push(("Content-Type".to_string(), ct.to_string()));
        }
    }

    // Decompression: reqwest handles Accept-Encoding and decompression by default.
    // When decompress=false, tell the server not to compress (Accept-Encoding: identity).
    if !opts.response_decompress && !merged_headers.contains_key(ACCEPT_ENCODING) {
        builder = builder.header(ACCEPT_ENCODING, "identity");
        implicit_headers.push(("Accept-Encoding".to_string(), "identity".to_string()));
    } else if opts.response_decompress && !merged_headers.contains_key(ACCEPT_ENCODING) {
        // reqwest auto-adds Accept-Encoding: gzip, deflate, br — log it for transparency
        implicit_headers.push((
            "Accept-Encoding".to_string(),
            "gzip, deflate, br".to_string(),
        ));
    }

    // Resume download: if response_save_resume is set and the file already exists,
    // add Range: bytes=N- so the server can respond with 206 Partial Content.
    if opts.response_save_resume {
        if let Some(ref save_path) = opts.response_save_file {
            if let Ok(meta) = std::fs::metadata(save_path) {
                let offset = meta.len();
                if offset > 0 && !merged_headers.contains_key("range") {
                    let range_val = format!("bytes={offset}-");
                    builder = builder.header("Range", range_val.clone());
                    implicit_headers.push(("Range".to_string(), range_val));
                }
            }
        }
    }

    // Emit request log if category enabled and there are implicit headers
    if !implicit_headers.is_empty() {
        let config = app.config.read().await;
        if config.log.contains(&"request".to_string()) {
            let mut fields = HashMap::new();
            fields.insert("id".to_string(), serde_json::Value::String(id.clone()));
            let headers_obj: serde_json::Map<String, serde_json::Value> = implicit_headers
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect();
            fields.insert(
                "implicit_headers".to_string(),
                serde_json::Value::Object(headers_obj),
            );
            let _ = app.writer.try_send(Output::Log {
                event: "request".to_string(),
                fields,
            });
        }
        drop(config);
    }

    // Set body — multipart is handled separately (streaming, can't retry)
    let is_multipart = body_multipart.is_some();
    if let Some(body_multipart) = body_multipart {
        match build_multipart(body_multipart).await {
            Ok(form) => {
                builder = builder.multipart(form);
            }
            Err(e) => {
                release_request_id(app, &id).await;
                send_error(app, Some(id), tag, ErrorInfo::invalid_request(e), start).await;
                return;
            }
        }
    } else if let Some(body_bytes) = req_body {
        builder = builder.body(body_bytes);
    }

    app.request_count
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // Execute request
    let idle_dur = Duration::from_secs(opts.timeout_idle_s);
    let result: Result<(reqwest::Response, u32), ErrorInfo> = if is_multipart {
        // Multipart bodies are streaming — send directly, no retry/redirect
        let send_result = tokio::select! {
            result = tokio::time::timeout(idle_dur, builder.send()) => {
                match result {
                    Ok(r) => r,
                    Err(_) => {
                        release_request_id(app, &id).await;
                        send_error(app, Some(id), tag, ErrorInfo::request_timeout(format!("no response within {}s", opts.timeout_idle_s)), start).await;
                        return;
                    }
                }
            }
            _ = cancel.cancelled() => {
                release_request_id(app, &id).await;
                send_error(app, Some(id), tag, ErrorInfo::cancelled(), start).await;
                return;
            }
        };
        send_result
            .map_err(|e| ErrorInfo::from_reqwest(&e))
            .map(|response| (response, 0))
    } else {
        retry_redirect_loop(&client, builder, &opts, &cancel, &id, &tag, app, start).await
    };

    match result {
        Ok((response, redirects)) => {
            handle_response(
                app, &id, &tag, response, &opts, cancel, start, &method, redirects,
            )
            .await;
            release_request_id(app, &id).await;
        }
        Err(info) => {
            send_error(app, Some(id.clone()), tag, info, start).await;
            release_request_id(app, &id).await;
        }
    }
}

fn build_body(
    body: Option<serde_json::Value>,
    body_base64: Option<String>,
    body_file: Option<String>,
    body_multipart: &Option<Vec<MultipartPart>>,
    body_urlencoded: Option<Vec<UrlencodedPart>>,
) -> Result<(Option<Vec<u8>>, Option<&'static str>), String> {
    // Multipart is handled separately (async file reading)
    if body_multipart.is_some() {
        if body.is_some()
            || body_base64.is_some()
            || body_file.is_some()
            || body_urlencoded.is_some()
        {
            return Err(
                "body, body_base64, body_file, body_multipart, and body_urlencoded are mutually exclusive"
                    .to_string(),
            );
        }
        return Ok((None, None));
    }

    match (body, body_base64, body_file, body_urlencoded) {
        (None, None, None, None) => Ok((None, None)),
        (Some(b), None, None, None) => {
            if b.is_object() || b.is_array() {
                let json = serde_json::to_vec(&b).map_err(|e| format!("serialize body: {e}"))?;
                Ok((Some(json), Some("application/json")))
            } else if let Some(s) = b.as_str() {
                // String body: no default Content-Type — caller must specify
                Ok((Some(s.as_bytes().to_vec()), None))
            } else {
                // number, bool — afhttp serializes as JSON, so Content-Type is unambiguous
                let json = serde_json::to_vec(&b).map_err(|e| format!("serialize body: {e}"))?;
                Ok((Some(json), Some("application/json")))
            }
        }
        (None, Some(b64), None, None) => {
            // Binary body: no default Content-Type — caller knows the MIME type
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&b64)
                .map_err(|e| format!("decode body_base64: {e}"))?;
            Ok((Some(bytes), None))
        }
        (None, None, Some(path), None) => {
            // File body: no default Content-Type — caller knows the MIME type
            let bytes = std::fs::read(&path).map_err(|e| format!("read body_file '{path}': {e}"))?;
            Ok((Some(bytes), None))
        }
        (None, None, None, Some(parts)) => {
            let encoded = build_urlencoded_bytes(parts);
            Ok((Some(encoded), Some("application/x-www-form-urlencoded")))
        }
        _ => Err(
            "body, body_base64, body_file, body_multipart, and body_urlencoded are mutually exclusive"
                .to_string(),
        ),
    }
}

fn build_urlencoded_bytes(parts: Vec<UrlencodedPart>) -> Vec<u8> {
    parts
        .iter()
        .map(|p| {
            format!(
                "{}={}",
                percent_encode_form(&p.name),
                percent_encode_form(&p.value)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
        .into_bytes()
}

/// Percent-encode a string for application/x-www-form-urlencoded.
/// Unreserved: `*`, `-`, `.`, `0-9`, `A-Z`, `_`, `a-z`. Space → `+`. All else → `%XX`.
fn percent_encode_form(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'*' | b'-' | b'.' | b'0'..=b'9' | b'A'..=b'Z' | b'_' | b'a'..=b'z' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

async fn build_multipart(parts: Vec<MultipartPart>) -> Result<reqwest::multipart::Form, String> {
    let mut form = reqwest::multipart::Form::new();

    for part in parts {
        if let Some(value) = part.value {
            let mut p = reqwest::multipart::Part::text(value);
            if let Some(filename) = part.filename {
                p = p.file_name(filename);
            }
            if let Some(ct) = part.content_type {
                p = p.mime_str(&ct).map_err(|e| format!("invalid mime: {e}"))?;
            }
            form = form.part(part.name, p);
        } else if let Some(b64) = part.value_base64 {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&b64)
                .map_err(|e| format!("decode multipart base64: {e}"))?;
            let mut p = reqwest::multipart::Part::bytes(bytes);
            if let Some(filename) = part.filename {
                p = p.file_name(filename);
            }
            if let Some(ct) = part.content_type {
                p = p.mime_str(&ct).map_err(|e| format!("invalid mime: {e}"))?;
            }
            form = form.part(part.name, p);
        } else if let Some(path) = part.file {
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|e| format!("read multipart file '{path}': {e}"))?;
            let filename = part.filename.unwrap_or_else(|| {
                Path::new(&path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("file")
                    .to_string()
            });
            let mut p = reqwest::multipart::Part::bytes(bytes).file_name(filename);
            if let Some(ct) = part.content_type {
                p = p.mime_str(&ct).map_err(|e| format!("invalid mime: {e}"))?;
            }
            form = form.part(part.name, p);
        } else {
            return Err(format!(
                "multipart part '{}' needs value, value_base64, or file",
                part.name
            ));
        }
    }
    Ok(form)
}

#[allow(clippy::too_many_arguments)]
async fn retry_redirect_loop(
    client: &reqwest::Client,
    initial_builder: reqwest::RequestBuilder,
    opts: &ResolvedOptions,
    cancel: &CancellationToken,
    id: &str,
    tag: &Option<String>,
    app: &Arc<App>,
    overall_start: Instant,
) -> Result<(reqwest::Response, u32), ErrorInfo> {
    // Build the initial request
    let request = initial_builder
        .build()
        .map_err(ErrorInfo::invalid_request)?;

    let mut current_url = request.url().clone();
    let mut current_method = request.method().clone();
    let mut current_headers = request.headers().clone();
    let mut current_body = request
        .body()
        .and_then(|b| b.as_bytes())
        .map(|b| b.to_vec());

    let mut redirects: u32 = 0;

    loop {
        // Retry loop for current URL
        let response = retry_loop(
            client,
            current_method.clone(),
            current_url.clone(),
            current_headers.clone(),
            current_body.clone(),
            opts,
            cancel,
            id,
            tag,
            app,
            overall_start,
        )
        .await?;

        let status = response.status().as_u16();

        // Check for redirect
        if matches!(status, 301 | 302 | 303 | 307 | 308) && opts.response_redirect > 0 {
            redirects += 1;
            if redirects > opts.response_redirect {
                return Err(ErrorInfo::too_many_redirects(opts.response_redirect));
            }

            if let Some(location) = response.headers().get("location") {
                let loc_str = location.to_str().map_err(|_| {
                    ErrorInfo::invalid_request(
                        "location header contains non-ASCII bytes — cannot follow redirect",
                    )
                })?;
                let new_url = current_url.join(loc_str).map_err(|e| {
                    ErrorInfo::invalid_request(format!("bad redirect url '{loc_str}': {e}"))
                })?;

                // Log redirect if enabled
                {
                    let config = app.config.read().await;
                    if config.log.contains(&"redirect".to_string()) {
                        let mut fields = HashMap::new();
                        fields.insert("id".to_string(), serde_json::Value::String(id.to_string()));
                        fields.insert(
                            "status".to_string(),
                            serde_json::Value::Number(status.into()),
                        );
                        fields.insert(
                            "from".to_string(),
                            serde_json::Value::String(current_url.to_string()),
                        );
                        fields.insert(
                            "to".to_string(),
                            serde_json::Value::String(new_url.to_string()),
                        );
                        let _ = app.writer.try_send(Output::Log {
                            event: "redirect".to_string(),
                            fields,
                        });
                    }
                }

                if is_cross_origin(&current_url, &new_url) {
                    strip_sensitive_redirect_headers(&mut current_headers);
                }
                if status == 303 && current_method != Method::GET && current_method != Method::HEAD
                {
                    current_method = Method::GET;
                    current_body = None;
                    current_headers.remove(CONTENT_TYPE);
                    current_headers.remove(CONTENT_LENGTH);
                }

                current_url = new_url;
                continue;
            }
        }

        return Ok((response, redirects));
    }
}

#[allow(clippy::too_many_arguments)]
async fn retry_loop(
    client: &reqwest::Client,
    method: Method,
    url: reqwest::Url,
    headers: reqwest::header::HeaderMap,
    body: Option<Vec<u8>>,
    opts: &ResolvedOptions,
    cancel: &CancellationToken,
    id: &str,
    tag: &Option<String>,
    app: &Arc<App>,
    _overall_start: Instant,
) -> Result<reqwest::Response, ErrorInfo> {
    let max_attempts = opts.retry + 1;
    let idle_dur = Duration::from_secs(opts.timeout_idle_s);

    for attempt in 0..max_attempts {
        if cancel.is_cancelled() {
            return Err(ErrorInfo::cancelled());
        }

        let mut builder = client
            .request(method.clone(), url.clone())
            .headers(headers.clone());
        if let Some(ref b) = body {
            builder = builder.body(b.clone());
        }

        // Use idle timeout for the send phase (connection + response headers)
        let send_result = tokio::select! {
            result = tokio::time::timeout(idle_dur, builder.send()) => {
                match result {
                    Ok(r) => r.map_err(|e| ErrorInfo::from_reqwest(&e)),
                    Err(_) => Err(ErrorInfo::request_timeout(format!("no response within {}s", opts.timeout_idle_s))),
                }
            }
            _ = cancel.cancelled() => {
                return Err(ErrorInfo::cancelled());
            }
        };
        match send_result {
            Ok(resp) => {
                let status = resp.status().as_u16();

                // Check retry_on_status
                if opts.retry_on_status.contains(&status) && attempt + 1 < max_attempts {
                    // Parse Retry-After header for delay
                    let backoff_ms = backoff_delay_ms(opts.retry_base_delay_ms, attempt);
                    let delay_ms = if let Some(ra) = resp.headers().get("retry-after") {
                        parse_retry_after(ra).unwrap_or(backoff_ms).min(300_000)
                    } else {
                        backoff_ms
                    };

                    // Log retry if enabled
                    {
                        let config = app.config.read().await;
                        if config.log.contains(&"retry".to_string()) {
                            let mut fields = HashMap::new();
                            fields.insert("id".into(), serde_json::Value::String(id.to_string()));
                            if let Some(t) = tag {
                                fields.insert("tag".into(), serde_json::Value::String(t.clone()));
                            }
                            if let Some(host) = url.host_str() {
                                fields.insert(
                                    "host".into(),
                                    serde_json::Value::String(host.to_string()),
                                );
                            }
                            fields.insert(
                                "reason".into(),
                                serde_json::Value::String(format!("status {status}")),
                            );
                            fields.insert(
                                "attempt".into(),
                                serde_json::Value::Number((attempt + 1).into()),
                            );
                            fields.insert(
                                "delay_ms".into(),
                                serde_json::Value::Number(delay_ms.into()),
                            );
                            let _ = app.writer.try_send(Output::Log {
                                event: "retry".to_string(),
                                fields,
                            });
                        }
                    }

                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => {}
                        _ = cancel.cancelled() => {
                            return Err(ErrorInfo::cancelled());
                        }
                    }
                    continue;
                }

                return Ok(resp);
            }
            Err(info) => {
                if !info.retryable || attempt + 1 >= max_attempts {
                    return Err(info);
                }

                // Log retry if enabled
                {
                    let config = app.config.read().await;
                    if config.log.contains(&"retry".to_string()) {
                        let mut fields = HashMap::new();
                        fields.insert("id".into(), serde_json::Value::String(id.to_string()));
                        if let Some(t) = tag {
                            fields.insert("tag".into(), serde_json::Value::String(t.clone()));
                        }
                        if let Some(host) = url.host_str() {
                            fields
                                .insert("host".into(), serde_json::Value::String(host.to_string()));
                        }
                        fields.insert(
                            "reason".into(),
                            serde_json::Value::String(info.error.clone()),
                        );
                        fields.insert(
                            "attempt".into(),
                            serde_json::Value::Number((attempt + 1).into()),
                        );
                        fields.insert(
                            "delay_ms".into(),
                            serde_json::Value::Number(
                                backoff_delay_ms(opts.retry_base_delay_ms, attempt).into(),
                            ),
                        );
                        let _ = app.writer.try_send(Output::Log {
                            event: "retry".to_string(),
                            fields,
                        });
                    }
                }

                // Exponential backoff
                let delay_ms = backoff_delay_ms(opts.retry_base_delay_ms, attempt);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => {}
                    _ = cancel.cancelled() => {
                        return Err(ErrorInfo::cancelled());
                    }
                }
            }
        }
    }

    unreachable!()
}

/// Parse Retry-After header value (seconds integer) to milliseconds.
fn parse_retry_after(value: &reqwest::header::HeaderValue) -> Option<u64> {
    value.to_str().ok()?.parse::<u64>().ok().map(|s| s * 1000)
}

/// Exponential backoff delay with overflow protection and a hard 5-minute cap.
/// `base_ms * 2^min(attempt, 10)`, capped at 300_000 ms.
fn backoff_delay_ms(base_ms: u64, attempt: u32) -> u64 {
    let exponent = attempt.min(10);
    base_ms.saturating_mul(1u64 << exponent).min(300_000)
}

#[allow(clippy::too_many_arguments)]
async fn handle_response(
    app: &Arc<App>,
    id: &str,
    tag: &Option<String>,
    response: reqwest::Response,
    opts: &ResolvedOptions,
    cancel: CancellationToken,
    start: Instant,
    method: &Method,
    redirects: u32,
) {
    let status = response.status().as_u16();
    let resp_headers = match response_headers_to_map(response.headers()) {
        Ok(h) => h,
        Err(e) => {
            send_error(
                app,
                Some(id.to_string()),
                tag.clone(),
                ErrorInfo::invalid_response(e),
                start,
            )
            .await;
            return;
        }
    };
    let http_version = format_http_version(response.version());
    let remote_addr = response.remote_addr().map(|a| a.to_string());

    // Check for chunked mode or save_to
    if opts.chunked {
        chunked::handle_chunked_response(
            app,
            id,
            tag,
            response,
            opts,
            cancel,
            start,
            http_version,
            redirects,
        )
        .await;
        return;
    }

    if opts.response_save_file.is_some() {
        chunked::handle_download(
            app,
            id,
            tag,
            response,
            opts,
            cancel,
            start,
            http_version,
            redirects,
        )
        .await;
        return;
    }

    // Check if empty body (204, 304, HEAD)
    let is_empty_body = status == 204 || status == 304 || *method == Method::HEAD;

    if is_empty_body {
        let _ = app
            .writer
            .send(Output::Response {
                id: id.to_string(),
                tag: tag.clone(),
                status,
                headers: resp_headers,
                body: None,
                body_base64: None,
                body_file: None,
                body_parse_failed: false,
                trace: Trace {
                    duration_ms: start.elapsed().as_millis() as u64,
                    http_version: Some(http_version),
                    remote_addr,
                    sent_bytes: None,
                    received_bytes: Some(0),
                    redirects: Some(redirects),
                    chunks: None,
                },
            })
            .await;
        return;
    }

    // Buffered: stream body with per-chunk idle timeout
    let content_type = resp_headers
        .get("content-type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let idle_dur = Duration::from_secs(opts.timeout_idle_s);
    let mut stream = response.bytes_stream();
    let mut body_buf = Vec::new();

    loop {
        tokio::select! {
            result = tokio::time::timeout(idle_dur, stream.next()) => {
                match result {
                    Ok(Some(Ok(chunk))) => {
                        body_buf.extend_from_slice(&chunk);
                        // Check max_response_bytes guard inline
                        if let Some(max) = opts.response_max_bytes {
                            if body_buf.len() as u64 > max {
                                send_error(app, Some(id.to_string()), tag.clone(), ErrorInfo::response_too_large(max), start).await;
                                return;
                            }
                        }
                    }
                    Ok(Some(Err(e))) => {
                        send_error(app, Some(id.to_string()), tag.clone(), ErrorInfo::chunk_disconnected(e), start).await;
                        return;
                    }
                    Ok(None) => break, // stream complete
                    Err(_) => {
                        send_error(app, Some(id.to_string()), tag.clone(), ErrorInfo::request_timeout(format!("no data received for {}s", opts.timeout_idle_s)), start).await;
                        return;
                    }
                }
            }
            _ = cancel.cancelled() => {
                send_error(app, Some(id.to_string()), tag.clone(), ErrorInfo::cancelled(), start).await;
                return;
            }
        }
    }

    let received_bytes = body_buf.len() as u64;

    // Check size vs response_save_above_bytes — if exceeds, save to response_save_dir
    if received_bytes > opts.response_save_above_bytes {
        let config = app.config.read().await;
        let file_path = auto_download_path(&config.response_save_dir, id);
        drop(config);

        if let Err(e) = tokio::fs::write(&file_path, &body_buf).await {
            send_error(
                app,
                Some(id.to_string()),
                tag.clone(),
                ErrorInfo::invalid_request(format!("write download: {e}")),
                start,
            )
            .await;
            return;
        }

        let output = Output::Response {
            id: id.to_string(),
            tag: tag.clone(),
            status,
            headers: resp_headers,
            body: None,
            body_base64: None,
            body_file: Some(file_path.clone()),
            body_parse_failed: false,
            trace: Trace {
                duration_ms: start.elapsed().as_millis() as u64,
                http_version: Some(http_version),
                remote_addr,
                sent_bytes: None,
                received_bytes: Some(received_bytes),
                redirects: Some(redirects),
                chunks: None,
            },
        };

        // Write sidecar JSON
        let sidecar_path = sidecar_path_for(&file_path);
        if let Ok(json) = serde_json::to_string(&output) {
            let _ = tokio::fs::write(&sidecar_path, json).await;
        }

        let _ = app.writer.send(output).await;
        return;
    }

    // Determine body representation.
    // Rule: only put a string in JSON if the bytes are valid UTF-8.
    // If bytes are not valid UTF-8, fall back to base64 regardless of Content-Type.
    let is_json = content_type.contains("json");
    let is_text = content_type.starts_with("text/") || is_json;

    let mut body_parse_failed = false;
    let (body_val, body_b64) = if is_json && opts.response_parse_json {
        match serde_json::from_slice::<serde_json::Value>(&body_buf) {
            Ok(v) => (Some(v), None),
            Err(_) => {
                // JSON parse failed — try string, fall back to base64 if not valid UTF-8
                body_parse_failed = true;
                match String::from_utf8(body_buf) {
                    Ok(s) => (Some(serde_json::Value::String(s)), None),
                    Err(e) => {
                        let b64 = base64::engine::general_purpose::STANDARD.encode(e.as_bytes());
                        (None, Some(b64))
                    }
                }
            }
        }
    } else if is_text {
        // text/* — valid UTF-8 → string, invalid → base64
        match String::from_utf8(body_buf) {
            Ok(s) => (Some(serde_json::Value::String(s)), None),
            Err(e) => {
                let b64 = base64::engine::general_purpose::STANDARD.encode(e.as_bytes());
                (None, Some(b64))
            }
        }
    } else {
        // Binary
        let b64 = base64::engine::general_purpose::STANDARD.encode(&body_buf);
        (None, Some(b64))
    };

    let _ = app
        .writer
        .send(Output::Response {
            id: id.to_string(),
            tag: tag.clone(),
            status,
            headers: resp_headers,
            body: body_val,
            body_base64: body_b64,
            body_file: None,
            body_parse_failed,
            trace: Trace {
                duration_ms: start.elapsed().as_millis() as u64,
                http_version: Some(http_version),
                remote_addr,
                sent_bytes: None,
                received_bytes: Some(received_bytes),
                redirects: Some(redirects),
                chunks: None,
            },
        })
        .await;
}

fn format_http_version(v: reqwest::Version) -> String {
    match v {
        reqwest::Version::HTTP_2 => "h2".to_string(),
        reqwest::Version::HTTP_11 => "h1".to_string(),
        reqwest::Version::HTTP_10 => "h1".to_string(),
        reqwest::Version::HTTP_3 => "h3".to_string(),
        _ => format!("{v:?}"),
    }
}

/// Helper to emit an error via the writer channel.
async fn send_error(
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

enum ReserveIdResult {
    Reserved,
    Duplicate,
    Overloaded,
}

async fn reserve_request_id(
    app: &Arc<App>,
    id: &str,
    cancel: &CancellationToken,
    request_concurrency_limit: u64,
) -> ReserveIdResult {
    let mut in_flight = app.in_flight.write().await;
    if in_flight.contains_key(id) {
        return ReserveIdResult::Duplicate;
    }
    if request_concurrency_limit > 0 && in_flight.len() as u64 >= request_concurrency_limit {
        return ReserveIdResult::Overloaded;
    }
    in_flight.insert(id.to_string(), cancel.clone());
    ReserveIdResult::Reserved
}

async fn release_request_id(app: &Arc<App>, id: &str) {
    let mut in_flight = app.in_flight.write().await;
    in_flight.remove(id);
}

fn is_cross_origin(from: &reqwest::Url, to: &reqwest::Url) -> bool {
    let from_host = from.host_str().map(|h| h.to_ascii_lowercase());
    let to_host = to.host_str().map(|h| h.to_ascii_lowercase());
    from.scheme() != to.scheme()
        || from_host != to_host
        || from.port_or_known_default() != to.port_or_known_default()
}

fn strip_sensitive_redirect_headers(headers: &mut HeaderMap) {
    headers.remove(AUTHORIZATION);
    headers.remove(COOKIE);
    headers.remove(PROXY_AUTHORIZATION);
}

fn auto_download_path(response_save_dir: &str, id: &str) -> String {
    let file_name = sanitize_file_name(id);
    Path::new(response_save_dir)
        .join(file_name)
        .to_string_lossy()
        .to_string()
}

fn sanitize_file_name(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "request".to_string()
    } else {
        out
    }
}

fn sidecar_path_for(path: &str) -> String {
    let mut sidecar = std::ffi::OsString::from(path);
    sidecar.push(".json");
    PathBuf::from(sidecar).to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::RuntimeConfig;
    use reqwest::header::{HeaderValue, AUTHORIZATION, COOKIE, PROXY_AUTHORIZATION};
    use tokio::sync::{mpsc, RwLock};
    use tokio_util::sync::CancellationToken;

    async fn test_app() -> Arc<App> {
        let save_dir = std::env::temp_dir()
            .join(format!("afhttp-handler-test-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let config = RuntimeConfig::new(save_dir);
        let client = config.build_client().expect("build client");
        let (tx, _rx) = mpsc::channel(16);
        Arc::new(App {
            config: RwLock::new(config),
            client: RwLock::new(client),
            writer: tx,
            in_flight: RwLock::new(HashMap::new()),
            ws_connections: RwLock::new(HashMap::new()),
            request_count: std::sync::atomic::AtomicU64::new(0),
            start_time: Instant::now(),
        })
    }

    #[test]
    fn body_builders_cover_variants() {
        let (body, ct) =
            build_body(Some(serde_json::json!({"a":1})), None, None, &None, None).expect("json");
        assert_eq!(body, Some(br#"{"a":1}"#.to_vec()));
        assert_eq!(ct, Some("application/json"));

        let (body, ct) = build_body(
            Some(serde_json::Value::String("hello".to_string())),
            None,
            None,
            &None,
            None,
        )
        .expect("string");
        assert_eq!(body, Some(b"hello".to_vec()));
        assert_eq!(ct, None);

        let (body, ct) =
            build_body(Some(serde_json::json!(123)), None, None, &None, None).expect("number");
        assert_eq!(body, Some(b"123".to_vec()));
        assert_eq!(ct, Some("application/json"));

        let (body, ct) =
            build_body(None, Some("aGk=".to_string()), None, &None, None).expect("base64");
        assert_eq!(body, Some(b"hi".to_vec()));
        assert_eq!(ct, None);

        let file = std::env::temp_dir()
            .join(format!("afhttp-body-{}.txt", std::process::id()))
            .to_string_lossy()
            .into_owned();
        std::fs::write(&file, b"file-bytes").expect("write");
        let (body, ct) = build_body(None, None, Some(file.clone()), &None, None).expect("file");
        assert_eq!(body, Some(b"file-bytes".to_vec()));
        assert_eq!(ct, None);
        let _ = std::fs::remove_file(file);

        let (body, ct) = build_body(
            None,
            None,
            None,
            &None,
            Some(vec![UrlencodedPart {
                name: "a b".to_string(),
                value: "x+y".to_string(),
            }]),
        )
        .expect("urlencoded");
        assert_eq!(body, Some(b"a+b=x%2By".to_vec()));
        assert_eq!(ct, Some("application/x-www-form-urlencoded"));

        assert!(build_body(
            Some(serde_json::json!({"x":1})),
            Some("aA==".to_string()),
            None,
            &None,
            None
        )
        .is_err());

        assert!(build_body(
            Some(serde_json::json!({"x":1})),
            None,
            None,
            &Some(vec![MultipartPart {
                name: "f".to_string(),
                value: Some("v".to_string()),
                value_base64: None,
                file: None,
                filename: None,
                content_type: None,
            }]),
            None
        )
        .is_err());
    }

    #[test]
    fn regression_multipart_body_fields_are_mutually_exclusive() {
        let err = build_body(
            Some(serde_json::json!({"x":1})),
            None,
            None,
            &Some(vec![MultipartPart {
                name: "f".to_string(),
                value: Some("v".to_string()),
                value_base64: None,
                file: None,
                filename: None,
                content_type: None,
            }]),
            None,
        )
        .expect_err("multipart + body must fail");
        assert!(err.contains("mutually exclusive"));
    }

    #[test]
    fn encoding_helpers_work() {
        assert_eq!(percent_encode_form("abc-_.123"), "abc-_.123");
        assert_eq!(percent_encode_form("a b+c"), "a+b%2Bc");
        let encoded = build_urlencoded_bytes(vec![
            UrlencodedPart {
                name: "x y".to_string(),
                value: "1+2".to_string(),
            },
            UrlencodedPart {
                name: "k".to_string(),
                value: "".to_string(),
            },
        ]);
        assert_eq!(encoded, b"x+y=1%2B2&k=".to_vec());
    }

    #[tokio::test]
    async fn build_multipart_handles_text_file_base64_and_errors() {
        let file = std::env::temp_dir()
            .join(format!("afhttp-mp-{}.bin", std::process::id()))
            .to_string_lossy()
            .into_owned();
        tokio::fs::write(&file, b"bytes").await.expect("write");
        let ok = build_multipart(vec![
            MultipartPart {
                name: "t".to_string(),
                value: Some("hello".to_string()),
                value_base64: None,
                file: None,
                filename: None,
                content_type: None,
            },
            MultipartPart {
                name: "b".to_string(),
                value: None,
                value_base64: Some("aGk=".to_string()),
                file: None,
                filename: Some("x.bin".to_string()),
                content_type: Some("application/octet-stream".to_string()),
            },
            MultipartPart {
                name: "f".to_string(),
                value: None,
                value_base64: None,
                file: Some(file.clone()),
                filename: None,
                content_type: None,
            },
        ])
        .await;
        assert!(ok.is_ok());

        let err = build_multipart(vec![MultipartPart {
            name: "bad".to_string(),
            value: None,
            value_base64: Some("%%%".to_string()),
            file: None,
            filename: None,
            content_type: None,
        }])
        .await;
        assert!(err.is_err());

        let err = build_multipart(vec![MultipartPart {
            name: "bad".to_string(),
            value: Some("x".to_string()),
            value_base64: None,
            file: None,
            filename: None,
            content_type: Some("not-a-mime".to_string()),
        }])
        .await;
        assert!(err.is_err());

        let err = build_multipart(vec![MultipartPart {
            name: "missing".to_string(),
            value: None,
            value_base64: None,
            file: None,
            filename: None,
            content_type: None,
        }])
        .await;
        assert!(err.is_err());
        let _ = tokio::fs::remove_file(file).await;
    }

    #[tokio::test]
    async fn reserve_and_release_request_id() {
        let app = test_app().await;
        let tok1 = CancellationToken::new();
        let tok2 = CancellationToken::new();
        let r = reserve_request_id(&app, "id1", &tok1, 1).await;
        assert!(matches!(r, ReserveIdResult::Reserved));
        let r = reserve_request_id(&app, "id1", &tok2, 1).await;
        assert!(matches!(r, ReserveIdResult::Duplicate));
        let r = reserve_request_id(&app, "id2", &tok2, 1).await;
        assert!(matches!(r, ReserveIdResult::Overloaded));
        release_request_id(&app, "id1").await;
        let r = reserve_request_id(&app, "id2", &tok2, 1).await;
        assert!(matches!(r, ReserveIdResult::Reserved));
    }

    #[tokio::test]
    async fn send_error_emits_output_error() {
        let save_dir = std::env::temp_dir()
            .join(format!("afhttp-handler-err-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let config = RuntimeConfig::new(save_dir);
        let client = config.build_client().expect("client");
        let (tx, mut rx) = mpsc::channel(4);
        let app = App {
            config: RwLock::new(config),
            client: RwLock::new(client),
            writer: tx,
            in_flight: RwLock::new(HashMap::new()),
            ws_connections: RwLock::new(HashMap::new()),
            request_count: std::sync::atomic::AtomicU64::new(0),
            start_time: Instant::now(),
        };
        send_error(
            &app,
            Some("id1".to_string()),
            Some("tag1".to_string()),
            ErrorInfo::invalid_request("bad"),
            Instant::now(),
        )
        .await;
        let out = rx.recv().await.expect("output");
        assert!(matches!(out, Output::Error { .. }));
    }

    #[test]
    fn parse_retry_after_and_backoff_delay() {
        let hv = HeaderValue::from_static("12");
        assert_eq!(parse_retry_after(&hv), Some(12_000));
        let bad = HeaderValue::from_static("abc");
        assert_eq!(parse_retry_after(&bad), None);

        assert_eq!(backoff_delay_ms(100, 0), 100);
        assert_eq!(backoff_delay_ms(100, 1), 200);
        assert_eq!(backoff_delay_ms(100, 10), 102_400);
        assert_eq!(backoff_delay_ms(100, 100), 102_400);
        assert_eq!(backoff_delay_ms(1_000_000, 10), 300_000);
    }

    #[test]
    fn format_http_version_maps_known_values() {
        assert_eq!(format_http_version(reqwest::Version::HTTP_2), "h2");
        assert_eq!(format_http_version(reqwest::Version::HTTP_11), "h1");
        assert_eq!(format_http_version(reqwest::Version::HTTP_10), "h1");
        assert_eq!(format_http_version(reqwest::Version::HTTP_3), "h3");
    }

    #[test]
    fn cross_origin_detection_and_header_stripping() {
        let a = reqwest::Url::parse("https://example.com/a").expect("url");
        let b = reqwest::Url::parse("https://example.com/b").expect("url");
        let c = reqwest::Url::parse("http://example.com/b").expect("url");
        let d = reqwest::Url::parse("https://api.example.com/b").expect("url");
        let e = reqwest::Url::parse("https://example.com:8443/b").expect("url");
        assert!(!is_cross_origin(&a, &b));
        assert!(is_cross_origin(&a, &c));
        assert!(is_cross_origin(&a, &d));
        assert!(is_cross_origin(&a, &e));

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer x"));
        headers.insert(COOKIE, HeaderValue::from_static("a=1"));
        headers.insert(PROXY_AUTHORIZATION, HeaderValue::from_static("Basic x"));
        headers.insert("x-safe", HeaderValue::from_static("ok"));
        strip_sensitive_redirect_headers(&mut headers);
        assert!(headers.get(AUTHORIZATION).is_none());
        assert!(headers.get(COOKIE).is_none());
        assert!(headers.get(PROXY_AUTHORIZATION).is_none());
        assert_eq!(
            headers.get("x-safe").and_then(|v| v.to_str().ok()),
            Some("ok")
        );
    }

    #[test]
    fn path_helpers_sanitize_and_sidecar() {
        assert_eq!(sanitize_file_name("abc-_.123"), "abc-_.123");
        assert_eq!(sanitize_file_name("a/b:c?d"), "a_b_c_d");
        assert_eq!(sanitize_file_name(""), "request");

        let auto = auto_download_path("/tmp/afhttpttp", "a/b");
        assert!(auto.ends_with("/tmp/afhttpttp/a_b"));

        let side = sidecar_path_for("/tmp/x.bin");
        assert_eq!(side, "/tmp/x.bin.json");
    }
}
