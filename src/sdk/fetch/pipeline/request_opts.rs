//! `PreparedRequestOptions` — validated, normalized form of all per-fetch
//! request modifiers (headers, cookies, user-agent, post-wait JS).

use std::collections::BTreeMap;

use cookie::{Expiration, SameSite};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, COOKIE, USER_AGENT};
use url::Url;

use crate::sdk::fetch::FetchBuilder;
use crate::shared::error::{Error, ErrorCode};

/// Request body variants. Resolved from the builder's `body`/`form` fields.
#[derive(Clone, Debug)]
pub(crate) enum BodyPayload {
    None,
    Bytes(Vec<u8>),
    Form(Vec<(String, String)>),
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedRequestOptions {
    pub(crate) headers: Vec<PreparedHeader>,
    pub(crate) user_agent: Option<(String, HeaderValue)>,
    pub(crate) raw_cookie_header: Option<(String, HeaderValue)>,
    pub(crate) cookies: Vec<PreparedCookie>,
    pub(crate) raw_cookie_pairs: Vec<PreparedCookie>,
    pub(crate) evaluate_after_wait: Vec<String>,
    pub(crate) method: String,
    pub(crate) body_payload: BodyPayload,
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedHeader {
    pub(crate) name: HeaderName,
    pub(crate) value: HeaderValue,
    pub(crate) cdp_name: String,
    pub(crate) cdp_value: String,
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedCookie {
    pub(crate) name: String,
    pub(crate) value: String,
    pub(crate) domain: Option<String>,
    pub(crate) path: Option<String>,
    pub(crate) secure: Option<bool>,
    pub(crate) http_only: Option<bool>,
    pub(crate) same_site: Option<SameSite>,
    pub(crate) partitioned: Option<bool>,
    pub(crate) expires_epoch_s: Option<i64>,
}

impl PreparedRequestOptions {
    pub(crate) fn from_builder(builder: &FetchBuilder) -> Result<Self, Error> {
        let mut headers = Vec::new();
        let mut user_agent_from_header: Option<(String, HeaderValue)> = None;
        let mut raw_cookie_header: Option<(String, HeaderValue)> = None;

        for (name, value) in &builder.request.headers {
            let (header_name, header_value) = parse_header(name, value, ".header(...)")?;
            if header_name == USER_AGENT {
                if user_agent_from_header.is_some() {
                    return Err(Error::new(
                        ErrorCode::InvalidArgument,
                        "multiple User-Agent headers were provided",
                    ));
                }
                user_agent_from_header = Some((value.clone(), header_value));
                continue;
            }
            if header_name == COOKIE {
                if raw_cookie_header.is_some() {
                    return Err(Error::new(
                        ErrorCode::InvalidArgument,
                        "multiple Cookie headers were provided",
                    ));
                }
                raw_cookie_header = Some((value.clone(), header_value));
                continue;
            }
            headers.push(PreparedHeader {
                cdp_name: header_name.as_str().to_string(),
                cdp_value: value.clone(),
                name: header_name,
                value: header_value,
            });
        }

        if builder.request.user_agent.is_some() && user_agent_from_header.is_some() {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                ".user_agent(...) conflicts with .header(\"User-Agent\", ...)",
            ));
        }
        let user_agent = if let Some(value) = builder.request.user_agent.as_ref() {
            let (_, header_value) = parse_header("User-Agent", value, ".user_agent(...)")?;
            Some((value.clone(), header_value))
        } else {
            user_agent_from_header
        };

        if raw_cookie_header.is_some() && !builder.request.cookies.is_empty() {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                ".cookie(...) conflicts with .header(\"Cookie\", ...)",
            ));
        }

        let mut cookies = Vec::new();
        for cookie in &builder.request.cookies {
            cookies.push(prepare_cookie(cookie, ".cookie(...)")?);
        }

        let raw_cookie_pairs = match raw_cookie_header.as_ref() {
            Some((value, _)) => parse_cookie_header(value)?,
            None => Vec::new(),
        };

        if builder.request.body.is_some() && !builder.request.form.is_empty() {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "--data and --form are mutually exclusive",
            ));
        }
        let body_payload = if let Some(bytes) = builder.request.body.clone() {
            BodyPayload::Bytes(bytes)
        } else if !builder.request.form.is_empty() {
            BodyPayload::Form(builder.request.form.clone())
        } else {
            BodyPayload::None
        };

        let method = builder
            .request
            .method
            .clone()
            .unwrap_or_else(|| "GET".into())
            .to_uppercase();

        Ok(Self {
            headers,
            user_agent,
            raw_cookie_header,
            cookies,
            raw_cookie_pairs,
            evaluate_after_wait: builder.request.evaluate_after_wait.clone(),
            method,
            body_payload,
        })
    }

    pub(crate) fn http_header_map(&self, url: &str) -> Result<HeaderMap, Error> {
        let mut map = HeaderMap::new();
        for header in &self.headers {
            map.insert(header.name.clone(), header.value.clone());
        }
        if let Some((_, value)) = &self.user_agent {
            map.insert(USER_AGENT, value.clone());
        }
        if let Some((_, value)) = &self.raw_cookie_header {
            map.insert(COOKIE, value.clone());
        } else if let Some(value) = self.cookie_header_for_url(url)? {
            map.insert(COOKIE, value);
        }
        Ok(map)
    }

    fn cookie_header_for_url(&self, url: &str) -> Result<Option<HeaderValue>, Error> {
        if self.cookies.is_empty() {
            return Ok(None);
        }
        let url = parse_cookie_url(url)?;
        let mut pairs = Vec::new();
        for cookie in &self.cookies {
            validate_cookie_scope(cookie, &url, ".cookie(...)")?;
            if effective_cookie_secure(cookie) && url.scheme() != "https" {
                continue;
            }
            if cookie_is_expired(cookie) {
                continue;
            }
            pairs.push(format!("{}={}", cookie.name, cookie.value));
        }
        if pairs.is_empty() {
            return Ok(None);
        }
        let value = pairs.join("; ");
        Ok(Some(HeaderValue::from_str(&value).map_err(|e| {
            Error::new(
                ErrorCode::InvalidArgument,
                format!("cookie header value is invalid: {e}"),
            )
        })?))
    }

    pub(crate) fn cdp_extra_headers(&self) -> BTreeMap<String, String> {
        self.headers
            .iter()
            .map(|header| (header.cdp_name.clone(), header.cdp_value.clone()))
            .collect()
    }

    pub(crate) fn merge_jar_cookies(&mut self, extras: Vec<PreparedCookie>) {
        let existing: std::collections::HashSet<String> =
            self.cookies.iter().map(|c| c.name.clone()).collect();
        for c in extras {
            if !existing.contains(&c.name) {
                self.cookies.push(c);
            }
        }
    }

    pub(crate) fn browser_cookies(&self) -> &[PreparedCookie] {
        if self.raw_cookie_header.is_some() {
            &self.raw_cookie_pairs
        } else {
            &self.cookies
        }
    }

    pub(crate) fn has_evaluate_after_wait(&self) -> bool {
        !self.evaluate_after_wait.is_empty()
    }
}

fn parse_header(name: &str, value: &str, source: &str) -> Result<(HeaderName, HeaderValue), Error> {
    let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
        Error::new(
            ErrorCode::InvalidArgument,
            format!("{source}: invalid header name {name:?}: {e}"),
        )
    })?;
    let header_value = HeaderValue::from_str(value).map_err(|e| {
        Error::new(
            ErrorCode::InvalidArgument,
            format!("{source}: invalid value for header {name:?}: {e}"),
        )
    })?;
    Ok((header_name, header_value))
}

pub(crate) fn prepare_cookie(
    cookie: &cookie::Cookie<'static>,
    source: &str,
) -> Result<PreparedCookie, Error> {
    validate_cookie_pair(cookie.name(), cookie.value(), source)?;
    if cookie.partitioned() == Some(true) && cookie.secure() == Some(false) {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            format!("{source}: Partitioned cookies require Secure"),
        ));
    }
    if cookie.same_site() == Some(SameSite::None) && cookie.secure() == Some(false) {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            format!("{source}: SameSite=None cookies require Secure"),
        ));
    }
    Ok(PreparedCookie {
        name: cookie.name().to_string(),
        value: cookie.value().to_string(),
        domain: cookie.domain().map(str::to_ascii_lowercase),
        path: cookie.path().map(str::to_string),
        secure: cookie.secure(),
        http_only: cookie.http_only(),
        same_site: cookie.same_site(),
        partitioned: cookie.partitioned(),
        expires_epoch_s: cookie_expires_epoch_s(cookie),
    })
}

fn validate_cookie_pair(name: &str, value: &str, source: &str) -> Result<(), Error> {
    let trimmed_name = name.trim();
    if trimmed_name.is_empty() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            format!("{source}: cookie name must not be empty"),
        ));
    }
    if trimmed_name
        .bytes()
        .any(|b| b == b';' || b == b'=' || b.is_ascii_control() || b.is_ascii_whitespace())
    {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            format!("{source}: invalid cookie name {name:?}"),
        ));
    }
    if value.bytes().any(|b| b == b';' || b == b'\r' || b == b'\n') {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            format!("{source}: invalid cookie value for {trimmed_name:?}"),
        ));
    }
    Ok(())
}

fn parse_cookie_header(value: &str) -> Result<Vec<PreparedCookie>, Error> {
    let mut cookies = Vec::new();
    for parsed in cookie::Cookie::split_parse(value.to_string()) {
        let cookie = parsed.map_err(|e| {
            Error::new(
                ErrorCode::InvalidArgument,
                format!(".header(\"Cookie\", ...): invalid cookie header: {e}"),
            )
        })?;
        cookies.push(prepare_cookie(
            &cookie.into_owned(),
            ".header(\"Cookie\", ...)",
        )?);
    }
    Ok(cookies)
}

fn cookie_expires_epoch_s(cookie: &cookie::Cookie<'static>) -> Option<i64> {
    if let Some(max_age) = cookie.max_age() {
        return cookie::time::OffsetDateTime::now_utc()
            .checked_add(max_age)
            .map(|t| t.unix_timestamp());
    }
    match cookie.expires() {
        Some(Expiration::DateTime(t)) => Some(t.unix_timestamp()),
        Some(Expiration::Session) | None => None,
    }
}

pub(crate) fn cookie_is_expired(cookie: &PreparedCookie) -> bool {
    cookie
        .expires_epoch_s
        .is_some_and(|expires| expires <= cookie::time::OffsetDateTime::now_utc().unix_timestamp())
}

pub(crate) fn parse_cookie_url(url: &str) -> Result<Url, Error> {
    Url::parse(url).map_err(|e| {
        Error::new(
            ErrorCode::InvalidArgument,
            format!("cookie URL scope requires an absolute URL: {e}"),
        )
    })
}

pub(crate) fn validate_cookie_scope(
    cookie: &PreparedCookie,
    url: &Url,
    source: &str,
) -> Result<(), Error> {
    if let Some(domain) = &cookie.domain {
        let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                format!(
                    "{source}: cookie {:?} has Domain but URL has no host",
                    cookie.name
                ),
            ));
        };
        if host != *domain && !host.ends_with(&format!(".{domain}")) {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                format!(
                    "{source}: cookie {:?} Domain={domain:?} does not match URL host {host:?}",
                    cookie.name
                ),
            ));
        }
    }
    if let Some(path) = &cookie.path {
        if !url.path().starts_with(path) {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                format!(
                    "{source}: cookie {:?} Path={path:?} does not match URL path {:?}",
                    cookie.name,
                    url.path()
                ),
            ));
        }
    }
    Ok(())
}

pub(crate) fn effective_cookie_secure(cookie: &PreparedCookie) -> bool {
    cookie.secure == Some(true)
        || cookie.same_site == Some(SameSite::None)
        || cookie.partitioned == Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared(name: &str) -> PreparedCookie {
        PreparedCookie {
            name: name.to_string(),
            value: "v".to_string(),
            domain: None,
            path: None,
            secure: None,
            http_only: None,
            same_site: None,
            partitioned: None,
            expires_epoch_s: None,
        }
    }

    #[test]
    fn parse_header_accepts_valid_rejects_bad_name_and_value() {
        let (n, v) = parse_header("X-Test", "ok", "src").unwrap();
        assert_eq!(n.as_str(), "x-test");
        assert_eq!(v.to_str().unwrap(), "ok");
        assert!(parse_header("Bad Name", "v", "src").is_err());
        assert!(parse_header("X-Test", "bad\nvalue", "src").is_err());
    }

    #[test]
    fn validate_cookie_pair_enforces_name_and_value_rules() {
        assert!(validate_cookie_pair("ok", "value", "s").is_ok());
        assert!(validate_cookie_pair("  ", "v", "s").is_err()); // empty name
        assert!(validate_cookie_pair("a=b", "v", "s").is_err()); // '=' in name
        assert!(validate_cookie_pair("a;b", "v", "s").is_err()); // ';' in name
        assert!(validate_cookie_pair("ok", "a;b", "s").is_err()); // ';' in value
        assert!(validate_cookie_pair("ok", "a\nb", "s").is_err()); // newline in value
    }

    #[test]
    fn prepare_cookie_happy_path_and_security_rules() {
        let c = cookie::Cookie::parse("foo=bar").unwrap().into_owned();
        let p = prepare_cookie(&c, "s").unwrap();
        assert_eq!(p.name, "foo");
        assert_eq!(p.value, "bar");

        // Partitioned cookies require Secure.
        let c = cookie::Cookie::build(("p", "v"))
            .partitioned(true)
            .secure(false)
            .build();
        assert!(prepare_cookie(&c, "s").is_err());

        // SameSite=None cookies require Secure.
        let c = cookie::Cookie::build(("p", "v"))
            .same_site(SameSite::None)
            .secure(false)
            .build();
        assert!(prepare_cookie(&c, "s").is_err());

        // Domain is normalized to lowercase.
        let c = cookie::Cookie::build(("d", "v"))
            .domain("EXAMPLE.com")
            .secure(true)
            .same_site(SameSite::None)
            .build();
        let p = prepare_cookie(&c, "s").unwrap();
        assert_eq!(p.domain.as_deref(), Some("example.com"));
    }

    #[test]
    fn parse_cookie_header_splits_pairs() {
        let cookies = parse_cookie_header("a=1; b=2").unwrap();
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[0].name, "a");
        assert_eq!(cookies[1].name, "b");
    }

    #[test]
    fn cookie_expires_epoch_s_handles_max_age_datetime_and_session() {
        // max-age resolves relative to now.
        let c = cookie::Cookie::build(("m", "v"))
            .max_age(cookie::time::Duration::seconds(3600))
            .build();
        let exp = cookie_expires_epoch_s(&c).unwrap();
        let now = cookie::time::OffsetDateTime::now_utc().unix_timestamp();
        assert!((exp - now - 3600).abs() <= 5, "exp={exp} now={now}");

        // explicit datetime is passed through.
        let dt = cookie::time::OffsetDateTime::from_unix_timestamp(1_000_000_000).unwrap();
        let c = cookie::Cookie::build(("d", "v"))
            .expires(Expiration::DateTime(dt))
            .build();
        assert_eq!(cookie_expires_epoch_s(&c), Some(1_000_000_000));

        // session expiry has no unix stamp.
        let c = cookie::Cookie::build(("s", "v"))
            .expires(Expiration::Session)
            .build();
        assert_eq!(cookie_expires_epoch_s(&c), None);
    }

    #[test]
    fn cookie_is_expired_compares_against_now() {
        let mut past = prepared("x");
        past.expires_epoch_s = Some(1); // 1970
        assert!(cookie_is_expired(&past));

        let mut future = prepared("x");
        future.expires_epoch_s = Some(i64::MAX / 2);
        assert!(!cookie_is_expired(&future));

        // Session cookies (no stamp) never expire by time.
        assert!(!cookie_is_expired(&prepared("x")));
    }

    #[test]
    fn parse_cookie_url_requires_absolute_url() {
        assert!(parse_cookie_url("https://example.com/").is_ok());
        assert!(parse_cookie_url("not a url").is_err());
    }

    #[test]
    fn validate_cookie_scope_checks_domain_and_path() {
        let url = parse_cookie_url("https://app.example.com/admin/page").unwrap();

        // Subdomain of the cookie Domain is in scope.
        let mut c = prepared("x");
        c.domain = Some("example.com".to_string());
        assert!(validate_cookie_scope(&c, &url, "s").is_ok());

        // Unrelated domain is rejected.
        let mut c = prepared("x");
        c.domain = Some("other.com".to_string());
        assert!(validate_cookie_scope(&c, &url, "s").is_err());

        // Domain set but URL has no host.
        let hostless = parse_cookie_url("data:text/plain,x").unwrap();
        let mut c = prepared("x");
        c.domain = Some("example.com".to_string());
        assert!(validate_cookie_scope(&c, &hostless, "s").is_err());

        // Path prefix must match.
        let mut c = prepared("x");
        c.path = Some("/admin".to_string());
        assert!(validate_cookie_scope(&c, &url, "s").is_ok());
        let mut c = prepared("x");
        c.path = Some("/other".to_string());
        assert!(validate_cookie_scope(&c, &url, "s").is_err());
    }

    #[test]
    fn effective_cookie_secure_covers_each_trigger() {
        assert!(!effective_cookie_secure(&prepared("x")));

        let mut c = prepared("x");
        c.secure = Some(true);
        assert!(effective_cookie_secure(&c));

        let mut c = prepared("x");
        c.same_site = Some(SameSite::None);
        assert!(effective_cookie_secure(&c));

        let mut c = prepared("x");
        c.partitioned = Some(true);
        assert!(effective_cookie_secure(&c));
    }
}
