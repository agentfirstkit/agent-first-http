//! HTTP-only fast path and SPA-shell detector.

use std::borrow::Cow;
use std::time::Instant;

use url::Url;

use crate::sdk::fetch::FetchBuilder;
use crate::sdk::fetch::artifacts::body as body_artifact;
use crate::sdk::fetch::deadline::FetchDeadline;
use crate::sdk::fetch::result::{FetchResult, RenderDecision, Warning};
use crate::sdk::fetch::writer;
use crate::shared::artifacts::{Artifact, ArtifactPaths};
use crate::shared::error::{Error, ErrorCode};
use crate::shared::ids::RequestId;
use crate::shared::time::duration_ms;

use super::super::page_classification;
use super::cookie_jar_resolve::persist_set_cookies;
use super::request_opts::{
    BodyPayload, PreparedRequestOptions, prepare_cookie, validate_cookie_scope,
};

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

    let http_owned;
    let http_client = if builder.http.proxy.is_some()
        || builder.http.ca_cert.is_some()
        || builder.http.tls_insecure
        || builder.cookie_jar.path.is_some()
    {
        http_owned = build_per_fetch_http_client(builder).await?;
        &http_owned
    } else {
        builder.client.http()
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
            send_http_response(builder, request_options, http_client).await
        })
        .await?;

    let final_url = resp.url().to_string();
    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let (bytes, truncated_at) = deadline
        .run_result(
            "capture_body",
            ErrorCode::NavigationTimeout,
            read_body_with_cap(resp, builder.http.max_response_bytes),
        )
        .await?;

    let mut result = FetchResult::new(request_id, builder.url.clone(), deadline.snapshot());
    result.final_url = final_url;
    result.status = status;
    result.content_type = content_type.clone();

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

async fn send_http_response(
    builder: &FetchBuilder,
    request_options: &PreparedRequestOptions,
    client: &reqwest::Client,
) -> Result<reqwest::Response, Error> {
    let mut url = Url::parse(&builder.url).map_err(|_| {
        let code = if builder.cookie_jar.path.is_some() {
            ErrorCode::InvalidArgument
        } else {
            ErrorCode::IoError
        };
        Error::new(code, "invalid HTTP request URL")
    })?;
    let mut options = request_options.clone();
    for redirects in 0..=10 {
        let mut opts = resolve_jar_cookies(builder, &options, &url)?;
        if redirects > 0 {
            // The caller scoped its cookies against the URL it asked for, and
            // the first hop still rejects a mismatch. A redirect target is the
            // server's choice: a cookie that does not apply there is simply
            // not sent, the way a browser would treat it.
            opts.to_mut()
                .cookies
                .retain(|cookie| validate_cookie_scope(cookie, &url, ".cookie(...)").is_ok());
        }
        let method = reqwest::Method::from_bytes(opts.method.as_bytes()).map_err(|_| {
            Error::new(
                ErrorCode::InvalidArgument,
                format!("invalid HTTP method {:?}", opts.method),
            )
        })?;
        let mut request = client
            .request(method, url.clone())
            .headers(opts.http_header_map(url.as_str())?);
        request = match &opts.body_payload {
            BodyPayload::None => request,
            BodyPayload::Bytes(bytes) => request.body(bytes.clone()),
            BodyPayload::Form(fields) => request.form(fields),
        };
        let response = request.send().await.map_err(|error| {
            Error::new(
                classify_http_error(&error),
                format!("HTTP request: {error}"),
            )
        })?;
        let Some(jar_path) = &builder.cookie_jar.path else {
            return Ok(response);
        };
        let cookies = response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok().map(String::from))
            .collect::<Vec<_>>();
        persist_set_cookies(jar_path, &cookies, response.url())?;
        let status = response.status().as_u16();
        if !matches!(status, 301 | 302 | 303 | 307 | 308) {
            return Ok(response);
        }
        let Some(location) = response.headers().get(reqwest::header::LOCATION) else {
            return Ok(response);
        };
        if redirects == 10 {
            return Err(Error::new(
                ErrorCode::IoError,
                "HTTP redirect limit exceeded (10 hops)",
            ));
        }
        let next = location
            .to_str()
            .ok()
            .and_then(|location| url.join(location).ok())
            .filter(|next| matches!(next.scheme(), "http" | "https"))
            // The server chose this Location; the caller's arguments were fine.
            .ok_or_else(|| Error::new(ErrorCode::IoError, "invalid HTTP redirect location"))?;
        if url.origin() != next.origin() {
            options.headers.retain(|header| {
                !matches!(
                    header.name.as_str(),
                    "authorization"
                        | "proxy-authorization"
                        | "cookie"
                        | "cookie2"
                        | "host"
                        | "www-authenticate"
                )
            });
            options.raw_cookie_header = None;
            options.raw_cookie_pairs.clear();
            options.cookies.clear();
        }
        if ((status == 301 || status == 302) && options.method == "POST")
            || (status == 303 && options.method != "HEAD")
        {
            options.method = "GET".into();
            options.body_payload = BodyPayload::None;
            options.headers.retain(|header| {
                !matches!(
                    header.name.as_str(),
                    "content-type" | "content-length" | "transfer-encoding"
                )
            });
        }
        url = next;
    }
    Err(Error::new(
        ErrorCode::InternalError,
        "HTTP redirect loop ended unexpectedly",
    ))
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
    url: &Url,
) -> Result<Cow<'a, PreparedRequestOptions>, Error> {
    let Some(jar_path) = &builder.cookie_jar.path else {
        return Ok(Cow::Borrowed(request_options));
    };
    let jar = crate::sdk::profile::cookie_jar::CookieJar::load(jar_path)?;
    let mut prepared = Vec::new();
    for c in jar.applicable_cookies(url) {
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
    if builder.cookie_jar.path.is_some() {
        b = b.redirect(reqwest::redirect::Policy::none());
    }
    match &builder.http.proxy {
        Some(url) => {
            // A proxy URL routinely carries `user:password@`, so the value
            // never appears in the message — and neither does the underlying
            // error, which quotes the URL it was handed.
            let proxy = reqwest::Proxy::all(url).map_err(|_| {
                Error::new(
                    ErrorCode::InvalidArgument,
                    "--proxy-url: not a usable proxy URL".to_string(),
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

    async fn read_fixture_request(stream: &mut tokio::net::TcpStream) -> String {
        use tokio::io::AsyncReadExt;
        let mut request = Vec::new();
        loop {
            let mut chunk = [0; 2048];
            let count = stream.read(&mut chunk).await.unwrap();
            assert!(count > 0);
            request.extend_from_slice(&chunk[..count]);
            if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length:")?
                            .trim()
                            .parse::<usize>()
                            .ok()
                    })
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        String::from_utf8(request).unwrap()
    }

    #[tokio::test]
    async fn cookie_jar_redirect_loop_stops_after_ten_hops() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/loop", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for _ in 0..=10 {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_fixture_request(&mut stream).await;
                stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: /loop\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let client = crate::sdk::Client::http_only().unwrap();
        let error = client
            .fetch(url)
            .render(crate::sdk::fetch::RenderMode::None)
            .cookie_jar(dir.path().join("cookies.json"))
            .out_dir(dir.path().join("out"))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .unwrap_err();
        assert_eq!(error.error_code, ErrorCode::IoError);
        assert_eq!(error.detail, "HTTP redirect limit exceeded (10 hops)");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn redirect_hops_persist_cookies_and_preserve_method_and_credential_rules() {
        use tokio::io::AsyncWriteExt;
        for status in [301, 302, 303, 307, 308] {
            let login = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let login_url = format!("http://{}/login", login.local_addr().unwrap());
            let echo_url = format!("http://{}/echo-cookie", echo.local_addr().unwrap());
            let destination = echo_url.clone();
            let server = tokio::spawn(async move {
                let (mut stream, _) = login.accept().await.unwrap();
                let initial = read_fixture_request(&mut stream).await;
                assert!(initial.starts_with("POST /login "));
                stream.write_all(format!("HTTP/1.1 {status} Redirect\r\nLocation: {destination}\r\nSet-Cookie: sid=redirect-session; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
                drop(stream);
                let (mut stream, _) = echo.accept().await.unwrap();
                let request = read_fixture_request(&mut stream).await;
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await
                    .unwrap();
                request
            });
            let dir = tempfile::tempdir().unwrap();
            let jar_path = dir.path().join("cookies.json");
            let client = crate::sdk::Client::http_only().unwrap();
            let result = client
                .fetch(login_url)
                .render(crate::sdk::fetch::RenderMode::None)
                .method("POST")
                .body(b"login=data".to_vec())
                .header("Authorization", "Bearer caller-secret")
                .header("Cookie", "caller=secret")
                .cookie_jar(&jar_path)
                .out_dir(dir.path().join("out"))
                .want([Artifact::Body])
                .timeout(std::time::Duration::from_secs(2))
                .send()
                .await
                .unwrap();
            assert_eq!(result.final_url, echo_url);
            assert_eq!(result.status, 200);
            let request = server.await.unwrap();
            let headers = request.to_ascii_lowercase();
            assert!(
                headers.contains("cookie: sid=redirect-session\r\n"),
                "{request}"
            );
            assert!(!headers.contains("caller-secret") && !headers.contains("caller=secret"));
            if matches!(status, 307 | 308) {
                assert!(
                    request.starts_with("POST /echo-cookie ") && request.ends_with("login=data")
                );
            } else {
                assert!(request.starts_with("GET /echo-cookie ") && request.ends_with("\r\n\r\n"));
            }
            let jar = crate::sdk::profile::cookie_jar::CookieJar::load(&jar_path).unwrap();
            assert!(
                jar.applicable_cookies(&Url::parse(&echo_url).unwrap())
                    .iter()
                    .any(|cookie| cookie.name() == "sid" && cookie.value() == "redirect-session")
            );
        }
    }

    #[tokio::test]
    async fn same_origin_redirect_keeps_credentials_and_skips_out_of_scope_caller_cookies() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let first = read_fixture_request(&mut stream).await;
            // A relative Location: resolved against /login/start it is /login/../home.
            stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: ../home?step=2\r\nSet-Cookie: sid=redirect-session; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
            drop(stream);
            let (mut stream, _) = listener.accept().await.unwrap();
            let second = read_fixture_request(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .unwrap();
            (first, second)
        });
        let dir = tempfile::tempdir().unwrap();
        let client = crate::sdk::Client::http_only().unwrap();
        let result = client
            .fetch(format!("{origin}/login/start"))
            .render(crate::sdk::fetch::RenderMode::None)
            .header("Authorization", "Bearer caller-secret")
            .cookie_full(
                cookie::Cookie::build(("scoped", "login-only"))
                    .path("/login")
                    .build(),
            )
            .cookie("everywhere", "yes")
            .cookie_jar(dir.path().join("cookies.json"))
            .out_dir(dir.path().join("out"))
            .want([Artifact::Body])
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .unwrap();
        assert_eq!(result.status, 200);
        assert_eq!(result.final_url, format!("{origin}/home?step=2"));
        let (first, second) = server.await.unwrap();
        let (first_lower, second_lower) = (first.to_ascii_lowercase(), second.to_ascii_lowercase());
        assert!(first.starts_with("GET /login/start "), "{first}");
        assert!(
            first_lower.contains("cookie: scoped=login-only; everywhere=yes\r\n"),
            "{first}"
        );
        assert!(second.starts_with("GET /home?step=2 "), "{second}");
        assert!(
            second_lower.contains("authorization: bearer caller-secret\r\n"),
            "{second}"
        );
        assert!(
            second_lower.contains("cookie: everywhere=yes; sid=redirect-session\r\n"),
            "{second}"
        );
    }

    #[tokio::test]
    async fn out_of_scope_caller_cookie_on_the_requested_url_is_still_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let client = crate::sdk::Client::http_only().unwrap();
        let error = client
            .fetch("http://127.0.0.1:1/home")
            .render(crate::sdk::fetch::RenderMode::None)
            .cookie_full(
                cookie::Cookie::build(("scoped", "login-only"))
                    .path("/login")
                    .build(),
            )
            .cookie_jar(dir.path().join("cookies.json"))
            .out_dir(dir.path().join("out"))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .unwrap_err();
        assert_eq!(error.error_code, ErrorCode::InvalidArgument);
        assert_eq!(
            error.detail,
            ".cookie(...): cookie \"scoped\" Path=\"/login\" does not match URL path \"/home\""
        );
    }

    #[tokio::test]
    async fn unusable_redirect_location_is_a_server_failure_not_a_caller_error() {
        use tokio::io::AsyncWriteExt;
        for location in ["ftp://example.invalid/file", "http://[broken"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/start", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_fixture_request(&mut stream).await;
                stream.write_all(format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            });
            let dir = tempfile::tempdir().unwrap();
            let client = crate::sdk::Client::http_only().unwrap();
            let error = client
                .fetch(url)
                .render(crate::sdk::fetch::RenderMode::None)
                .cookie_jar(dir.path().join("cookies.json"))
                .out_dir(dir.path().join("out"))
                .timeout(std::time::Duration::from_secs(2))
                .send()
                .await
                .unwrap_err();
            assert_eq!(error.error_code, ErrorCode::IoError, "{location}");
            assert_eq!(error.detail, "invalid HTTP redirect location");
            server.await.unwrap();
        }
    }

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

    #[tokio::test]
    async fn http_only_request_failure_redacts_sdk_error_urls() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local address").port();
        drop(listener);
        let out = tempfile::tempdir().expect("output directory");
        let client = crate::sdk::Client::http_only().expect("HTTP-only client");
        let err = client
            .fetch(format!(
                "http://reader:password-canary@127.0.0.1:{port}/path?token_secret=query-canary&api_key=legacy-canary&safe=ok"
            ))
            .render(crate::sdk::fetch::RenderMode::None)
            .want([Artifact::Body])
            .out_dir(out.path().to_path_buf())
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .expect_err("closed endpoint must fail");
        assert_eq!(err.error_code, ErrorCode::TargetUnreachable);
        assert!(err.detail.contains("token_secret=***"));
        assert!(err.detail.contains("api_key=***"));
        let display = err.to_string();
        let json = serde_json::to_string(&err).expect("serialize SDK error");
        for text in [&err.detail, &display, &json] {
            assert!(!text.contains("password-canary"), "password leaked: {text}");
            assert!(!text.contains("query-canary"), "query leaked: {text}");
            assert!(
                !text.contains("legacy-canary"),
                "legacy query leaked: {text}"
            );
        }
    }
}
