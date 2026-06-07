//! Cookie-jar path resolution (called by `execute` before the fetch) and
//! browser-session cookie sync (called by `browser_path` after navigation).

use url::Url;

use crate::sdk::fetch::FetchBuilder;
use crate::shared::error::{Error, ErrorCode};
use crate::shared::path::absolute_lexical;

/// Decide the effective cookie-jar path for this fetch and enforce the
/// isolation invariant on any user-provided override.
pub(super) async fn resolve_cookie_jar_path(builder: &mut FetchBuilder) -> Result<(), Error> {
    if builder.cookie_jar.disabled {
        builder.cookie_jar.path = None;
        return Ok(());
    }
    if builder.client.is_hostless() {
        builder.cookie_jar.path = builder.cookie_jar.path.take().map(absolute_lexical);
        return Ok(());
    }
    if builder.client.has_inline_host() && !builder.client.inline_host_started().await {
        builder.cookie_jar.path = builder.cookie_jar.path.take().map(absolute_lexical);
        return Ok(());
    }
    // A `/profile` failure (inline or external afhttp host) is a trace
    // warning, not a hard error: the fetch still runs, just without the
    // implicit profile cookie jar. An explicit `--cookie-jar` is honored
    // as-is below.
    let info = match builder.client.profile_info().await {
        Ok(info) => Some(info),
        Err(e) => {
            if builder.cookie_jar.path.is_none() {
                builder.cookie_jar.warning = Some(format!(
                    "GET /profile unavailable; implicit profile cookie jar disabled: {}",
                    e.detail
                ));
            }
            None
        }
    };
    match (&builder.cookie_jar.path, info.as_ref()) {
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
                builder.cookie_jar.path = Some(expected_abs);
            }
        }
        (Some(explicit), None) => {
            builder.cookie_jar.path = Some(absolute_lexical(explicit.clone()));
        }
        (None, Some(info)) => {
            if let Some(jar) = info.canonical_cookie_jar() {
                if !builder.client.has_inline_host() {
                    builder.cookie_jar.path = None;
                    builder.cookie_jar.warning = Some(format!(
                        "implicit profile cookie jar disabled for external host: host profile path {} is not a client-owned local profile; browser profile cookies remain host-side",
                        info.path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "<absent>".into())
                    ));
                } else if implicit_profile_path_is_local(info.path.as_deref()) {
                    builder.cookie_jar.path = Some(absolute_lexical(jar));
                } else {
                    builder.cookie_jar.path = None;
                    builder.cookie_jar.warning = Some(format!(
                        "implicit profile cookie jar disabled: host profile path {} is not visible as a writable local directory; browser profile cookies remain host-side",
                        info.path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "<absent>".into())
                    ));
                }
            }
        }
        (None, None) => {}
    }
    Ok(())
}

fn implicit_profile_path_is_local(path: Option<&std::path::Path>) -> bool {
    let Some(path) = path else {
        return false;
    };
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    meta.is_dir() && !meta.permissions().readonly()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implicit_profile_path_locality_checks_existing_writable_dirs_only() {
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join("file");
        std::fs::write(&file, "not a dir").expect("write file");
        let missing = dir.path().join("missing");

        assert!(!implicit_profile_path_is_local(None));
        assert!(!implicit_profile_path_is_local(Some(&missing)));
        assert!(!implicit_profile_path_is_local(Some(&file)));
        assert!(implicit_profile_path_is_local(Some(dir.path())));
    }

    #[test]
    fn jar_paths_match_normalizes_current_dir_only() {
        let a = std::path::Path::new("/tmp/afhttp/./cookies.jar.json");
        let b = std::path::Path::new("/tmp/afhttp/cookies.jar.json");
        let c = std::path::Path::new("/tmp/other/cookies.jar.json");

        assert!(jar_paths_match(a, b));
        assert!(!jar_paths_match(a, c));
    }

    #[test]
    fn persist_set_cookies_ignores_invalid_lines_and_writes_valid_cookie() {
        let dir = tempfile::tempdir().expect("dir");
        let jar_path = dir.path().join("cookies.jar.json");
        let url = Url::parse("https://example.test/path/page").expect("url");
        let lines = vec![
            "sid=abc; Path=/path; Secure; HttpOnly".to_string(),
            "not a cookie; bad".to_string(),
        ];

        persist_set_cookies(&jar_path, &lines, &url).expect("persist");
        let jar = crate::sdk::profile::cookie_jar::CookieJar::load(&jar_path).expect("load");
        let cookies = jar.applicable_cookies(&url);

        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].name(), "sid");
        assert_eq!(cookies[0].value(), "abc");
    }

    #[test]
    fn cdp_cookie_to_set_cookie_maps_supported_attributes() {
        let entry = serde_json::json!({
            "name": "sid",
            "value": "abc",
            "domain": ".example.test",
            "path": "/account",
            "secure": true,
            "httpOnly": true,
            "expires": 1893456000.0
        });

        let cookie = cdp_cookie_to_set_cookie(&entry).expect("cookie");

        assert_eq!(cookie.name(), "sid");
        assert_eq!(cookie.value(), "abc");
        assert_eq!(cookie.domain(), Some("example.test"));
        assert_eq!(cookie.path(), Some("/account"));
        assert_eq!(cookie.secure(), Some(true));
        assert_eq!(cookie.http_only(), Some(true));
        assert!(cookie.expires().is_some());
    }

    #[test]
    fn cdp_cookie_to_set_cookie_rejects_missing_required_fields() {
        assert!(cdp_cookie_to_set_cookie(&serde_json::json!({"value": "abc"})).is_none());
        assert!(cdp_cookie_to_set_cookie(&serde_json::json!({"name": "sid"})).is_none());
    }
}
