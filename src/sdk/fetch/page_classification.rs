//! Mechanical page classification for pages that load but are not the target
//! content an agent asked for (Cloudflare/Turnstile, generic access denials).

use crate::sdk::fetch::result::PageKind;
use crate::shared::error::ErrorCode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PageClassification {
    pub(crate) kind: PageKind,
    pub(crate) code: ErrorCode,
    pub(crate) detail: String,
}

/// Classify well-known security challenge / bot-wall HTML. The detector is
/// intentionally conservative: it looks for provider-specific markers or
/// several generic challenge terms before labelling content unverifiable.
pub(crate) fn classify(
    html: Option<&str>,
    text: Option<&str>,
    title: Option<&str>,
) -> Option<PageClassification> {
    let mut haystack = String::new();
    if let Some(title) = title {
        haystack.push_str(title);
        haystack.push('\n');
    }
    if let Some(text) = text {
        haystack.push_str(text);
        haystack.push('\n');
    }
    if let Some(html) = html {
        haystack.push_str(html);
    }
    classify_text(&haystack)
}

fn classify_text(raw: &str) -> Option<PageClassification> {
    let lower = raw.to_ascii_lowercase();
    let cloudflare_marker = lower.contains("cloudflare")
        || lower.contains("_cf_chl")
        || lower.contains("cf-chl")
        || lower.contains("challenges.cloudflare.com");
    let turnstile_marker =
        lower.contains("cf-turnstile") || (lower.contains("turnstile") && cloudflare_marker);
    let checking_browser = lower.contains("checking your browser")
        || lower.contains("just a moment")
        || lower.contains("verify you are human")
        || lower.contains("review the security of your connection");
    if turnstile_marker || (cloudflare_marker && checking_browser) {
        return Some(PageClassification {
            kind: PageKind::BotWallDetected,
            code: ErrorCode::BotWallDetected,
            detail: challenge_detail("bot wall/security challenge"),
        });
    }

    let access_denied = lower.contains("access denied")
        || lower.contains("request blocked")
        || lower.contains("you have been blocked")
        || lower.contains("unusual traffic");
    let challenge_marker = lower.contains("captcha")
        || lower.contains("challenge-platform")
        || lower.contains("security check")
        || lower.contains("verify that you are not a robot");
    if access_denied && (challenge_marker || lower.contains("reference #")) {
        return Some(PageClassification {
            kind: PageKind::SecurityChallengeDetected,
            code: ErrorCode::SecurityChallengeDetected,
            detail: challenge_detail("security challenge/access denied page"),
        });
    }

    None
}

fn challenge_detail(label: &str) -> String {
    format!(
        "detected {label}; artifacts likely show a challenge page, not verified target content. \
         Use human takeover for review/input, e.g. `afhttp ui --endpoint-url <host>`; \
         this is not a captcha bypass."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cloudflare_turnstile() {
        let html = r#"<!doctype html><title>Just a moment...</title>
          <div class="cf-turnstile" data-sitekey="x"></div>
          <script src="https://challenges.cloudflare.com/turnstile/v0/api.js"></script>"#;
        let class = classify(Some(html), None, None).expect("classify");
        assert_eq!(class.kind, PageKind::BotWallDetected);
        assert_eq!(class.code, ErrorCode::BotWallDetected);
    }

    #[test]
    fn detects_generic_access_denied_challenge() {
        let html = "Access Denied. Reference #18. Please complete the captcha security check.";
        let class = classify(Some(html), None, None).expect("classify");
        assert_eq!(class.kind, PageKind::SecurityChallengeDetected);
        assert_eq!(class.code, ErrorCode::SecurityChallengeDetected);
    }

    #[test]
    fn plain_access_denied_without_challenge_marker_is_not_enough() {
        assert!(classify(Some("Access denied to admin area"), None, None).is_none());
    }
}
