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

struct DocumentCopy {
    title: String,
    text: String,
}

/// Classify well-known security challenge / bot-wall HTML. The detector is
/// intentionally conservative: provider markers need page-level challenge
/// copy before labelling content unverifiable.
pub(crate) fn classify(
    html: Option<&str>,
    text: Option<&str>,
    title: Option<&str>,
) -> Option<PageClassification> {
    let html = html.unwrap_or_default().to_ascii_lowercase();
    // Provider code can belong to a form or chat widget. Only document copy
    // (never script bodies or attributes) is evidence that it blocks the page.
    let document = (text.is_none() || title.is_none()).then(|| html_copy(&html));
    let copy = text
        .unwrap_or_else(|| document.as_ref().map_or("", |document| &document.text))
        .to_ascii_lowercase();
    let title = title
        .unwrap_or_else(|| document.as_ref().map_or("", |document| &document.title))
        .to_ascii_lowercase();
    let cloudflare_marker = html.contains("cloudflare")
        || html.contains("_cf_chl")
        || html.contains("cf-chl")
        || html.contains("cf-turnstile");
    let challenge_phrases = [
        "checking your browser",
        "just a moment",
        "verify you are human",
        "verifying you are human",
        "review the security of your connection",
    ];
    let challenge_copy = |value: &str| {
        challenge_phrases
            .iter()
            .any(|phrase| value.contains(phrase))
    };
    let challenge_document = copy.chars().count() <= 1200
        && challenge_copy(&copy)
        && (challenge_phrases
            .iter()
            .any(|phrase| copy.trim_start().starts_with(phrase))
            || html.contains("_cf_chl")
            || html.contains("cf-chl"));
    // Some providers omit vendor markers entirely. Require the distinctive
    // short document introduction, rather than matching embedded widget copy.
    let browser_check_document = copy.chars().count() <= 1200
        && copy.trim_start().starts_with("hang tight")
        && copy.contains("we are checking your browser");
    // A document title or short challenge document supplies page-level
    // evidence. A verification prompt embedded in substantial target content
    // does not make the whole document a challenge.
    if browser_check_document
        || (cloudflare_marker && (challenge_copy(&title) || challenge_document))
    {
        return Some(PageClassification {
            kind: PageKind::BotWallDetected,
            code: ErrorCode::BotWallDetected,
            detail: challenge_detail("bot wall/security challenge"),
        });
    }

    let lower = format!("{title}\n{copy}");
    let access_denied = lower.contains("access denied")
        || lower.contains("request blocked")
        || lower.contains("you have been blocked")
        || lower.contains("unusual traffic");
    let challenge_marker = lower.contains("captcha")
        || html.contains("challenge-platform")
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

/// HTTP-path copy without script bodies, attributes, comments, or explicitly
/// hidden subtrees. Browser classification uses rendered innerText instead.
fn html_copy(html: &str) -> DocumentCopy {
    let document = scraper::Html::parse_document(html);
    let mut title = String::new();
    let mut copy = String::new();
    for node in document.tree.nodes() {
        let Some(text) = node.value().as_text() else {
            continue;
        };
        let mut in_title = false;
        let hidden = node.ancestors().any(|ancestor| {
            let Some(element) = ancestor.value().as_element() else {
                return false;
            };
            in_title |= element.name() == "title";
            matches!(element.name(), "script" | "style" | "template" | "noscript")
                || element.attr("hidden").is_some()
                || element.attr("aria-hidden") == Some("true")
        });
        if hidden {
            continue;
        }
        let target = if in_title { &mut title } else { &mut copy };
        target.push_str(text);
        target.push(' ');
    }
    let normalize = |value: String| value.split_whitespace().collect::<Vec<_>>().join(" ");
    DocumentCopy {
        title: normalize(title),
        text: normalize(copy),
    }
}

fn challenge_detail(label: &str) -> String {
    format!(
        "detected {label}; artifacts likely show a challenge page, not verified target content. \
         Use human takeover for review/input, e.g. `afhttp fetch <url> --takeover`; \
         this is not a captcha bypass."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_short_browser_check_without_vendor_markers() {
        let html =
            "<title>Hang tight!</title><h1>Hang tight!</h1><p>We are checking your browser</p>";
        assert_eq!(
            classify(Some(html), None, None).unwrap().kind,
            PageKind::BotWallDetected
        );
        assert!(
            classify(
                Some(
                    "<h1>Hosting prices</h1><aside>Hang tight! We are checking your browser</aside>"
                ),
                None,
                None
            )
            .is_none()
        );
    }

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
    fn embedded_turnstile_is_not_a_document_wall() {
        let html = r#"<title>Hosting plans</title><main><h1>Plans from €4/month</h1></main>
            <aside>Contact support: verify you are human <div class="cf-turnstile"></div></aside>
            <script src="https://challenges.cloudflare.com/turnstile/v0/api.js"></script>
            <script>const message = "just a moment: checking your browser";</script>"#;
        assert!(classify(Some(html), None, None).is_none());
        assert!(
            classify(
                Some(html),
                Some("Plans from €4/month Contact support: verify you are human"),
                Some("Hosting plans")
            )
            .is_none()
        );
    }

    #[test]
    fn provider_assets_and_script_copy_alone_are_not_a_wall() {
        let html = r#"<script src="https://challenges.cloudflare.com/turnstile/v0/api.js"></script>
            <script>const message = "just a moment: checking your browser";</script>
            <!-- verify you are human --><h1 data-message="verify you are human">Plans</h1>"#;
        assert!(classify(Some(html), None, None).is_none());
    }

    #[test]
    fn detects_challenge_copy_without_a_turnstile_widget() {
        let html =
            "<script src='/cdn-cgi/challenge-platform/scripts/jsd/main.js'></script>Cloudflare";
        let class = classify(
            Some(html),
            Some("Verifying you are human. This may take a few seconds."),
            Some("Security check"),
        )
        .expect("classify");
        assert_eq!(class.kind, PageKind::BotWallDetected);
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
