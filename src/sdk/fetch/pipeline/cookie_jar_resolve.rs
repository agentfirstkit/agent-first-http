//! Cookie-jar path resolution (called by `execute` before the fetch) and
//! browser-session cookie sync (called by `browser_path` after navigation).

use url::Url;

use crate::sdk::fetch::FetchBuilder;
use crate::shared::error::{Error, ErrorCode};
use crate::shared::path::absolute_lexical;

/// Decide the effective cookie-jar path for this fetch and enforce the
/// isolation invariant on any user-provided override.
pub(super) async fn resolve_cookie_jar_path(builder: &mut FetchBuilder) -> Result<(), Error> {
    if builder.cookie_jar_disabled {
        builder.cookie_jar = None;
        return Ok(());
    }
    if builder.client.is_hostless() {
        builder.cookie_jar = builder.cookie_jar.take().map(absolute_lexical);
        return Ok(());
    }
    if builder.client.has_inline_host() && !builder.client.inline_host_started().await {
        builder.cookie_jar = builder.cookie_jar.take().map(absolute_lexical);
        return Ok(());
    }
    // A `/profile` failure (inline or external afhttp host) is a trace
    // warning, not a hard error: the fetch still runs, just without the
    // implicit profile cookie jar. An explicit `--cookie-jar` is honored
    // as-is below.
    let info = match builder.client.profile_info().await {
        Ok(info) => Some(info),
        Err(e) => {
            if builder.cookie_jar.is_none() {
                builder.cookie_jar_warning = Some(format!(
                    "GET /profile unavailable; implicit profile cookie jar disabled: {}",
                    e.detail
                ));
            }
            None
        }
    };
    match (&builder.cookie_jar, info.as_ref()) {
        (Some(explicit), Some(info)) => {
            if let Some(expected) = info.canonical_cookie_jar() {
                let explicit_abs = absolute_lexical(explicit.clone());
                let expected_abs = absolute_lexical(expected);
                if !jar_paths_match(&explicit_abs, &expected_abs) {
                    return Err(Error::new(
                        ErrorCode::InvalidArgument,
                        format!(
                            "--cookie-jar {} does not match the host's profile jar {}; \
                             the isolation invariant requires the jar to live inside the \
                             active profile dir",
                            explicit.display(),
                            expected_abs.display()
                        ),
                    ));
                }
                builder.cookie_jar = Some(expected_abs);
            }
        }
        (Some(explicit), None) => {
            builder.cookie_jar = Some(absolute_lexical(explicit.clone()));
        }
        (None, Some(info)) => {
            if let Some(jar) = info.canonical_cookie_jar() {
                builder.cookie_jar = Some(absolute_lexical(jar));
            }
        }
        (None, None) => {}
    }
    Ok(())
}

fn jar_paths_match(a: &std::path::Path, b: &std::path::Path) -> bool {
    fn normalize(p: &std::path::Path) -> std::path::PathBuf {
        let mut out = std::path::PathBuf::new();
        for component in p.components() {
            match component {
                std::path::Component::CurDir => {}
                _ => out.push(component.as_os_str()),
            }
        }
        out
    }
    normalize(a) == normalize(b)
}

/// Write `Set-Cookie` lines from an HTTP response back into the jar.
pub(crate) fn persist_set_cookies(
    jar_path: &std::path::Path,
    set_cookie_lines: &[String],
    request_url: &Url,
) -> Result<(), Error> {
    if set_cookie_lines.is_empty() {
        return Ok(());
    }
    let mut jar = crate::sdk::profile::cookie_jar::CookieJar::load(jar_path)?;
    for line in set_cookie_lines {
        match cookie::Cookie::parse(line.to_string()) {
            Ok(c) => jar.merge(c.into_owned(), request_url),
            Err(_) => continue,
        }
    }
    jar.persist()
}

/// Pull cookies the browser holds for `final_url` and merge into the jar.
pub(crate) async fn sync_browser_cookies_to_jar(
    conn: &crate::sdk::cdp::ws_client::Connection,
    session_id: &str,
    final_url: &str,
    jar_path: &std::path::Path,
) -> Result<(), Error> {
    let final_url_parsed = Url::parse(final_url).map_err(|e| {
        Error::new(
            ErrorCode::InternalError,
            format!("cookie jar sync: parse final_url {final_url:?}: {e}"),
        )
    })?;
    let urls = [final_url_parsed.as_str().to_string()];
    let r = conn
        .send(
            "Network.getCookies",
            &serde_json::json!({ "urls": urls }),
            Some(session_id),
        )
        .await?;
    let cookies = match r.get("cookies").and_then(|v| v.as_array()) {
        Some(arr) => arr.clone(),
        None => return Ok(()),
    };
    if cookies.is_empty() {
        return Ok(());
    }
    let mut jar = crate::sdk::profile::cookie_jar::CookieJar::load(jar_path)?;
    for entry in cookies {
        if let Some(c) = cdp_cookie_to_set_cookie(&entry) {
            jar.merge(c, &final_url_parsed);
        }
    }
    jar.persist()
}

fn cdp_cookie_to_set_cookie(entry: &serde_json::Value) -> Option<cookie::Cookie<'static>> {
    let name = entry.get("name")?.as_str()?.to_string();
    let value = entry.get("value")?.as_str()?.to_string();
    let mut b = cookie::Cookie::build((name, value));
    if let Some(d) = entry.get("domain").and_then(|v| v.as_str()) {
        if !d.is_empty() {
            b = b.domain(d.to_string());
        }
    }
    if let Some(p) = entry.get("path").and_then(|v| v.as_str()) {
        if !p.is_empty() {
            b = b.path(p.to_string());
        }
    }
    if entry
        .get("secure")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        b = b.secure(true);
    }
    if entry
        .get("httpOnly")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        b = b.http_only(true);
    }
    if let Some(expires_sec) = entry.get("expires").and_then(|v| v.as_f64()) {
        if expires_sec > 0.0 {
            if let Ok(dt) = cookie::time::OffsetDateTime::from_unix_timestamp(expires_sec as i64) {
                b = b.expires(cookie::Expiration::DateTime(dt));
            }
        }
    }
    Some(b.build())
}
