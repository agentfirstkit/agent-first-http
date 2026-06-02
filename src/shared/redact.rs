//! Header-name redaction list applied to `network.json` by default (and to
//! any tool-originated log line). Per `design.md §"Secrets are redacted"`,
//! credential-bearing headers are replaced with `"[redacted]"` unless the
//! caller passes `--network-redact off`.
//!
//! Server response **bodies** pass through unmodified — redaction is for
//! tool-captured metadata only.

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
