//! Persistent cookie jar for the HTTP fast path.
//!
//! The browser path already persists cookies through chromium's own
//! user-data-dir storage when the host runs with `--profile <name>`. The
//! HTTP path (`--render none`, or the `auto` path before escalation) has
//! no such backing store, so an agent that does `login → navigate` over
//! HTTP loses every Set-Cookie unless it parses the network artifact and
//! manually replays cookies on the next request.
//!
//! This module provides a JSON-on-disk jar keyed on the agent's chosen
//! path. Pipeline loads it before issuing the request, applies cookies
//! whose (domain, path) matches the target URL, then merges any
//! Set-Cookie responses back. Atomic via tempfile + rename; concurrent
//! pipeline runs serialize on an fs2 advisory lock on a `.lock` sibling.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use cookie::{Cookie, Expiration, SameSite};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::shared::error::{Error, ErrorCode};

/// Serialized cookie entry. We use a deliberately small subset of RFC 6265
/// attributes — name/value/domain/path/expiry/flags. SameSite is preserved
/// for round-tripping but not used for matching.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct JarEntry {
    name: String,
    value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    domain: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    host_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_epoch_s: Option<i64>,
    #[serde(default, skip_serializing_if = "is_false")]
    secure: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    http_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    same_site: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Redacted cookie entry: value is stripped, metadata retained.
/// Used by `afhttp profile cookies <name>`.
#[derive(Debug, Clone, Serialize)]
pub struct RedactedCookie {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_epoch_s: Option<i64>,
    #[serde(skip_serializing_if = "is_false")]
    pub secure: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub http_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
}

/// Snapshot of the on-disk jar plus its source path. Held just long
/// enough to apply cookies to the outgoing request and to merge
/// post-response Set-Cookies back. Construct via [`load`].
#[derive(Debug)]
pub struct CookieJar {
    path: PathBuf,
    entries: Vec<JarEntry>,
}

impl CookieJar {
    /// Load the jar at `path`. A missing file returns an empty jar so the
    /// first fetch can write into it without a pre-step.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        let entries = match std::fs::read(&path) {
            Ok(bytes) if bytes.is_empty() => Vec::new(),
            Ok(bytes) => serde_json::from_slice::<Vec<JarEntry>>(&bytes).map_err(|e| {
                Error::new(
                    ErrorCode::IoError,
                    format!("cookie jar {}: parse error: {e}", path.display()),
                )
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                return Err(Error::new(
                    ErrorCode::IoError,
                    format!("cookie jar {}: read error: {e}", path.display()),
                ));
            }
        };
        Ok(Self { path, entries })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Return cookies whose (domain, path) match `url`, in jar order.
    /// Expired entries are filtered out. The caller injects them as a
    /// `Cookie` header (or via CDP `Network.setCookies` for the browser).
    pub fn applicable_cookies(&self, url: &Url) -> Vec<Cookie<'static>> {
        let now = unix_now();
        let host = url.host_str().unwrap_or("");
        let path = url.path();
        let https = url.scheme() == "https";
        self.entries
            .iter()
            .filter(|e| !e.is_expired(now))
            .filter(|e| !e.secure || https)
            .filter(|e| domain_matches(e.domain.as_deref(), e.host_only, host))
            .filter(|e| path_matches(e.path.as_deref(), path))
            .map(JarEntry::to_cookie)
            .collect()
    }

    /// Merge a freshly-received Set-Cookie into the jar in memory, or refuse it.
    ///
    /// A jar is shared across every site a profile visits, so what a response
    /// is *allowed* to write has to be checked before it is written. This used
    /// to take the `Domain` attribute at its word: a response from anywhere
    /// could say `Set-Cookie: sid=…; Domain=bank.example`, and the entry was
    /// then sent to `bank.example` on a later fetch — replacing, by RFC
    /// identity, whatever session cookie was already there. Returns `false`
    /// when the response was not permitted to set the cookie, so callers can
    /// count refusals rather than wonder where the cookie went.
    ///
    /// A `Max-Age` of zero or less is a deletion, and deleting is a merge
    /// outcome like any other: it expires the matching identity and reports
    /// success.
    pub fn merge(&mut self, set_cookie: Cookie<'static>, request_url: &Url) -> bool {
        let Some(host) = request_url.host_str() else {
            return false;
        };
        if !may_set_domain(set_cookie.domain(), host) {
            return false;
        }
        let entry = JarEntry::from_set_cookie(set_cookie.clone(), request_url);
        if let Some(idx) = self.entries.iter().position(|e| e.same_identity(&entry)) {
            // Replace in place.
            self.entries[idx] = entry;
        } else {
            self.entries.push(entry);
        }
        true
    }

    /// Drop expired entries; called automatically by [`Self::persist`] so
    /// the on-disk jar doesn't grow unboundedly.
    fn prune_expired(&mut self) {
        let now = unix_now();
        self.entries.retain(|e| !e.is_expired(now));
    }

    /// Write the jar back to disk atomically (tempfile + rename) under an
    /// fs2 advisory lock on a sibling `.lock` file. Concurrent pipeline
    /// runs serialize on the lock so neither corrupts the JSON.
    /// Return a redacted view of every non-expired entry. Values are replaced
    /// with `"[redacted]"` — names, domains, paths, and flags are exposed so
    /// an agent can diagnose jar state without exposing session secrets.
    pub fn cookies_redacted(&self) -> Vec<RedactedCookie> {
        let now = unix_now();
        self.entries
            .iter()
            .filter(|e| !e.is_expired(now))
            .map(|e| RedactedCookie {
                name: e.name.clone(),
                domain: e.domain.clone(),
                path: e.path.clone(),
                expires_epoch_s: e.expires_epoch_s,
                secure: e.secure,
                http_only: e.http_only,
                same_site: e.same_site.clone(),
            })
            .collect()
    }

    pub fn persist(mut self) -> Result<(), Error> {
        let parent = self.path.parent().ok_or_else(|| {
            Error::new(
                ErrorCode::IoError,
                format!("cookie jar {} has no parent dir", self.path.display()),
            )
        })?;
        std::fs::create_dir_all(parent).map_err(|e| {
            Error::new(
                ErrorCode::IoError,
                format!("create cookie jar dir {}: {e}", parent.display()),
            )
        })?;
        let lock_path = sibling_lock(&self.path);
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| {
                Error::new(
                    ErrorCode::IoError,
                    format!("open cookie jar lock {}: {e}", lock_path.display()),
                )
            })?;
        use fs2::FileExt;
        lock_file.lock_exclusive().map_err(|e| {
            Error::new(
                ErrorCode::IoError,
                format!("acquire cookie jar lock {}: {e}", lock_path.display()),
            )
        })?;

        // While we hold the lock, re-read the on-disk jar so a sibling
        // process's writes aren't clobbered. Then merge our in-memory
        // entries on top (newer wins by identity).
        let on_disk = Self::load(&self.path)?;
        let mut merged = on_disk.entries;
        for entry in self.entries.drain(..) {
            if let Some(idx) = merged.iter().position(|e| e.same_identity(&entry)) {
                merged[idx] = entry;
            } else {
                merged.push(entry);
            }
        }

        // Expired replacements must reach the merge first, otherwise a
        // deletion would resurrect the older on-disk cookie.
        self.entries = merged;
        self.prune_expired();
        let json = serde_json::to_vec_pretty(&self.entries)
            .map_err(|e| Error::new(ErrorCode::IoError, format!("serialize cookie jar: {e}")))?;
        atomic_write(&self.path, &json).map_err(|e| {
            Error::new(
                ErrorCode::IoError,
                format!("write cookie jar {}: {e}", self.path.display()),
            )
        })?;

        let _ = FileExt::unlock(&lock_file);
        Ok(())
    }
}

fn sibling_lock(path: &Path) -> PathBuf {
    let mut p = path.to_path_buf();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "cookies".into());
    p.set_file_name(format!("{name}.lock"));
    p
}

fn atomic_write(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = target.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "target has no parent dir")
    })?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file_mut().sync_all()?;
    tmp.persist(target).map_err(|e| e.error)?;
    Ok(())
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// RFC 6265 §5.1.3 domain matching. Cookie `Domain=example.com` matches
/// `host == example.com` AND `host == anything.example.com`. Cookies
/// without an explicit Domain match the exact host that set them.
fn domain_matches(cookie_domain: Option<&str>, host_only: bool, host: &str) -> bool {
    let host = host.trim_start_matches('.').to_ascii_lowercase();
    match cookie_domain {
        Some(d) => {
            let d = d.trim_start_matches('.').to_ascii_lowercase();
            if host_only {
                host == d
            } else {
                host == d || host.ends_with(&format!(".{d}"))
            }
        }
        None => false,
    }
}

/// RFC 6265 §5.1.4 path matching.
fn path_matches(cookie_path: Option<&str>, request_path: &str) -> bool {
    let cp = cookie_path.unwrap_or("/");
    if cp == request_path {
        return true;
    }
    if let Some(rest) = request_path.strip_prefix(cp) {
        // Either the cookie path ends with `/` (covers everything below)
        // or the next char in the request path is `/`.
        return cp.ends_with('/') || rest.starts_with('/');
    }
    false
}

impl JarEntry {
    /// RFC 6265 cookie identity: name + domain + path. Two entries with the
    /// same identity are the same cookie, so a newer one replaces the older
    /// rather than accumulating a duplicate. An absent domain/path compares
    /// as `""`/`"/"` so jar entries and freshly-merged ones line up.
    fn same_identity(&self, other: &JarEntry) -> bool {
        self.name == other.name
            && self.domain.as_deref().unwrap_or("") == other.domain.as_deref().unwrap_or("")
            && self.path.as_deref().unwrap_or("/") == other.path.as_deref().unwrap_or("/")
    }

    fn is_expired(&self, now: i64) -> bool {
        match self.expires_epoch_s {
            Some(t) => t <= now,
            None => false, // Session-only cookies survive in the jar.
        }
    }

    fn from_set_cookie(c: Cookie<'static>, request_url: &Url) -> Self {
        let host_only = c.domain().is_none();
        let domain = c
            .domain()
            .map(|d| d.trim_start_matches('.').to_string())
            .or_else(|| request_url.host_str().map(str::to_string));
        let path = c
            .path()
            .map(str::to_string)
            .or_else(|| Some(default_path(request_url)));
        // Max-Age takes precedence over Expires, including deletion.
        let expires_epoch_s = match c.max_age() {
            Some(age) if age.is_positive() => Some(unix_now().saturating_add(age.whole_seconds())),
            Some(_) => Some(0),
            None => c.expires().and_then(|e| match e {
                Expiration::DateTime(dt) => Some(dt.unix_timestamp()),
                Expiration::Session => None,
            }),
        };
        let same_site = c.same_site().map(|ss| match ss {
            SameSite::Strict => "Strict".to_string(),
            SameSite::Lax => "Lax".to_string(),
            SameSite::None => "None".to_string(),
        });
        Self {
            name: c.name().to_string(),
            value: c.value().to_string(),
            domain,
            host_only,
            path,
            expires_epoch_s,
            secure: c.secure().unwrap_or(false),
            http_only: c.http_only().unwrap_or(false),
            same_site,
        }
    }

    fn to_cookie(&self) -> Cookie<'static> {
        let mut b = Cookie::build((self.name.clone(), self.value.clone()));
        if let Some(d) = &self.domain {
            b = b.domain(d.clone());
        }
        if let Some(p) = &self.path {
            b = b.path(p.clone());
        }
        if self.secure {
            b = b.secure(true);
        }
        if self.http_only {
            b = b.http_only(true);
        }
        b.build()
    }
}

/// Whether a response from `host` may set a cookie for `cookie_domain`.
///
/// RFC 6265 §5.3: a `Domain` attribute is only honoured when the request host
/// domain-matches it — a site may broaden a cookie to its own parent domain and
/// no further. Omitting `Domain` is host-only and always permitted.
///
/// The bare-label rule below stands in for a public suffix list. Without one,
/// `Domain=co.uk` from `shop.co.uk` passes the domain-match test and would be
/// sent to every other `.co.uk` site in the jar. A full list is a dependency
/// with its own update cadence; refusing a single-label domain blocks the `com`
/// / `net` case, and the multi-label suffixes remain a narrower gap than the
/// one this closes.
fn may_set_domain(cookie_domain: Option<&str>, host: &str) -> bool {
    let Some(domain) = cookie_domain else {
        return true;
    };
    let domain = domain.trim_start_matches('.').to_ascii_lowercase();
    let host = host.trim_start_matches('.').to_ascii_lowercase();
    if domain.is_empty() {
        return false;
    }
    // An address is only ever host-only; `Domain` on one is not meaningful.
    if host.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    if !domain.contains('.') {
        return false;
    }
    host == domain || host.ends_with(&format!(".{domain}"))
}

/// RFC 6265 §5.1.4 default-path computation.
fn default_path(url: &Url) -> String {
    let path = url.path();
    if !path.starts_with('/') {
        return "/".into();
    }
    if let Some(idx) = path.rfind('/') {
        if idx == 0 {
            "/".into()
        } else {
            path[..idx].to_string()
        }
    } else {
        "/".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn persisted_cookie_deletions_do_not_resurrect_and_max_age_expires() {
        let url = make_url("https://example.test/");
        for deletion in [
            "sid=; Path=/; Max-Age=0",
            "sid=; Path=/; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
            "sid=; Path=/; Max-Age=0; Expires=Wed, 09 Jun 2100 10:18:14 GMT",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("jar.json");
            let mut jar = CookieJar::load(&path).unwrap();
            assert!(jar.merge(
                Cookie::parse("sid=live; Path=/").unwrap().into_owned(),
                &url
            ));
            jar.persist().unwrap();
            let mut jar = CookieJar::load(&path).unwrap();
            assert!(jar.merge(Cookie::parse(deletion).unwrap().into_owned(), &url));
            jar.persist().unwrap();
            let jar = CookieJar::load(&path).unwrap();
            assert!(jar.applicable_cookies(&url).is_empty());
            assert!(jar.entries.is_empty());
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path).unwrap())
                    .unwrap(),
                serde_json::json!([])
            );
        }
        let before = unix_now();
        let entry = JarEntry::from_set_cookie(
            Cookie::parse("sid=live; Max-Age=60; Expires=Thu, 01 Jan 1970 00:00:00 GMT")
                .unwrap()
                .into_owned(),
            &url,
        );
        assert!((before + 60..=unix_now() + 60).contains(&entry.expires_epoch_s.unwrap()));
    }

    #[test]
    fn empty_jar_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let jar = CookieJar::load(dir.path().join("nope.json")).unwrap();
        assert!(jar.entries.is_empty());
    }

    #[test]
    fn round_trip_preserves_set_cookie_basics() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jar.json");
        let url = make_url("https://example.com/foo/bar");
        let mut jar = CookieJar::load(&path).unwrap();
        let raw = Cookie::parse("session=abc123; Domain=example.com; Path=/; Secure; HttpOnly")
            .unwrap()
            .into_owned();
        jar.merge(raw, &url);
        jar.persist().unwrap();

        let jar = CookieJar::load(&path).unwrap();
        let cookies = jar.applicable_cookies(&url);
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].name(), "session");
        assert_eq!(cookies[0].value(), "abc123");
        assert_eq!(cookies[0].secure(), Some(true));
        assert_eq!(cookies[0].http_only(), Some(true));
    }

    #[test]
    fn a_site_cannot_set_a_cookie_for_a_domain_it_does_not_own() {
        // One jar serves every site a profile visits, so the interesting
        // question is not "did this parse" but "was this response allowed to
        // write it". A response from anywhere used to be able to claim
        // `Domain=bank.example` — and, by RFC identity, replace the session
        // cookie already stored for that site.
        let dir = tempfile::tempdir().unwrap();
        let mut jar = CookieJar::load(dir.path().join("jar.json")).unwrap();
        let bank = make_url("https://bank.example/");
        assert!(
            jar.merge(
                Cookie::parse("sid=real; Domain=bank.example; Path=/")
                    .unwrap()
                    .into_owned(),
                &bank,
            )
        );

        let attacker = make_url("https://evil.example/");
        for raw in [
            "sid=stolen; Domain=bank.example; Path=/",
            "sid=stolen; Domain=.bank.example; Path=/",
            // A public-suffix-shaped claim would reach every `.com` in the jar.
            "sid=stolen; Domain=com; Path=/",
        ] {
            assert!(
                !jar.merge(Cookie::parse(raw).unwrap().into_owned(), &attacker),
                "`{raw}` must be refused"
            );
        }
        let kept = jar.applicable_cookies(&make_url("https://bank.example/"));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].value(), "real", "the real session cookie survived");

        // A site broadening to its own parent domain is the legitimate case.
        let shop = make_url("https://shop.example.com/");
        assert!(
            jar.merge(
                Cookie::parse("pref=dark; Domain=example.com; Path=/")
                    .unwrap()
                    .into_owned(),
                &shop,
            )
        );
    }

    #[test]
    fn max_age_outranks_expires_and_zero_deletes() {
        // `Max-Age=0` is how a site logs you out. Reading only `Expires` turned
        // that into a session cookie that outlived the revocation.
        let dir = tempfile::tempdir().unwrap();
        let mut jar = CookieJar::load(dir.path().join("jar.json")).unwrap();
        let url = make_url("https://example.com/");
        assert!(
            jar.merge(
                Cookie::parse("sid=live; Domain=example.com; Path=/")
                    .unwrap()
                    .into_owned(),
                &url,
            )
        );
        assert_eq!(
            jar.applicable_cookies(&make_url("https://example.com/"))
                .len(),
            1
        );

        assert!(
            jar.merge(
                Cookie::parse("sid=; Domain=example.com; Path=/; Max-Age=0")
                    .unwrap()
                    .into_owned(),
                &url,
            )
        );
        assert!(
            jar.applicable_cookies(&make_url("https://example.com/"))
                .is_empty(),
            "Max-Age=0 must remove the cookie, not store an empty one"
        );

        // And a deletion still wins when `Expires` says otherwise.
        assert!(
            jar.merge(
                Cookie::parse("sid=live; Domain=example.com; Path=/")
                    .unwrap()
                    .into_owned(),
                &url,
            )
        );
        assert!(jar.merge(
            Cookie::parse(
                "sid=; Domain=example.com; Path=/; Max-Age=0; Expires=Wed, 09 Jun 2100 10:18:14 GMT"
            )
            .unwrap()
            .into_owned(),
            &url,
        ));
        assert!(
            jar.applicable_cookies(&make_url("https://example.com/"))
                .is_empty()
        );
    }

    #[test]
    fn second_merge_with_same_identity_overwrites_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jar.json");
        let url = make_url("https://example.com/");
        let mut jar = CookieJar::load(&path).unwrap();
        jar.merge(
            Cookie::parse("token=v1; Domain=example.com; Path=/")
                .unwrap()
                .into_owned(),
            &url,
        );
        jar.merge(
            Cookie::parse("token=v2; Domain=example.com; Path=/")
                .unwrap()
                .into_owned(),
            &url,
        );
        jar.persist().unwrap();
        let jar = CookieJar::load(&path).unwrap();
        let cookies = jar.applicable_cookies(&url);
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].value(), "v2");
    }

    #[test]
    fn domain_match_handles_subdomains() {
        assert!(domain_matches(Some("example.com"), false, "example.com"));
        assert!(domain_matches(
            Some("example.com"),
            false,
            "www.example.com"
        ));
        assert!(domain_matches(
            Some(".example.com"),
            false,
            "deep.www.example.com"
        ));
        assert!(!domain_matches(Some("example.com"), false, "example.org"));
        assert!(!domain_matches(Some("example.com"), false, "myexample.com"));
    }

    #[test]
    fn host_only_cookies_match_exact_host_only() {
        assert!(domain_matches(Some("example.com"), true, "example.com"));
        assert!(!domain_matches(
            Some("example.com"),
            true,
            "www.example.com"
        ));
        assert!(!domain_matches(None, true, "example.com"));
    }

    #[test]
    fn path_match_handles_prefixes() {
        assert!(path_matches(Some("/"), "/"));
        assert!(path_matches(Some("/"), "/anything"));
        assert!(path_matches(Some("/foo"), "/foo"));
        assert!(path_matches(Some("/foo"), "/foo/bar"));
        assert!(!path_matches(Some("/foo"), "/foobar"));
    }

    #[test]
    fn expired_cookies_are_filtered_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jar.json");
        let url = make_url("https://example.com/");
        let mut jar = CookieJar::load(&path).unwrap();
        // Cookie that expired in 1970.
        jar.entries.push(JarEntry {
            name: "old".into(),
            value: "stale".into(),
            domain: Some("example.com".into()),
            host_only: false,
            path: Some("/".into()),
            expires_epoch_s: Some(1),
            secure: false,
            http_only: false,
            same_site: None,
        });
        assert!(jar.applicable_cookies(&url).is_empty());
    }

    #[test]
    fn cookies_for_other_domain_are_not_returned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jar.json");
        let mut jar = CookieJar::load(&path).unwrap();
        jar.merge(
            Cookie::parse("a=1; Domain=example.com; Path=/")
                .unwrap()
                .into_owned(),
            &make_url("https://example.com/"),
        );
        let cookies = jar.applicable_cookies(&make_url("https://other.org/"));
        assert!(cookies.is_empty());
    }

    #[test]
    fn secure_cookies_are_skipped_for_http_urls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jar.json");
        let mut jar = CookieJar::load(&path).unwrap();
        jar.merge(
            Cookie::parse("secure_token=1; Domain=example.com; Path=/; Secure")
                .unwrap()
                .into_owned(),
            &make_url("https://example.com/"),
        );
        assert!(
            jar.applicable_cookies(&make_url("http://example.com/"))
                .is_empty()
        );
        assert_eq!(
            jar.applicable_cookies(&make_url("https://example.com/"))
                .len(),
            1
        );
    }

    #[test]
    fn concurrent_persists_preserve_both_writers() {
        // Two CookieJars, loaded independently, each merging a different
        // cookie, both call persist(). The final on-disk jar must contain
        // both cookies — the file lock during persist() makes the
        // re-read-and-merge atomic across the two.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jar.json");
        let url = make_url("https://example.com/");

        let p1 = path.clone();
        let p2 = path.clone();
        let h1 = std::thread::spawn(move || {
            let mut jar = CookieJar::load(&p1).unwrap();
            jar.merge(
                Cookie::parse("a=1; Domain=example.com; Path=/")
                    .unwrap()
                    .into_owned(),
                &make_url("https://example.com/"),
            );
            jar.persist().unwrap();
        });
        let h2 = std::thread::spawn(move || {
            let mut jar = CookieJar::load(&p2).unwrap();
            jar.merge(
                Cookie::parse("b=2; Domain=example.com; Path=/")
                    .unwrap()
                    .into_owned(),
                &make_url("https://example.com/"),
            );
            jar.persist().unwrap();
        });
        h1.join().unwrap();
        h2.join().unwrap();

        let jar = CookieJar::load(&path).unwrap();
        let cookies = jar.applicable_cookies(&url);
        let names: std::collections::BTreeSet<_> = cookies.iter().map(|c| c.name()).collect();
        assert!(names.contains("a"), "lost cookie a; got {names:?}");
        assert!(names.contains("b"), "lost cookie b; got {names:?}");
    }
}
