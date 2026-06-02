//! Duration parsing for `--timeout 30s`, `--older-than 30d`, etc.
//!
//! Delegates to `humantime` for the wire format and converts to
//! `std::time::Duration`. Keeps a single error type (`InvalidArgument`) so
//! every flag that takes a duration produces the same shape on bad input.

use crate::shared::error::{Error, ErrorCode};

/// Parse a human-readable duration (`30s`, `250ms`, `1m`, `30d`) into a
/// `std::time::Duration`. Empty or unparseable input returns
/// [`ErrorCode::InvalidArgument`].
pub fn parse_duration(input: &str) -> Result<std::time::Duration, Error> {
    if input.is_empty() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "duration: empty input",
        ));
    }
    humantime::parse_duration(input)
        .map_err(|e| Error::new(ErrorCode::InvalidArgument, format!("duration: {e}")))
}

/// Convert a `Duration` to milliseconds, saturating on overflow. Used by
/// `*_ms` fields in the protocol output.
#[must_use]
pub fn duration_ms(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parses_common_units() {
        assert_eq!(parse_duration("30s").map(|d| d.as_secs()).ok(), Some(30));
        assert_eq!(
            parse_duration("250ms").map(|d| d.as_millis()).ok(),
            Some(250)
        );
        assert_eq!(parse_duration("1m").map(|d| d.as_secs()).ok(), Some(60));
        assert_eq!(
            parse_duration("2h").map(|d| d.as_secs()).ok(),
            Some(2 * 3600)
        );
        assert_eq!(
            parse_duration("30d").map(|d| d.as_secs()).ok(),
            Some(30 * 86400),
        );
    }

    #[test]
    fn empty_input_is_invalid_argument() {
        let err = parse_duration("").err();
        assert!(err.is_some());
        if let Some(e) = err {
            assert_eq!(e.error_code, ErrorCode::InvalidArgument);
        }
    }

    #[test]
    fn garbage_input_is_invalid_argument() {
        let err = parse_duration("nonsense").err();
        assert!(err.is_some());
        if let Some(e) = err {
            assert_eq!(e.error_code, ErrorCode::InvalidArgument);
        }
    }

    #[test]
    fn duration_ms_saturates() {
        assert_eq!(duration_ms(Duration::from_millis(1234)), 1234);
        assert_eq!(duration_ms(Duration::ZERO), 0);
    }
}
