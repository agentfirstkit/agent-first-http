//! Header-name redaction list applied to `network.json` by default (and to
//! any tool-originated log line). Per `design.md §"Secrets are redacted"`,
//! credential-bearing headers are replaced with `"[redacted]"` unless the
//! caller passes `--no-network-redact`.
//!
//! Server response **bodies** pass through unmodified — redaction is for
//! tool-captured metadata only. URLs pass through too: the SDK hands back what
//! it fetched and what failed as it was, and the CLI and host redact URL
//! credentials when they write an AFDATA event.

/// Header names that are always redacted (case-insensitive match).
pub const ALWAYS_REDACTED: &[&str] = &[
    "cookie",
    "set-cookie",
    "authorization",
    "proxy-authorization",
];

/// Header-name *suffixes* that trigger redaction (case-insensitive match
/// against the full header). Catches `x-api-token`, `x-csrf-token`,
/// `x-shared-secret`, etc.
pub const REDACTED_SUFFIXES: &[&str] = &["-token", "-secret"];

/// Legacy/common URL query parameter names that carry credentials but cannot
/// be renamed by afhttp because they belong to third-party sites.
///
/// AFDATA already redacts `_secret` query names. This compatibility list is
/// deliberately exact and is applied only while parsing a single URL, by the
/// host and CLI as they emit events.
#[cfg(feature = "host")]
pub const URL_SECRET_NAMES: &[&str] = &[
    "access_token",
    "api_key",
    "auth",
    "authorization",
    "code",
    "handoff",
    "handoff_secret",
    "id_token",
    "key",
    "password",
    "passwd",
    "session",
    "sessionid",
    "sig",
    "signature",
    "token",
];

/// Returns true if `header_name` should be redacted under the default
/// policy. Case-insensitive.
#[must_use]
pub fn should_redact(header_name: &str) -> bool {
    let lower = header_name.to_ascii_lowercase();
    if ALWAYS_REDACTED.iter().any(|h| *h == lower) {
        return true;
    }
    REDACTED_SUFFIXES
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

/// Sentinel string written in place of redacted values.
pub const REDACTED_VALUE: &str = "[redacted]";

/// AFDATA redactor configured for third-party HTTP URL compatibility names.
///
/// Keep this URL-only: AFDATA `secret_names` also match ordinary structured
/// field names, where names such as the protocol's `code` field are public.
#[cfg(feature = "host")]
#[must_use]
fn url_redactor() -> agent_first_data::Redactor {
    agent_first_data::Redactor::new().secret_names(URL_SECRET_NAMES.iter().copied())
}

/// Redact credentials from one URL, including userinfo and common third-party
/// query parameter names.
#[cfg(feature = "host")]
#[must_use]
pub fn redact_url(url: &str) -> String {
    url_redactor().url(url)
}

/// Apply the same credential policy to complete URLs embedded in diagnostic
/// prose, without treating ordinary structured fields such as `code` as secret.
#[cfg(feature = "host")]
#[must_use]
pub(crate) fn redact_urls_in_text(text: &str) -> String {
    url_redactor().urls_in_text(text)
}

/// Redact third-party credential parameters in the fields of an emitted
/// payload that carry URLs.
///
/// Anywhere in the payload, that is the `_url`/`_URL` fields, which are
/// afhttp's own naming for a URL. Beyond them only afhttp's own envelope is
/// read, by position: a fetch result's `warnings[].detail` and
/// `next_action.recommended_command`, and the `targets[].url` of a `tabs`
/// listing. Prose is redacted URL span by URL span, so the rest of the
/// sentence is untouched. A field merely *named* `url` or `detail` elsewhere is
/// not read: `afhttp cdp` returns the browser's own answer, and a page's
/// navigation history or an evaluated object is reported as the page gave it.
///
/// That includes `_url` names: a `cdp` event's `result` is the browser's answer
/// and is skipped whole, so an evaluated `{"next_url": ...}` keeps its query.
/// The `cdp` command also emits without the AFDATA `_secret` redactor (see
/// `cli::output::emit_raw_capture`), so `{"api_secret": ...}` survives too.
///
/// AFDATA deliberately uses one exact-name list for both structured fields and
/// URL query parameters. Since afhttp does not own third-party parameter names,
/// it applies that compatibility list to URL values first, then lets the
/// default AFDATA redactor handle `_secret` fields at serialization time.
#[cfg(feature = "host")]
#[must_use]
pub fn redact_url_fields(value: &serde_json::Value) -> serde_json::Value {
    let mut value = value.clone();
    if let Some(raw) = take_raw_capture(&mut value) {
        redact_url_fields_in_place(&mut value);
        redact_envelope_prose(&mut value);
        if let serde_json::Value::Object(fields) = &mut value {
            fields.insert("result".into(), raw);
        }
        return value;
    }
    redact_url_fields_in_place(&mut value);
    redact_envelope_prose(&mut value);
    value
}

/// The `result` of a `cdp` event, lifted out so no redaction walk reaches it.
#[cfg(feature = "host")]
fn take_raw_capture(value: &mut serde_json::Value) -> Option<serde_json::Value> {
    if value.get("code").and_then(|code| code.as_str()) != Some("cdp") {
        return None;
    }
    value.as_object_mut()?.remove("result")
}

#[cfg(feature = "host")]
fn redact_envelope_prose(value: &mut serde_json::Value) {
    if let Some(warnings) = value.get_mut("warnings").and_then(|v| v.as_array_mut()) {
        for warning in warnings {
            if let Some(serde_json::Value::String(detail)) = warning.get_mut("detail") {
                *detail = redact_urls_in_text(detail);
            }
        }
    }
    if let Some(serde_json::Value::String(command)) = value
        .get_mut("next_action")
        .and_then(|next| next.get_mut("recommended_command"))
    {
        *command = redact_urls_in_text(command);
    }
    if value.get("code").and_then(|code| code.as_str()) == Some("tabs")
        && let Some(targets) = value.get_mut("targets").and_then(|v| v.as_array_mut())
    {
        for target in targets {
            if let Some(serde_json::Value::String(url)) = target.get_mut("url") {
                *url = redact_url(url);
            }
        }
    }
}

/// Apply afhttp's URL compatibility policy followed by standard AFDATA value
/// redaction.
#[cfg(feature = "host")]
#[must_use]
pub fn redact_value(value: &serde_json::Value) -> serde_json::Value {
    agent_first_data::Redactor::new().value(&redact_url_fields(value))
}

#[cfg(feature = "host")]
fn redact_url_fields_in_place(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            for (name, value) in fields {
                match value {
                    serde_json::Value::String(url)
                        if name.ends_with("_url") || name.ends_with("_URL") =>
                    {
                        *url = redact_url(url);
                    }
                    _ => redact_url_fields_in_place(value),
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                redact_url_fields_in_place(value);
            }
        }
        _ => {}
    }
}

/// Mask the password component of every `scheme://user:pass@host` userinfo
/// found in arbitrary text, leaving the rest byte-for-byte intact.
///
/// Used on browser stderr lines surfaced by `/diagnostics`, which can echo
/// afhttp's own `--proxy-server=http://user:pass@host` launch argument. This
/// is afhttp's own injected credential — never page-captured data — so masking
/// it does not violate faithful capture. afdata's `redact_url_secrets` only
/// operates on a string that is itself a single URL, so it cannot be used on a
/// prose line; this is the narrow, userinfo-password-only equivalent.
#[must_use]
pub fn redact_userinfo_passwords(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(pos) = rest.find("://") {
        // Copy through the scheme separator.
        out.push_str(&rest[..pos + 3]);
        let after = &rest[pos + 3..];
        // Authority ends at the first '/', '?', '#', or whitespace.
        let auth_end = after
            .find(|c: char| matches!(c, '/' | '?' | '#') || c.is_whitespace())
            .unwrap_or(after.len());
        let authority = &after[..auth_end];
        match (authority.find('@'), authority.find(':')) {
            (Some(at), Some(colon)) if colon < at => {
                out.push_str(&authority[..colon]);
                out.push_str(":***");
                out.push_str(&authority[at..]);
            }
            _ => out.push_str(authority),
        }
        rest = &after[auth_end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_redacted_headers_match_case_insensitively() {
        for h in ["Cookie", "COOKIE", "cookie", "Set-Cookie", "SET-COOKIE"] {
            assert!(should_redact(h), "{h}");
        }
        assert!(should_redact("Authorization"));
        assert!(should_redact("Proxy-Authorization"));
    }

    #[test]
    fn suffix_match_catches_token_and_secret_headers() {
        assert!(should_redact("X-Api-Token"));
        assert!(should_redact("x-csrf-token"));
        assert!(should_redact("X-Shared-Secret"));
        assert!(should_redact("x-bearer-secret"));
    }

    #[test]
    fn unrelated_headers_pass_through() {
        for h in ["Content-Type", "User-Agent", "Accept", "X-Trace-Id"] {
            assert!(!should_redact(h), "{h}");
        }
    }

    #[cfg(feature = "host")]
    #[test]
    fn url_redaction_covers_userinfo_and_legacy_query_names() {
        assert_eq!(
            redact_url("https://user:pass@example.test/path?token=abc&safe=ok&next_secret=hidden"),
            "https://user:***@example.test/path?token=***&safe=ok&next_secret=***"
        );
    }

    #[cfg(feature = "host")]
    #[test]
    fn url_compatibility_names_do_not_redact_ordinary_fields() {
        let redacted = redact_value(&serde_json::json!({
            "code": "fetch",
            "token": "public field",
            "request_url": "https://example.test/?code=otp&token=bearer&safe=ok",
            "token_secret": "private"
        }));
        assert_eq!(redacted["code"], "fetch");
        assert_eq!(redacted["token"], "public field");
        assert_eq!(
            redacted["request_url"],
            "https://example.test/?code=***&token=***&safe=ok"
        );
        assert_eq!(redacted["token_secret"], "***");
    }

    /// The fields of an emitted payload that quote a URL without being named
    /// `_url`: a warning's `detail`, a CDP target's `url`, and the command a
    /// result recommends. Text the page produced is left as it was.
    #[cfg(feature = "host")]
    #[test]
    fn emitted_prose_and_cdp_urls_lose_their_credentials_but_page_text_does_not() {
        let redacted = redact_url_fields(&serde_json::json!({
            "code": "tabs",
            "warnings": [{"detail": "cookie jar sync: https://a.test/?token=t1&safe=ok refused"}],
            "targets": [{"url": "https://u:p@a.test/cb?access_token=t2"}],
            "next_action": {"recommended_command": "afhttp fetch 'https://a.test/?api_key=t3' --takeover"},
            "text": "the page said https://a.test/?token=page-own",
        }));
        assert_eq!(
            redacted["warnings"][0]["detail"],
            "cookie jar sync: https://a.test/?token=***&safe=ok refused"
        );
        assert_eq!(
            redacted["targets"][0]["url"],
            "https://u:***@a.test/cb?access_token=***"
        );
        assert!(
            !redacted["next_action"]["recommended_command"]
                .as_str()
                .unwrap()
                .contains("t3")
        );
        assert_eq!(
            redacted["text"],
            "the page said https://a.test/?token=page-own"
        );

        // `afhttp cdp` returns the browser's own answer: a navigation entry's
        // `url` or an evaluated object's `detail` is the page's, not afhttp's.
        let cdp = serde_json::json!({
            "code": "cdp",
            "result": {
                "entries": [{"url": "https://a.test/?token=page-own"}],
                "value": {"detail": "see https://a.test/?token=page-own"},
            },
        });
        assert_eq!(redact_url_fields(&cdp), cdp);

        // Nor are `_url` names inside it afhttp's: an evaluated object is
        // returned as the page built it.
        let cdp = serde_json::json!({
            "code": "cdp",
            "result": {"result": {"value": {
                "next_url": "https://a.test/?token=page-own",
                "nested": [{"callback_url": "https://u:p@a.test/?access_token=x"}],
            }}},
        });
        assert_eq!(redact_url_fields(&cdp), cdp);

        // Any other command's `_url` fields are still afhttp's own.
        let fetch = serde_json::json!({
            "code": "fetch",
            "result": {"final_url": "https://a.test/?token=t"},
        });
        assert_eq!(
            redact_url_fields(&fetch)["result"]["final_url"],
            "https://a.test/?token=***"
        );
    }

    #[test]
    fn userinfo_password_is_masked_in_text() {
        assert_eq!(
            redact_userinfo_passwords("launch --proxy-server=http://user:pass@proxy:8080 done"),
            "launch --proxy-server=http://user:***@proxy:8080 done"
        );
        // username-only userinfo, host:port, and non-URL text are untouched.
        assert_eq!(
            redact_userinfo_passwords("http://user@host/x"),
            "http://user@host/x"
        );
        assert_eq!(
            redact_userinfo_passwords("connect socks5://10.0.0.5:1080 now"),
            "connect socks5://10.0.0.5:1080 now"
        );
        assert_eq!(redact_userinfo_passwords("no url here"), "no url here");
    }
}
