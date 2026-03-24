use crate::config::{parse_content_length, response_headers_to_map};
use crate::types::*;
use base64::Engine;
use futures::StreamExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::App;

/// Handle a chunked streaming response. Splits incoming bytes on delimiter
/// and emits ChunkStart, ChunkData..., ChunkEnd.
#[allow(clippy::too_many_arguments)]
pub async fn handle_chunked_response(
    app: &Arc<App>,
    id: &str,
    tag: &Option<String>,
    response: reqwest::Response,
    opts: &ResolvedOptions,
    cancel: CancellationToken,
    start: Instant,
    http_version: String,
    redirects: u32,
) {
    let status = response.status().as_u16();
    let resp_headers = match response_headers_to_map(response.headers()) {
        Ok(h) => h,
        Err(e) => {
            let _ = app
                .writer
                .send(make_error(
                    Some(id.to_string()),
                    tag.clone(),
                    ErrorInfo::invalid_response(e),
                    Trace::error_only(start.elapsed().as_millis() as u64),
                ))
                .await;
            return;
        }
    };
    let content_length_bytes = parse_content_length(&resp_headers);

    let _ = app
        .writer
        .send(Output::ChunkStart {
            id: id.to_string(),
            tag: tag.clone(),
            status,
            headers: resp_headers,
            content_length_bytes,
        })
        .await;

    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut chunk_count: u32 = 0;
    let mut received_bytes: u64 = 0;
    let delimiter = opts.chunked_delimiter.as_deref();
    let idle_dur = Duration::from_secs(opts.timeout_idle_s);

    loop {
        tokio::select! {
            result = tokio::time::timeout(idle_dur, stream.next()) => {
                match result {
                    Ok(Some(Ok(bytes))) => {
                        received_bytes += bytes.len() as u64;

                        // Check max_response_bytes guard
                        if let Some(max) = opts.response_max_bytes {
                            if received_bytes > max {
                                let _ = app.writer.send(make_error(
                                    Some(id.to_string()),
                                    tag.clone(),
                                    ErrorInfo::response_too_large(max),
                                    Trace {
                                        duration_ms: start.elapsed().as_millis() as u64,
                                        http_version: None,
                                        remote_addr: None,
                                        sent_bytes: None,
                                        received_bytes: Some(received_bytes),
                                        redirects: Some(redirects),
                                        chunks: Some(chunk_count),
                                    },
                                )).await;
                                return;
                            }
                        }

                        match delimiter {
                            None => {
                                // Raw mode: emit each HTTP chunk as-is, base64 encoded
                                chunk_count += 1;
                                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                                let _ = app.writer.send(Output::ChunkData {
                                    id: id.to_string(),
                                    data: None,
                                    data_base64: Some(b64),
                                }).await;
                            }
                            Some(delim) => {
                                // Delimiter mode: buffer and split
                                buffer.extend_from_slice(&bytes);
                                let delim_bytes = delim.as_bytes();
                                while let Some(pos) = find_delimiter(&buffer, delim_bytes) {
                                    let chunk_data: Vec<u8> = buffer.drain(..pos).collect();
                                    // Skip the delimiter itself
                                    buffer.drain(..delim_bytes.len());
                                    // Don't emit empty chunks
                                    if !chunk_data.is_empty() {
                                        chunk_count += 1;
                                        let (data, data_base64) = match String::from_utf8(chunk_data) {
                                            Ok(text) => (Some(text), None),
                                            Err(e) => {
                                                let b64 = base64::engine::general_purpose::STANDARD.encode(e.as_bytes());
                                                (None, Some(b64))
                                            }
                                        };
                                        let _ = app.writer.send(Output::ChunkData {
                                            id: id.to_string(),
                                            data,
                                            data_base64,
                                        }).await;
                                    }
                                }
                            }
                        }
                    }
                    Ok(Some(Err(e))) => {
                        let _ = app.writer.send(make_error(
                            Some(id.to_string()),
                            tag.clone(),
                            ErrorInfo::chunk_disconnected(e),
                            Trace {
                                duration_ms: start.elapsed().as_millis() as u64,
                                http_version: None,
                                remote_addr: None,
                                sent_bytes: None,
                                received_bytes: Some(received_bytes),
                                redirects: Some(redirects),
                                chunks: Some(chunk_count),
                            },
                        )).await;
                        return;
                    }
                    Ok(None) => {
                        // Stream complete — flush remaining buffer
                        if delimiter.is_some() && !buffer.is_empty() {
                            chunk_count += 1;
                            let (data, data_base64) = match String::from_utf8(buffer) {
                                Ok(text) => (Some(text), None),
                                Err(e) => {
                                    let b64 = base64::engine::general_purpose::STANDARD.encode(e.as_bytes());
                                    (None, Some(b64))
                                }
                            };
                            let _ = app.writer.send(Output::ChunkData {
                                id: id.to_string(),
                                data,
                                data_base64,
                            }).await;
                        }
                        break;
                    }
                    Err(_) => {
                        // Idle timeout
                        let _ = app.writer.send(make_error(
                            Some(id.to_string()),
                            tag.clone(),
                            ErrorInfo::request_timeout(format!("no data received for {}s", opts.timeout_idle_s)),
                            Trace {
                                duration_ms: start.elapsed().as_millis() as u64,
                                http_version: None,
                                remote_addr: None,
                                sent_bytes: None,
                                received_bytes: Some(received_bytes),
                                redirects: Some(redirects),
                                chunks: Some(chunk_count),
                            },
                        )).await;
                        return;
                    }
                }
            }
            _ = cancel.cancelled() => {
                let _ = app.writer.send(make_error(
                    Some(id.to_string()),
                    tag.clone(),
                    ErrorInfo::cancelled(),
                    Trace {
                        duration_ms: start.elapsed().as_millis() as u64,
                        http_version: None,
                        remote_addr: None,
                        sent_bytes: None,
                        received_bytes: Some(received_bytes),
                        redirects: Some(redirects),
                        chunks: Some(chunk_count),
                    },
                )).await;
                return;
            }
        }
    }

    let _ = app
        .writer
        .send(Output::ChunkEnd {
            id: id.to_string(),
            tag: tag.clone(),
            body_file: None,
            trace: Trace {
                duration_ms: start.elapsed().as_millis() as u64,
                http_version: Some(http_version),
                remote_addr: None,
                sent_bytes: None,
                received_bytes: Some(received_bytes),
                redirects: Some(redirects),
                chunks: Some(chunk_count),
            },
        })
        .await;
}

/// Handle a file download response. Streams to file with optional progress reporting.
#[allow(clippy::too_many_arguments)]
pub async fn handle_download(
    app: &Arc<App>,
    id: &str,
    tag: &Option<String>,
    response: reqwest::Response,
    opts: &ResolvedOptions,
    cancel: CancellationToken,
    start: Instant,
    http_version: String,
    redirects: u32,
) {
    let status = response.status().as_u16();
    let resp_headers = match response_headers_to_map(response.headers()) {
        Ok(h) => h,
        Err(e) => {
            let _ = app
                .writer
                .send(make_error(
                    Some(id.to_string()),
                    tag.clone(),
                    ErrorInfo::invalid_response(e),
                    Trace::error_only(start.elapsed().as_millis() as u64),
                ))
                .await;
            return;
        }
    };
    let content_length_bytes = parse_content_length(&resp_headers);

    let save_path = match &opts.response_save_file {
        Some(p) => p.clone(),
        None => {
            // Auto-download to response_save_dir
            let config = app.config.read().await;
            auto_download_path(&config.response_save_dir, id)
        }
    };

    let _ = app
        .writer
        .send(Output::ChunkStart {
            id: id.to_string(),
            tag: tag.clone(),
            status,
            headers: resp_headers.clone(),
            content_length_bytes,
        })
        .await;

    // Open file for writing (or appending if resume)
    let mut file_offset: u64 = 0;
    let file = if opts.response_save_resume && status == 206 {
        // Server supports range — append to existing file
        match tokio::fs::OpenOptions::new()
            .append(true)
            .open(&save_path)
            .await
        {
            Ok(f) => {
                file_offset = f.metadata().await.map(|m| m.len()).unwrap_or(0);
                f
            }
            Err(e) => {
                let _ = app
                    .writer
                    .send(make_error(
                        Some(id.to_string()),
                        tag.clone(),
                        ErrorInfo::invalid_request(format!("open file: {e}")),
                        Trace::error_only(start.elapsed().as_millis() as u64),
                    ))
                    .await;
                return;
            }
        }
    } else {
        // Create/overwrite
        match tokio::fs::File::create(&save_path).await {
            Ok(f) => f,
            Err(e) => {
                let _ = app
                    .writer
                    .send(make_error(
                        Some(id.to_string()),
                        tag.clone(),
                        ErrorInfo::invalid_request(format!("create file: {e}")),
                        Trace::error_only(start.elapsed().as_millis() as u64),
                    ))
                    .await;
                return;
            }
        }
    };

    let mut writer = tokio::io::BufWriter::new(file);
    let mut stream = response.bytes_stream();
    let mut received_bytes: u64 = file_offset;
    let mut last_progress_bytes: u64 = received_bytes;
    let mut last_progress_time = Instant::now();
    let idle_dur = Duration::from_secs(opts.timeout_idle_s);
    let progress_log_enabled = {
        let config = app.config.read().await;
        config.log.contains(&"progress".to_string())
    };

    // Total bytes for progress calculation (includes file offset for resume)
    let total_bytes_for_progress = content_length_bytes.map(|cl| cl + file_offset);

    loop {
        tokio::select! {
            result = tokio::time::timeout(idle_dur, stream.next()) => {
                match result {
                    Ok(Some(Ok(bytes))) => {
                        received_bytes += bytes.len() as u64;

                        // Check max_response_bytes guard
                        if let Some(max) = opts.response_max_bytes {
                            if received_bytes > max {
                                let _ = writer.flush().await;
                                let _ = app.writer.send(make_error(
                                    Some(id.to_string()),
                                    tag.clone(),
                                    ErrorInfo::response_too_large(max),
                                    Trace::error_only(start.elapsed().as_millis() as u64),
                                )).await;
                                return;
                            }
                        }

                        if let Err(e) = writer.write_all(&bytes).await {
                            let _ = app.writer.send(make_error(
                                Some(id.to_string()),
                                tag.clone(),
                                ErrorInfo::chunk_disconnected(format!("write error: {e}")),
                                Trace::error_only(start.elapsed().as_millis() as u64),
                            )).await;
                            return;
                        }

                        // Check progress triggers (only if progress log enabled)
                        if progress_log_enabled {
                            let mut emit = false;
                            if opts.progress_bytes > 0 && received_bytes - last_progress_bytes >= opts.progress_bytes {
                                emit = true;
                            }
                            if opts.progress_ms > 0 && last_progress_time.elapsed().as_millis() as u64 >= opts.progress_ms {
                                emit = true;
                            }
                            if emit {
                                last_progress_bytes = received_bytes;
                                last_progress_time = Instant::now();

                                let mut fields: Vec<(&str, serde_json::Value)> = vec![
                                    ("id", serde_json::Value::String(id.to_string())),
                                    ("received_bytes", serde_json::json!(received_bytes)),
                                ];
                                if let Some(total) = total_bytes_for_progress {
                                    if total > 0 {
                                        fields.push(("total_bytes", serde_json::json!(total)));
                                        let pct = ((received_bytes as f64 / total as f64) * 100.0).min(100.0) as u8;
                                        fields.push(("percent", serde_json::json!(pct)));
                                        let elapsed_ms = start.elapsed().as_millis() as u64;
                                        if received_bytes > file_offset && elapsed_ms > 0 {
                                            let downloaded = received_bytes - file_offset;
                                            let remaining = total.saturating_sub(received_bytes);
                                            let bytes_per_ms = downloaded as f64 / elapsed_ms as f64;
                                            if bytes_per_ms > 0.0 {
                                                fields.push(("eta_s", serde_json::json!((remaining as f64 / bytes_per_ms / 1000.0) as u64)));
                                            }
                                        }
                                    }
                                }
                                let _ = app.writer.try_send(make_log("progress", fields));
                            }
                        }
                    }
                    Ok(Some(Err(e))) => {
                        let _ = writer.flush().await;
                        let _ = app.writer.send(make_error(
                            Some(id.to_string()),
                            tag.clone(),
                            ErrorInfo::chunk_disconnected(e),
                            Trace::error_only(start.elapsed().as_millis() as u64),
                        )).await;
                        return;
                    }
                    Ok(None) => break,
                    Err(_) => {
                        // Idle timeout
                        let _ = writer.flush().await;
                        let _ = app.writer.send(make_error(
                            Some(id.to_string()),
                            tag.clone(),
                            ErrorInfo::request_timeout(format!("no data received for {}s", opts.timeout_idle_s)),
                            Trace::error_only(start.elapsed().as_millis() as u64),
                        )).await;
                        return;
                    }
                }
            }
            _ = cancel.cancelled() => {
                let _ = writer.flush().await;
                let _ = app.writer.send(make_error(
                    Some(id.to_string()),
                    tag.clone(),
                    ErrorInfo::cancelled(),
                    Trace::error_only(start.elapsed().as_millis() as u64),
                )).await;
                return;
            }
        }
    }

    // Emit one final progress event on completion if thresholds never fired.
    if progress_log_enabled && received_bytes != last_progress_bytes {
        let mut fields: Vec<(&str, serde_json::Value)> = vec![
            ("id", serde_json::Value::String(id.to_string())),
            ("received_bytes", serde_json::json!(received_bytes)),
        ];
        if let Some(total) = total_bytes_for_progress {
            if total > 0 {
                fields.push(("total_bytes", serde_json::json!(total)));
                let pct = ((received_bytes as f64 / total as f64) * 100.0).min(100.0) as u8;
                fields.push(("percent", serde_json::json!(pct)));
                let elapsed_ms = start.elapsed().as_millis() as u64;
                if received_bytes > file_offset && elapsed_ms > 0 {
                    let downloaded = received_bytes - file_offset;
                    let remaining = total.saturating_sub(received_bytes);
                    let bytes_per_ms = downloaded as f64 / elapsed_ms as f64;
                    if bytes_per_ms > 0.0 {
                        fields.push((
                            "eta_s",
                            serde_json::json!((remaining as f64 / bytes_per_ms / 1000.0) as u64),
                        ));
                    }
                }
            }
        }
        let _ = app.writer.try_send(make_log("progress", fields));
    }

    if let Err(e) = writer.flush().await {
        let _ = app
            .writer
            .send(make_error(
                Some(id.to_string()),
                tag.clone(),
                ErrorInfo::chunk_disconnected(format!("flush error: {e}")),
                Trace::error_only(start.elapsed().as_millis() as u64),
            ))
            .await;
        return;
    }

    // Write sidecar JSON for auto-downloads
    if opts.response_save_file.is_none() {
        let sidecar = serde_json::json!({
            "id": id,
            "status": status,
            "headers": resp_headers,
            "body_file": save_path,
            "received_bytes": received_bytes,
        });
        let sidecar_path = sidecar_path_for(&save_path);
        let _ = tokio::fs::write(&sidecar_path, sidecar.to_string()).await;
    }

    let _ = app
        .writer
        .send(Output::ChunkEnd {
            id: id.to_string(),
            tag: tag.clone(),
            body_file: Some(save_path),
            trace: Trace {
                duration_ms: start.elapsed().as_millis() as u64,
                http_version: Some(http_version),
                remote_addr: None,
                sent_bytes: None,
                received_bytes: Some(received_bytes),
                redirects: Some(redirects),
                chunks: None,
            },
        })
        .await;
}

fn find_delimiter(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
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
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::types::RuntimeConfig;
    use std::collections::HashMap;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::{mpsc, RwLock};

    async fn test_app() -> (Arc<App>, mpsc::Receiver<Output>) {
        let save_dir = std::env::temp_dir()
            .join(format!("afhttp-chunked-test-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let config = RuntimeConfig::new(save_dir);
        let client = config.build_client().expect("build client");
        let (tx, rx) = mpsc::channel(64);
        let app = Arc::new(App {
            config: RwLock::new(config),
            client: RwLock::new(client),
            writer: tx,
            in_flight: RwLock::new(HashMap::new()),
            ws_connections: RwLock::new(HashMap::new()),
            request_count: std::sync::atomic::AtomicU64::new(0),
            start_time: Instant::now(),
        });
        (app, rx)
    }

    fn opts(chunked_delimiter: Option<String>) -> ResolvedOptions {
        ResolvedOptions {
            timeout_idle_s: 1,
            retry: 0,
            response_redirect: 0,
            response_parse_json: true,
            response_decompress: true,
            response_save_resume: false,
            chunked: true,
            chunked_delimiter,
            response_save_file: None,
            progress_bytes: 0,
            progress_ms: 0,
            response_save_above_bytes: 1024,
            retry_base_delay_ms: 100,
            retry_on_status: vec![],
            response_max_bytes: None,
        }
    }

    async fn serve_once(raw_response: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let _ = socket.write_all(&raw_response).await;
                let _ = socket.shutdown().await;
            }
        });
        format!("http://{addr}")
    }

    async fn get_response(url: &str) -> reqwest::Response {
        reqwest::Client::new()
            .get(url)
            .send()
            .await
            .expect("request")
    }

    #[tokio::test]
    async fn handle_chunked_response_delimiter_mode() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\ncontent-length: 12\r\n\r\n6\r\nhello\n\r\n6\r\nworld\n\r\n0\r\n\r\n".to_vec();
        let url = serve_once(raw).await;
        let response = get_response(&url).await;
        let (app, mut rx) = test_app().await;

        handle_chunked_response(
            &app,
            "id1",
            &Some("t".to_string()),
            response,
            &opts(Some("\n".to_string())),
            CancellationToken::new(),
            Instant::now(),
            "h1".to_string(),
            0,
        )
        .await;

        let o1 = rx.recv().await.expect("chunk_start");
        assert!(matches!(o1, Output::ChunkStart { .. }));
        let o2 = rx.recv().await.expect("chunk_data1");
        let o3 = rx.recv().await.expect("chunk_data2");
        let o4 = rx.recv().await.expect("chunk_end");
        assert!(matches!(o2, Output::ChunkData { .. }));
        assert!(matches!(o3, Output::ChunkData { .. }));
        assert!(matches!(o4, Output::ChunkEnd { .. }));
    }

    #[tokio::test]
    async fn handle_chunked_response_raw_mode_and_limits() {
        let raw =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n".to_vec();
        let url = serve_once(raw).await;
        let response = get_response(&url).await;
        let (app, mut rx) = test_app().await;
        handle_chunked_response(
            &app,
            "id2",
            &None,
            response,
            &opts(None),
            CancellationToken::new(),
            Instant::now(),
            "h1".to_string(),
            0,
        )
        .await;
        let _ = rx.recv().await;
        let o2 = rx.recv().await.expect("chunk_data");
        match o2 {
            Output::ChunkData { data_base64, .. } => assert!(data_base64.is_some()),
            _ => panic!("expected chunk_data"),
        }

        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n"
            .to_vec();
        let url = serve_once(raw).await;
        let response = get_response(&url).await;
        let (app, mut rx) = test_app().await;
        let mut o = opts(Some("\n".to_string()));
        o.response_max_bytes = Some(2);
        handle_chunked_response(
            &app,
            "id3",
            &None,
            response,
            &o,
            CancellationToken::new(),
            Instant::now(),
            "h1".to_string(),
            0,
        )
        .await;
        let _ = rx.recv().await;
        let err = rx.recv().await.expect("error");
        assert!(matches!(err, Output::Error { .. }));
    }

    #[tokio::test]
    async fn handle_chunked_response_cancel_and_timeout() {
        let raw =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n".to_vec();
        let url = serve_once(raw).await;
        let response = get_response(&url).await;
        let (app, mut rx) = test_app().await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        handle_chunked_response(
            &app,
            "id4",
            &None,
            response,
            &opts(Some("\n".to_string())),
            cancel,
            Instant::now(),
            "h1".to_string(),
            0,
        )
        .await;
        let _ = rx.recv().await;
        let e = rx.recv().await.expect("error");
        assert!(matches!(e, Output::Error { .. }));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 512];
                let _ = socket.read(&mut buf).await;
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                    .await;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
        let response = get_response(&format!("http://{addr}")).await;
        let (app, mut rx) = test_app().await;
        let mut o = opts(Some("\n".to_string()));
        o.timeout_idle_s = 0;
        handle_chunked_response(
            &app,
            "id5",
            &None,
            response,
            &o,
            CancellationToken::new(),
            Instant::now(),
            "h1".to_string(),
            0,
        )
        .await;
        let _ = rx.recv().await;
        let e = rx.recv().await.expect("error");
        assert!(matches!(e, Output::Error { .. }));
    }

    #[tokio::test]
    async fn handle_download_save_resume_and_errors() {
        let body = b"abc";
        let raw = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).expect("utf8")
        )
        .into_bytes();
        let url = serve_once(raw).await;
        let response = get_response(&url).await;
        let (app, mut rx) = test_app().await;
        let file = std::env::temp_dir()
            .join(format!("afhttp-dl-{}.txt", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let mut o = opts(Some("\n".to_string()));
        o.response_save_file = Some(file.clone());
        o.chunked = false;
        handle_download(
            &app,
            "id6",
            &None,
            response,
            &o,
            CancellationToken::new(),
            Instant::now(),
            "h1".to_string(),
            0,
        )
        .await;
        let _ = rx.recv().await;
        let end = rx.recv().await.expect("chunk_end");
        assert!(matches!(end, Output::ChunkEnd { .. }));
        let saved = tokio::fs::read(&file).await.expect("saved file");
        assert_eq!(saved, body);
        let _ = tokio::fs::remove_file(&file).await;

        let resume_file = std::env::temp_dir()
            .join(format!("afhttp-dl-resume-{}.txt", std::process::id()))
            .to_string_lossy()
            .into_owned();
        tokio::fs::write(&resume_file, b"ab").await.expect("seed");
        let raw = b"HTTP/1.1 206 Partial Content\r\nContent-Length: 2\r\n\r\ncd".to_vec();
        let url = serve_once(raw).await;
        let response = get_response(&url).await;
        let (app, mut rx) = test_app().await;
        let mut o = opts(Some("\n".to_string()));
        o.response_save_file = Some(resume_file.clone());
        o.response_save_resume = true;
        handle_download(
            &app,
            "id7",
            &None,
            response,
            &o,
            CancellationToken::new(),
            Instant::now(),
            "h1".to_string(),
            0,
        )
        .await;
        let _ = rx.recv().await;
        let _ = rx.recv().await;
        let saved = tokio::fs::read(&resume_file).await.expect("resume file");
        assert_eq!(saved, b"abcd");
        let _ = tokio::fs::remove_file(&resume_file).await;

        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nabcd".to_vec();
        let url = serve_once(raw).await;
        let response = get_response(&url).await;
        let (app, mut rx) = test_app().await;
        let mut o = opts(Some("\n".to_string()));
        o.response_save_file = Some(
            std::env::temp_dir()
                .join(format!("afhttp-dl-max-{}.txt", std::process::id()))
                .to_string_lossy()
                .into_owned(),
        );
        o.response_max_bytes = Some(1);
        handle_download(
            &app,
            "id8",
            &None,
            response,
            &o,
            CancellationToken::new(),
            Instant::now(),
            "h1".to_string(),
            0,
        )
        .await;
        let _ = rx.recv().await;
        let e = rx.recv().await.expect("error");
        assert!(matches!(e, Output::Error { .. }));
    }

    #[tokio::test]
    async fn handle_chunked_and_download_invalid_headers_and_stream_error() {
        let raw = b"HTTP/1.1 200 OK\r\nX-Bad: \xFF\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n"
            .to_vec();
        let url = serve_once(raw).await;
        let response = get_response(&url).await;
        let (app, mut rx) = test_app().await;
        handle_chunked_response(
            &app,
            "id9",
            &None,
            response,
            &opts(Some("\n".to_string())),
            CancellationToken::new(),
            Instant::now(),
            "h1".to_string(),
            0,
        )
        .await;
        let e = rx.recv().await.expect("error");
        assert!(matches!(e, Output::Error { .. }));

        let raw =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nZ\r\nbad\r\n0\r\n\r\n".to_vec();
        let url = serve_once(raw).await;
        let response = get_response(&url).await;
        let (app, mut rx) = test_app().await;
        handle_chunked_response(
            &app,
            "id10",
            &None,
            response,
            &opts(Some("\n".to_string())),
            CancellationToken::new(),
            Instant::now(),
            "h1".to_string(),
            0,
        )
        .await;
        let _ = rx.recv().await; // chunk_start
        let e = rx.recv().await.expect("stream error");
        assert!(matches!(e, Output::Error { .. }));

        let raw = b"HTTP/1.1 200 OK\r\nX-Bad: \xFF\r\nContent-Length: 1\r\n\r\na".to_vec();
        let url = serve_once(raw).await;
        let response = get_response(&url).await;
        let (app, mut rx) = test_app().await;
        let mut o = opts(Some("\n".to_string()));
        o.chunked = false;
        o.response_save_file = Some(
            std::env::temp_dir()
                .join(format!("afhttp-dl-bad-{}.txt", std::process::id()))
                .to_string_lossy()
                .into_owned(),
        );
        handle_download(
            &app,
            "id11",
            &None,
            response,
            &o,
            CancellationToken::new(),
            Instant::now(),
            "h1".to_string(),
            0,
        )
        .await;
        let e = rx.recv().await.expect("error");
        assert!(matches!(e, Output::Error { .. }));
    }

    #[tokio::test]
    async fn handle_chunked_response_flushes_remaining_binary_without_delimiter() {
        let mut raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\n".to_vec();
        raw.push(0xff);
        raw.extend_from_slice(b"\r\n0\r\n\r\n");
        let url = serve_once(raw).await;
        let response = get_response(&url).await;
        let (app, mut rx) = test_app().await;

        handle_chunked_response(
            &app,
            "id-rem",
            &None,
            response,
            &opts(Some("\n".to_string())),
            CancellationToken::new(),
            Instant::now(),
            "h1".to_string(),
            0,
        )
        .await;

        let _ = rx.recv().await.expect("chunk_start");
        let data = rx.recv().await.expect("chunk_data");
        match data {
            Output::ChunkData {
                data,
                data_base64: Some(b64),
                ..
            } => {
                assert!(data.is_none());
                assert_eq!(b64, "/w==");
            }
            _ => panic!("expected binary chunk_data"),
        }
        let end = rx.recv().await.expect("chunk_end");
        assert!(matches!(end, Output::ChunkEnd { .. }));
    }

    #[tokio::test]
    async fn handle_download_errors_when_resume_file_cannot_be_opened() {
        let raw = b"HTTP/1.1 206 Partial Content\r\nContent-Length: 1\r\n\r\na".to_vec();
        let url = serve_once(raw).await;
        let response = get_response(&url).await;
        let (app, mut rx) = test_app().await;

        let bad_path = std::env::temp_dir()
            .join(format!(
                "afhttp-no-parent-{}/resume.bin",
                std::process::id()
            ))
            .to_string_lossy()
            .into_owned();
        let mut o = opts(Some("\n".to_string()));
        o.response_save_file = Some(bad_path);
        o.response_save_resume = true;

        handle_download(
            &app,
            "id-open",
            &None,
            response,
            &o,
            CancellationToken::new(),
            Instant::now(),
            "h1".to_string(),
            0,
        )
        .await;

        let _ = rx.recv().await.expect("chunk_start");
        let err = rx.recv().await.expect("error");
        assert!(matches!(err, Output::Error { .. }));
    }

    #[tokio::test]
    async fn handle_download_errors_when_target_file_cannot_be_created() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\na".to_vec();
        let url = serve_once(raw).await;
        let response = get_response(&url).await;
        let (app, mut rx) = test_app().await;

        let bad_path = std::env::temp_dir()
            .join(format!(
                "afhttp-no-parent-{}/create.bin",
                std::process::id()
            ))
            .to_string_lossy()
            .into_owned();
        let mut o = opts(Some("\n".to_string()));
        o.response_save_file = Some(bad_path);

        handle_download(
            &app,
            "id-create",
            &None,
            response,
            &o,
            CancellationToken::new(),
            Instant::now(),
            "h1".to_string(),
            0,
        )
        .await;

        let _ = rx.recv().await.expect("chunk_start");
        let err = rx.recv().await.expect("error");
        assert!(matches!(err, Output::Error { .. }));
    }

    #[tokio::test]
    async fn handle_download_progress_and_sidecar() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc".to_vec();
        let url = serve_once(raw).await;
        let response = get_response(&url).await;
        let (app, mut rx) = test_app().await;
        {
            let mut cfg = app.config.write().await;
            cfg.log = vec!["progress".to_string()];
        }
        let mut o = opts(Some("\n".to_string()));
        o.chunked = false;
        o.progress_bytes = 1;
        o.progress_ms = 1;
        o.response_save_file = None;
        handle_download(
            &app,
            "id12",
            &None,
            response,
            &o,
            CancellationToken::new(),
            Instant::now(),
            "h1".to_string(),
            0,
        )
        .await;

        let mut saw_any = false;
        while let Some(out) = tokio::time::timeout(Duration::from_millis(20), rx.recv())
            .await
            .ok()
            .flatten()
        {
            saw_any = true;
            match out {
                Output::Log { event, .. } if event == "progress" => {
                    let _ = event;
                }
                Output::ChunkEnd { body_file, .. } => {
                    if let Some(path) = body_file {
                        let sidecar = sidecar_path_for(&path);
                        let _ = tokio::fs::remove_file(path).await;
                        let _ = tokio::fs::remove_file(sidecar).await;
                    }
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_any);
    }

    #[test]
    fn local_helpers_work() {
        assert_eq!(find_delimiter(b"abc--def", b"--"), Some(3));
        assert_eq!(find_delimiter(b"abc", b""), None);
        assert_eq!(find_delimiter(b"a", b"aa"), None);
        assert_eq!(sanitize_file_name("a/b:c"), "a_b_c");
        assert_eq!(sanitize_file_name(""), "request");
        let p = auto_download_path("/tmp/afhttpttp", "a/b");
        assert!(p.ends_with("/tmp/afhttpttp/a_b"));
        assert_eq!(sidecar_path_for("/tmp/x.bin"), "/tmp/x.bin.json");
    }
}
