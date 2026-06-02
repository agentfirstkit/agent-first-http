//! Wait condition for browser-backed fetches (`architecture.md §5`).

use crate::shared::error::{Error, ErrorCode};

/// When to consider a page ready.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Wait {
    /// CDP `Page.loadEventFired`. Default.
    #[default]
    Load,
    /// CDP `Network.idle` (no requests for ~500 ms).
    Idle,
    /// A CSS selector matches `document.querySelector(...)`. Existence-only,
    /// not visibility — a node hidden by CSS or with zero dimensions still
    /// satisfies the wait. Use [`Wait::SelectorVisible`] if you need the
    /// node to actually paint.
    Selector(String),
    /// A CSS selector matches `document.querySelector(...)` AND the matched
    /// node has a non-zero bounding box, `display != "none"` on every
    /// ancestor (`offsetParent != null` on non-fixed elements), and
    /// `visibility != "hidden"`. Catches the common framework pattern of
    /// rendering the node into the DOM before the layout has painted it.
    SelectorVisible(String),
    /// A fixed wall-clock delay after navigation start.
    Ms(u64),
}

impl Wait {
    pub fn parse(s: &str) -> Result<Self, Error> {
        if s == "load" {
            Ok(Self::Load)
        } else if s == "idle" {
            Ok(Self::Idle)
        } else if let Some(sel) = s.strip_prefix("selector-visible:") {
            if sel.is_empty() {
                Err(Error::new(
                    ErrorCode::InvalidArgument,
                    "--wait selector-visible: requires a non-empty CSS selector",
                ))
            } else {
                Ok(Self::SelectorVisible(sel.to_string()))
            }
        } else if let Some(sel) = s.strip_prefix("selector:") {
            if sel.is_empty() {
                Err(Error::new(
                    ErrorCode::InvalidArgument,
                    "--wait selector: requires a non-empty CSS selector",
                ))
            } else {
                Ok(Self::Selector(sel.to_string()))
            }
        } else if let Some(ms) = s.strip_prefix("ms:") {
            let n: u64 = ms.parse().map_err(|_| {
                Error::new(
                    ErrorCode::InvalidArgument,
                    format!("--wait ms: not a u64: {ms:?}"),
                )
            })?;
            Ok(Self::Ms(n))
        } else {
            Err(Error::new(
                ErrorCode::InvalidArgument,
                format!(
                    "--wait: unknown mode {s:?}; expected \
                     load|idle|selector:<css>|selector-visible:<css>|ms:<n>"
                ),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_modes() {
        assert_eq!(Wait::parse("load").unwrap(), Wait::Load);
        assert_eq!(Wait::parse("idle").unwrap(), Wait::Idle);
        assert_eq!(
            Wait::parse("selector:#root").unwrap(),
            Wait::Selector("#root".into())
        );
        assert_eq!(
            Wait::parse("selector-visible:#root").unwrap(),
            Wait::SelectorVisible("#root".into())
        );
        assert_eq!(Wait::parse("ms:250").unwrap(), Wait::Ms(250));
    }

    #[test]
    fn rejects_empty_selector() {
        let err = Wait::parse("selector:").err();
        assert_eq!(err.map(|e| e.error_code), Some(ErrorCode::InvalidArgument));
        let err = Wait::parse("selector-visible:").err();
        assert_eq!(err.map(|e| e.error_code), Some(ErrorCode::InvalidArgument));
    }

    #[test]
    fn selector_visible_prefix_does_not_collide_with_selector() {
        // The prefix check order matters: selector-visible: must be
        // tested before selector: so we don't strip "selector:" and
        // end up with "visible:#root" as the selector body.
        let parsed = Wait::parse("selector-visible:.btn").unwrap();
        assert_eq!(parsed, Wait::SelectorVisible(".btn".into()));
    }

    #[test]
    fn rejects_bad_ms() {
        let err = Wait::parse("ms:nope").err();
        assert_eq!(err.map(|e| e.error_code), Some(ErrorCode::InvalidArgument));
    }

    #[test]
    fn rejects_unknown_mode() {
        let err = Wait::parse("forever").err();
        assert_eq!(err.map(|e| e.error_code), Some(ErrorCode::InvalidArgument));
    }
}
