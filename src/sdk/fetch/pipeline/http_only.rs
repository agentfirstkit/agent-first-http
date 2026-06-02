//! HTTP-only fast path and SPA-shell detector.

use std::time::Instant;

use url::Url;

use crate::sdk::fetch::artifacts::body as body_artifact;
use crate::sdk::fetch::result::{FetchResult, RenderDecision, Trace, Warning};
use crate::sdk::fetch::writer;
use crate::sdk::fetch::FetchBuilder;
use crate::shared::artifacts::{Artifact, ArtifactPaths};
use crate::shared::error::{Error, ErrorCode};
use crate::shared::ids::RequestId;
use crate::shared::time::duration_ms;

use super::cookie_jar_resolve::persist_set_cookies;
use super::request_opts::{prepare_cookie, BodyPayload, PreparedRequestOptions};

pub(super) struct HttpOnlyOutcome {
    pub(super) result: FetchResult,
    pub(super) body_bytes: Vec<u8>,
    pub(super) content_type: Option<String>,
}

pub(super) async fn http_only(
    builder: &FetchBuilder,
    request_options: &PreparedRequestOptions,
    request_id: RequestId,
    paths: &ArtifactPaths,
    start: Instant,
    escalation_from: Option<String>,
) -> Result<HttpOnlyOutcome, Error> {
    writer::ensure_dir(&paths.root).await?;

    let mut local_options;
    let opts_ref: &PreparedRequestOptions = if let Some(jar_path) = &builder.cookie_jar {
        let url = Url::parse(&builder.url).map_err(|e| {
            Error::new(
                ErrorCode::InvalidArgument,
                format!("cookie jar: invalid request url {:?}: {e}", builder.url),
            )
        })?;
        let jar = crate::sdk::profile::cookie_jar::CookieJar::load(jar_path)?;
        let applicable = jar.applicable_cookies(&url);
        let mut prepared = Vec::with_capacity(applicable.len());
        for c in applicable {
            if let Ok(p) = prepare_cookie(&c, "cookie_jar") {
                prepared.push(p);
            }
        }
        local_options = request_options.clone();
        local_options.merge_jar_cookies(prepared);
        &local_options
    } else {
        request_options
    };

    let http_owned;
    let http_client =
        if builder.proxy.is_some() || builder.ca_cert.is_some() || builder.tls_insecure {
            http_owned = build_per_fetch_http_client(builder).await?;
            &http_owned
        } else {
            builder.client.http()
        };

    let http_method = reqwest::Method::from_bytes(opts_ref.method.as_bytes()).map_err(|_| {
        Error::new(
            ErrorCode::InvalidArgument,
            format!("invalid HTTP method {:?}", opts_ref.method),
        )
    })?;
    let mut request = http_client.request(http_method, &builder.url);
    let headers = opts_ref.http_header_map(&builder.url)?;
    if !headers.is_empty() {
        request = request.headers(headers);
    }
    request = match &opts_ref.body_payload {
        BodyPayload::None => request,
        BodyPayload::Bytes(b) => request.body(b.clone()),
        BodyPayload::Form(fields) => request.form(fields),
    };

    let resp = tokio::time::timeout(builder.timeout, request.send())
        .await
        .map_err(|_| {
            Error::new(
                ErrorCode::NavigationTimeout,
                format!("HTTP fetch timed out after {:?}", builder.timeout),
            )
        })?
        .map_err(|e| {
            let code = classify_http_error(&e);
            let prefix = if e.is_timeout() {
                "HTTP timeout"
            } else if e.is_connect() {
                "HTTP connect"
            } else {
                "HTTP error"
            };
            Error::new(code, format!("{prefix}: {e}"))
        })?;

    let final_url = resp.url().to_string();
    let final_url_parsed = resp.url().clone();
    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let set_cookie_lines: Vec<String> = resp
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok().map(String::from))
        .collect();
    let (bytes, truncated_at) = read_body_with_cap(resp, builder.max_response_bytes).await?;

    if let Some(jar_path) = &builder.cookie_jar {
        persist_set_cookies(jar_path, &set_cookie_lines, &final_url_parsed)?;
    }

    let mut result = FetchResult {
        request_id,
        url: builder.url.clone(),
        final_url,
        status,
        tab_id: None,
        trace: Trace {
            render_decision: RenderDecision::HttpOnly,
            render_mode: builder.render.as_trace(),
            render_used: false,
            escalation_reason: escalation_from,
            main_request_observed: true,
            duration_ms: duration_ms(start.elapsed()),
            navigation_duration_ms: None,
            cookie_jar_file: builder.cookie_jar.clone(),
            cookie_jar_warning: builder.cookie_jar_warning.clone(),
            sensitive_capture: super::sensitive_capture(builder),
        },
        warnings: Vec::new(),
        body_file: None,
        rendered_html_file: None,
        text_file: None,
        screenshot_file: None,
        network_file: None,
        console_file: None,
        observation_file: None,
        storage_file: None,
        download_file: None,
        download_bytes: None,
        download_filename: None,
        download_url: None,
        download_state: None,
    };

    if builder.want.contains(&Artifact::Body) {
        let path = body_artifact::write(paths, content_type.as_deref(), &bytes).await?;
        result.set_artifact_file(Artifact::Body, path);
    }

    if let Some(cap) = truncated_at {
        result.warnings.push(Warning {
            artifact: Artifact::Body,
            code: ErrorCode::NetworkBodyTruncated,
            detail: format!(
                "HTTP response body exceeded --max-response-bytes={cap}; \
                 stored prefix only ({} bytes)",
                bytes.len()
            ),
        });
    }

    result.trace.duration_ms = duration_ms(start.elapsed());
    Ok(HttpOnlyOutcome {
        result,
        body_bytes: bytes.clone(),
        content_type,
    })
}

async fn build_per_fetch_http_client(builder: &FetchBuilder) -> Result<reqwest::Client, Error> {
    let mut b =
        reqwest::Client::builder().user_agent(concat!("afhttp/", env!("CARGO_PKG_VERSION")));
    match &builder.proxy {
        Some(url) => {
            let proxy = reqwest::Proxy::all(url).map_err(|e| {
                Error::new(
                    ErrorCode::InvalidArgument,
                    format!("--proxy-url {url:?}: {e}"),
                )
            })?;
            b = b.proxy(proxy);
        }
        None => {
            b = b.no_proxy();
        }
    }
    if let Some(path) = &builder.ca_cert {
        let pem = tokio::fs::read(path).await.map_err(|e| {
            Error::new(
                ErrorCode::IoError,
                format!("--ca-cert read {}: {e}", path.display()),
            )
        })?;
        let cert = reqwest::Certificate::from_pem(&pem).map_err(|e| {
            Error::new(
                ErrorCode::InvalidArgument,
                format!("--ca-cert parse {}: {e}", path.display()),
            )
        })?;
        b = b.add_root_certificate(cert);
    }
    if builder.tls_insecure {
        b = b.danger_accept_invalid_certs(true);
    }
    b.build().map_err(|e| {
        Error::new(
            ErrorCode::InternalError,
            format!("per-fetch reqwest build: {e}"),
        )
    })
}

async fn read_body_with_cap(
    mut resp: reqwest::Response,
    cap: u64,
) -> Result<(Vec<u8>, Option<u64>), Error> {
    if cap == 0 {
        let b = resp
            .bytes()
            .await
            .map_err(|e| Error::new(ErrorCode::IoError, format!("HTTP body: {e}")))?;
        return Ok((b.to_vec(), None));
    }
    let cap_usize: usize = usize::try_from(cap).unwrap_or(usize::MAX);
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| Error::new(ErrorCode::IoError, format!("HTTP body: {e}")))?
    {
        if buf.len() >= cap_usize {
            continue;
        }
        let remaining = cap_usize - buf.len();
        if chunk.len() <= remaining {
            buf.extend_from_slice(&chunk);
        } else {
            buf.extend_from_slice(&chunk[..remaining]);
        }
    }
    let truncated = if buf.len() == cap_usize {
        Some(cap)
    } else {
        None
    };
    Ok((buf, truncated))
}

fn classify_http_error(err: &reqwest::Error) -> ErrorCode {
    classify_http_error_parts(err.is_timeout(), err.is_connect(), &format!("{err:#}"))
}

pub(crate) fn classify_http_error_parts(
    is_timeout: bool,
    is_connect: bool,
    detail: &str,
) -> ErrorCode {
    let lower = detail.to_ascii_lowercase();
    if is_timeout {
        ErrorCode::NavigationTimeout
    } else if lower.contains("dns")
        || lower.contains("failed to lookup address information")
        || lower.contains("name or service not known")
        || lower.contains("nodename nor servname")
        || lower.contains("name does not resolve")
        || lower.contains("temporary failure in name resolution")
    {
        ErrorCode::DnsResolutionFailed
    } else if lower.contains("tls")
        || lower.contains("ssl")
        || lower.contains("certificate")
        || lower.contains("cert")
    {
        ErrorCode::TlsError
    } else if is_connect {
        ErrorCode::TargetUnreachable
    } else {
        ErrorCode::IoError
    }
}

/// Conservative SPA-shell detector.
pub(super) fn looks_like_empty_html_shell(bytes: &[u8], content_type: Option<&str>) -> bool {
    let ct = content_type.unwrap_or("").to_ascii_lowercase();
    if !ct.starts_with("text/html") && !ct.contains("application/xhtml") {
        return false;
    }
    if bytes.len() > 32 * 1024 {
        return false;
    }
    let lower = std::str::from_utf8(bytes)
        .unwrap_or("")
        .to_ascii_lowercase();
    if !lower.contains("<script") {
        return false;
    }
    let body_open = lower.find("<body");
    let visible_chars = match body_open {
        Some(idx) => count_visible_chars_after(&lower, idx),
        None => 0,
    };
    visible_chars < 200
}

fn count_visible_chars_after(html: &str, body_start: usize) -> usize {
    let after_body = &html[body_start..];
    let mut text = String::with_capacity(after_body.len());
    let mut in_tag = false;
    let mut skip_until: Option<&str> = None;
    let bytes = after_body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(needle) = skip_until {
            if after_body[i..].starts_with(needle) {
                i += needle.len();
                skip_until = None;
            } else {
                i += 1;
            }
            continue;
        }
        let b = bytes[i];
        if !in_tag && b == b'<' {
            if after_body[i..].starts_with("<script") {
                skip_until = Some("</script>");
                i += "<script".len();
                continue;
            }
            if after_body[i..].starts_with("<style") {
                skip_until = Some("</style>");
                i += "<style".len();
                continue;
            }
            in_tag = true;
            i += 1;
            continue;
        }
        if in_tag {
            if b == b'>' {
                in_tag = false;
            }
            i += 1;
            continue;
        }
        if !b.is_ascii_whitespace() {
            text.push(b as char);
        }
        i += 1;
    }
    text.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_html_shell_detector_classifies_common_cases() {
        let shell = b"<!doctype html><html><head><title>App</title></head><body><div id=\"root\"></div><script src=\"app.js\"></script></body></html>";
        assert!(looks_like_empty_html_shell(shell, Some("text/html")));

        let no_body = b"<!doctype html><html><head><script src=\"x.js\"></script></head></html>";
        assert!(looks_like_empty_html_shell(
            no_body,
            Some("text/html; charset=utf-8")
        ));

        let article = b"<!doctype html><html><body><h1>Title</h1><p>This is a long article with lots of visible text content, more than two hundred characters total, which means an agent reading the HTTP body would already have something useful to extract and shouldn't be redirected through a slow browser path.</p><script src=\"ads.js\"></script></body></html>";
        assert!(!looks_like_empty_html_shell(article, Some("text/html")));

        assert!(!looks_like_empty_html_shell(
            shell,
            Some("application/json")
        ));
        assert!(!looks_like_empty_html_shell(shell, None));

        let static_page = b"<!doctype html><html><body><div id=\"root\"></div></body></html>";
        assert!(!looks_like_empty_html_shell(static_page, Some("text/html")));

        let mut big = Vec::with_capacity(64 * 1024);
        big.extend_from_slice(b"<!doctype html><html><body><script>");
        big.resize(64 * 1024, b'x');
        big.extend_from_slice(b"</script></body></html>");
        assert!(!looks_like_empty_html_shell(&big, Some("text/html")));
    }

    #[test]
    fn classify_http_error_parts_matches_browser_side_buckets() {
        assert_eq!(
            classify_http_error_parts(
                false,
                true,
                "dns error: failed to lookup address information"
            ),
            ErrorCode::DnsResolutionFailed
        );
        assert_eq!(
            classify_http_error_parts(false, true, "invalid peer certificate: UnknownIssuer"),
            ErrorCode::TlsError
        );
        assert_eq!(
            classify_http_error_parts(false, true, "tcp connect error: Connection refused"),
            ErrorCode::TargetUnreachable
        );
        assert_eq!(
            classify_http_error_parts(true, true, "operation timed out"),
            ErrorCode::NavigationTimeout
        );
    }
}
