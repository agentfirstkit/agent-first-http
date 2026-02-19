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
