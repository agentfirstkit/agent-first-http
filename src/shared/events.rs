//! Reader for the protocol-v1 events an `afhttp host` answers with.
//!
//! The host writes `{"kind":"result","result":...}` or `{"kind":"error",
//! "error":{"code","message","retryable"?}}`. The SDK only reads those two
//! shapes, so plain serde covers it and the SDK floor links no AFDATA.

use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::shared::error::{Error, ErrorCode};

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Event {
    Result { result: serde_json::Value },
    Error { error: ErrorBody },
}

#[derive(Deserialize)]
struct ErrorBody {
    code: String,
    message: String,
    #[serde(default)]
    retryable: Option<bool>,
}

fn decode(bytes: &[u8]) -> Result<Event, Error> {
    serde_json::from_slice(bytes).map_err(|error| {
        Error::new(
            ErrorCode::InternalError,
            format!("invalid AFDATA event: {error}"),
        )
    })
}

pub fn decode_result<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    match decode(bytes)? {
        Event::Result { result } => serde_json::from_value(result).map_err(|error| {
            Error::new(
                ErrorCode::InternalError,
                format!("decode AFDATA result payload: {error}"),
            )
        }),
        Event::Error { .. } => Err(Error::new(
            ErrorCode::InternalError,
            "expected AFDATA result event",
        )),
    }
}

pub fn decode_error(bytes: &[u8]) -> Result<Error, Error> {
    match decode(bytes)? {
        Event::Error { error } => {
            let code = serde_json::from_value(serde_json::Value::String(error.code))
                .unwrap_or(ErrorCode::InternalError);
            let decoded = Error::new(code, error.message);
            Ok(match error.retryable {
                Some(retryable) => decoded.with_retryable(retryable),
                None => decoded,
            })
        }
        Event::Result { .. } => Err(Error::new(
            ErrorCode::InternalError,
            "expected AFDATA error event",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_payload_decodes_into_its_type() {
        #[derive(Deserialize)]
        struct Health {
            status: String,
        }
        let health: Health =
            decode_result(br#"{"kind":"result","result":{"code":"health","status":"ok"}}"#)
                .unwrap();
        assert_eq!(health.status, "ok");
    }

    #[test]
    fn error_event_keeps_code_message_and_retryable() {
        let error = decode_error(
            br#"{"kind":"error","error":{"code":"profile_locked","message":"busy","retryable":true}}"#,
        )
        .unwrap();
        assert_eq!(error.error_code, ErrorCode::ProfileLocked);
        assert_eq!(error.detail, "busy");
        assert!(error.retryable);
    }

    #[test]
    fn unknown_error_code_maps_to_internal_error() {
        let error =
            decode_error(br#"{"kind":"error","error":{"code":"from_the_future","message":"x"}}"#)
                .unwrap();
        assert_eq!(error.error_code, ErrorCode::InternalError);
    }

    #[test]
    fn wrong_kind_is_rejected_in_both_directions() {
        assert!(decode_error(br#"{"kind":"result","result":{}}"#).is_err());
        assert!(
            decode_result::<serde_json::Value>(
                br#"{"kind":"error","error":{"code":"io_error","message":"x"}}"#
            )
            .is_err()
        );
        assert!(decode_result::<serde_json::Value>(b"not json").is_err());
    }
}
