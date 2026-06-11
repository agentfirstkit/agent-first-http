//! HTTP-only fast path and SPA-shell detector.

use std::borrow::Cow;
use std::time::Instant;

use url::Url;

use crate::sdk::fetch::artifacts::body as body_artifact;
use crate::sdk::fetch::deadline::FetchDeadline;
use crate::sdk::fetch::result::{FetchResult, RenderDecision, Warning};
use crate::sdk::fetch::writer;
use crate::sdk::fetch::FetchBuilder;
use crate::shared::artifacts::{Artifact, ArtifactPaths};
use crate::shared::error::{Error, ErrorCode};
use crate::shared::ids::RequestId;
use crate::shared::time::duration_ms;

use super::super::page_classification;
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
    deadline: &FetchDeadline,
) -> Result<HttpOnlyOutcome, Error> {
    deadline.set_stage("prepare_http_fetch");
    writer::ensure_dir(&paths.root).await?;

    let opts = resolve_jar_cookies(builder, request_options)?;
    let opts_ref: &PreparedRequestOptions = &opts;

    let http_owned;
    let http_client = if builder.http.proxy.is_some()
        || builder.http.ca_cert.is_some()
        || builder.http.tls_insecure
    {
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

    deadline.update_trace(|trace| {
        trace.render_decision = RenderDecision::HttpOnly;
        trace.render_used = false;
        trace.escalation_reason = escalation_from.clone();
        trace.main_request_observed = true;
        trace.cookie_jar_file = builder.cookie_jar.path.clone();
        trace.cookie_jar_warning = builder.cookie_jar.warning.clone();
        trace.sensitive_capture = super::sensitive_capture(builder);
    });

    let resp = deadline
        .run_result("navigate", ErrorCode::NavigationTimeout, async {
            request.send().await.map_err(|e| {
                let code = classify_http_error(&e);
                let prefix = if e.is_timeout() {
                    "HTTP timeout"
                } else if e.is_connect() {
                    "HTTP connect"
                } else {
                    "HTTP error"
                };
                Error::new(code, format!("{prefix}: {e}"))
            })
        })
        .await?;

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
    let (bytes, truncated_at) = deadline
        .run_result(
            "capture_body",
            ErrorCode::NavigationTimeout,
            read_body_with_cap(resp, builder.http.max_response_bytes),
        )
        .await?;

    if let Some(jar_path) = &builder.cookie_jar.path {
        deadline
            .run_result(
                "sync_cookie_jar",
                ErrorCode::NavigationTimeout,
                std::future::ready(persist_set_cookies(
                    jar_path,
                    &set_cookie_lines,
                    &final_url_parsed,
                )),
            )
            .await?;
    }

    let mut result = FetchResult::new(request_id, builder.url.clone(), deadline.snapshot());
    result.final_url = final_url;
    result.status = status;

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
    if let Some(classification) = classify_http_body(&bytes, content_type.as_deref()) {
        result.set_page_kind(classification.kind);
        result.warnings.push(Warning {
            artifact: Artifact::Body,
            code: classification.code,
            detail: classification.detail,
        });
    }

    deadline.update_trace(|trace| {
        trace.duration_ms = duration_ms(start.elapsed());
    });
    result.trace = deadline.complete_trace();
    Ok(HttpOnlyOutcome {
        result,
        body_bytes: bytes.clone(),
        content_type,
    })
}

pub(super) fn classify_http_body(
    bytes: &[u8],
    content_type: Option<&str>,
) -> Option<page_classification::PageClassification> {
    let ct = content_type.unwrap_or("").to_ascii_lowercase();
    if !ct.starts_with("text/html") && !ct.contains("application/xhtml") {
        return None;
    }
    let html = std::str::from_utf8(bytes).ok()?;
    page_classification::classify(Some(html), None, None)
}

/// Layer cookie-jar cookies on top of `request_options` when a jar is
/// configured. Borrows `request_options` unchanged when there is no jar,
/// otherwise returns an owned clone with the jar's applicable cookies merged.
fn resolve_jar_cookies<'a>(
    builder: &FetchBuilder,
    request_options: &'a PreparedRequestOptions,
) -> Result<Cow<'a, PreparedRequestOptions>, Error> {
    let Some(jar_path) = &builder.cookie_jar.path else {
        return Ok(Cow::Borrowed(request_options));
    };
    let url = Url::parse(&builder.url).map_err(|e| {
        Error::new(
            ErrorCode::InvalidArgument,
            format!("cookie jar: invalid request url {:?}: {e}", builder.url),
        )
    })?;
    let jar = crate::sdk::profile::cookie_jar::CookieJar::load(jar_path)?;
    let mut prepared = Vec::new();
    for c in jar.applicable_cookies(&url) {
        if let Ok(p) = prepare_cookie(&c, "cookie_jar") {
            prepared.push(p);
        }
    }
    let mut owned = request_options.clone();
    owned.merge_jar_cookies(prepared);
    Ok(Cow::Owned(owned))
}

async fn build_per_fetch_http_client(builder: &FetchBuilder) -> Result<reqwest::Client, Error> {
    let mut b =
        reqwest::Client::builder().user_agent(concat!("afhttp/", env!("CARGO_PKG_VERSION")));
    match &builder.http.proxy {
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
    if let Some(path) = &builder.http.ca_cert {
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
    if builder.http.tls_insecure {
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
    let mut visible = 0;
    let mut in_tag = false;
    let mut skip_until: Option<&str> = None;
    let mut i = 0;
    while i < after_body.len() {
        if let Some(needle) = skip_until {
            if after_body[i..].starts_with(needle) {
                i += needle.len();
                skip_until = None;
            } else {
                i += after_body[i..]
                    .chars()
                    .next()
                    .map(char::len_utf8)
                    .unwrap_or(1);
            }
            continue;
        }
        let Some(ch) = after_body[i..].chars().next() else {
            break;
        };
        if !in_tag && ch == '<' {
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
            if ch == '>' {
                in_tag = false;
            }
            i += ch.len_utf8();
            continue;
        }
        if !ch.is_whitespace() {
            visible += 1;
        }
        i += ch.len_utf8();
    }
    visible
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
