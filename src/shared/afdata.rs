//! AFDATA protocol adapter.
//!
//! Every `afhttp` command emits one AFDATA protocol-v1 event per invocation:
//! a single-line `{"kind":"result","result":...}` or `{"kind":"error",
//! "error":...}` JSON value followed by a newline.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::Write;

use crate::shared::error::Error;

/// Emit a typed result event to `writer` and write it as one line of JSON followed
/// by a newline. The command-specific `code` remains inside `result`.
///
/// Redacts AFDATA `_secret` fields by default and never panics on well-formed
/// input — but we still funnel through this single seam so `print_stdout` /
/// `print_stderr` stay clippy-denied at crate level.
pub fn emit<W: Write, T: Serialize>(writer: &mut W, code: &str, payload: &T) -> Result<(), Error> {
    emit_inner(writer, code, payload, RedactionMode::Default)
}

/// Emit a payload without AFDATA redaction. Use only for commands that require
/// an explicit reveal flag and whose payload contains no unrelated secrets.
pub fn emit_unredacted<W: Write, T: Serialize>(
    writer: &mut W,
    code: &str,
    payload: &T,
) -> Result<(), Error> {
    emit_inner(writer, code, payload, RedactionMode::None)
}

#[derive(Clone, Copy)]
enum RedactionMode {
    Default,
    None,
}

fn emit_inner<W: Write, T: Serialize>(
    writer: &mut W,
    code: &str,
    payload: &T,
    redaction: RedactionMode,
) -> Result<(), Error> {
    let mut value = serde_json::to_value(payload).map_err(|e| {
        Error::new(
            crate::shared::error::ErrorCode::InternalError,
            format!("AFDATA: failed to serialize payload: {e}"),
        )
    })?;
    value = wrap_payload(code, value)?;

    let options = match redaction {
        RedactionMode::Default => agent_first_data::OutputOptions::default(),
        RedactionMode::None => agent_first_data::OutputOptions {
            redaction: agent_first_data::Redactor::new()
                .policy(agent_first_data::RedactionPolicy::RedactionNone),
            style: agent_first_data::OutputStyle::Raw,
        },
    };
    let mut emitter = agent_first_data::CliEmitter::with_options(
        writer,
        agent_first_data::OutputFormat::Json,
        options,
    )
    .with_strict_protocol();
    emitter.emit_result(value).map_err(|err| {
        Error::new(
            crate::shared::error::ErrorCode::InternalError,
            err.to_string(),
        )
    })?;
    Ok(())
}

fn wrap_payload(code: &str, value: serde_json::Value) -> Result<serde_json::Value, Error> {
    let serde_json::Value::Object(mut map) = value else {
        return Err(Error::new(
            crate::shared::error::ErrorCode::InternalError,
            "AFDATA result payload must serialize to a JSON object",
        ));
    };

    map.insert("code".into(), serde_json::Value::String(code.to_string()));
    Ok(serde_json::Value::Object(map))
}

/// Convenience: emit an SDK-built AFDATA error event.
pub fn emit_error<W: Write>(writer: &mut W, err: &Error) -> Result<(), Error> {
    let mut emitter =
        agent_first_data::CliEmitter::new(writer, agent_first_data::OutputFormat::Json)
            .with_strict_protocol();
    let event = agent_first_data::json_error(err.error_code.as_str(), &err.detail)
        .retryable_if(err.retryable)
        .build()
        .map_err(|err| {
            Error::new(
                crate::shared::error::ErrorCode::InternalError,
                err.to_string(),
            )
        })?;
    emitter.emit(event).map_err(|emit_err| {
        Error::new(
            crate::shared::error::ErrorCode::InternalError,
            emit_err.to_string(),
        )
    })
}

/// Emit an error event with caller-owned extension fields and trace.
pub fn emit_error_with<W: Write>(
    writer: &mut W,
    code: &str,
    message: &str,
    fields: serde_json::Value,
    trace: serde_json::Value,
) -> Result<(), Error> {
    let mut emitter =
        agent_first_data::CliEmitter::new(writer, agent_first_data::OutputFormat::Json)
            .with_strict_protocol();
    let retryable = fields
        .get("retryable")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let fields = match fields {
        serde_json::Value::Object(mut fields) => {
            fields.remove("retryable");
            serde_json::Value::Object(fields)
        }
        other => other,
    };
    let event = agent_first_data::json_error(code, message)
        .retryable_if(retryable)
        .fields(fields)
        .trace(trace)
        .build()
        .map_err(|err| {
            Error::new(
                crate::shared::error::ErrorCode::InternalError,
                err.to_string(),
            )
        })?;
    emitter.emit(event).map_err(|err| {
        Error::new(
            crate::shared::error::ErrorCode::InternalError,
            err.to_string(),
        )
    })
}

/// Build a strict AFDATA error event for HTTP response bodies.
pub fn error_value(code: &str, message: &str, retryable: bool) -> serde_json::Value {
    agent_first_data::json_error(code, message)
        .retryable_if(retryable)
        .build()
        .map(Into::into)
        .unwrap_or_else(|_| serde_json::json!({}))
}

/// Build a strict AFDATA result event for HTTP response bodies.
pub fn result_value(code: &str, mut payload: serde_json::Value) -> serde_json::Value {
    let payload = match &mut payload {
        serde_json::Value::Object(fields) => {
            fields
                .entry("code".to_string())
                .or_insert_with(|| serde_json::Value::String(code.to_string()));
            payload
        }
        _ => serde_json::json!({"code": code, "value": payload}),
    };
    agent_first_data::json_result(payload)
        .build()
        .map(Into::into)
        .unwrap_or_else(|_| serde_json::json!({}))
}

pub fn decode_result<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        Error::new(
            crate::shared::error::ErrorCode::InternalError,
            format!("decode AFDATA event: {error}"),
        )
    })?;
    match agent_first_data::decode_protocol_event(text) {
        Ok(agent_first_data::DecodedEvent::Result(result)) => serde_json::from_value(result.result)
            .map_err(|error| {
                Error::new(
                    crate::shared::error::ErrorCode::InternalError,
                    format!("decode AFDATA result payload: {error}"),
                )
            }),
        Ok(_) => Err(Error::new(
            crate::shared::error::ErrorCode::InternalError,
            "expected AFDATA result event",
        )),
        Err(error) => Err(Error::new(
            crate::shared::error::ErrorCode::InternalError,
            format!("invalid AFDATA event: {error}"),
        )),
    }
}

pub fn decode_error(bytes: &[u8]) -> Result<Error, Error> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        Error::new(
            crate::shared::error::ErrorCode::InternalError,
            format!("decode AFDATA error event: {error}"),
        )
    })?;
    match agent_first_data::decode_protocol_event(text) {
        Ok(agent_first_data::DecodedEvent::Error(error)) => {
            let code = serde_json::from_value(serde_json::Value::String(error.code))
                .unwrap_or(crate::shared::error::ErrorCode::InternalError);
            Ok(Error::new(code, error.message).with_retryable(error.retryable))
        }
        Ok(_) => Err(Error::new(
            crate::shared::error::ErrorCode::InternalError,
            "expected AFDATA error event",
        )),
        Err(error) => Err(Error::new(
            crate::shared::error::ErrorCode::InternalError,
            format!("invalid AFDATA error event: {error}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct HealthPayload {
        status: &'static str,
        uptime_s: u64,
    }

    #[test]
    fn json_result_event_is_single_line_with_code_field() {
        let mut buf = Vec::new();
        let payload = HealthPayload {
            status: "ok",
            uptime_s: 42,
        };
        emit(&mut buf, "health", &payload).unwrap();
        let s = String::from_utf8(buf).unwrap_or_default();
        assert!(s.ends_with('\n'));
        let trimmed = s.trim_end();
        let parsed: serde_json::Value = serde_json::from_str(trimmed).unwrap();
        assert_eq!(parsed["kind"], "result");
        assert_eq!(parsed["result"]["code"], "health");
        assert_eq!(parsed["result"]["status"], "ok");
        assert_eq!(parsed["result"]["uptime_s"], 42);
        assert_eq!(trimmed.lines().count(), 1);
    }

    #[test]
    fn error_event_uses_error_code_tag() {
        let mut buf = Vec::new();
        let err = Error::new(
            crate::shared::error::ErrorCode::NavigationTimeout,
            "no load",
        );
        emit_error(&mut buf, &err).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_slice(&buf).unwrap_or(serde_json::Value::Null);
        assert_eq!(parsed["kind"], "error");
        assert_eq!(parsed["error"]["code"], "navigation_timeout");
        assert_eq!(parsed["error"]["message"], "no load");
        assert_eq!(parsed["error"]["retryable"], true);
    }

    #[test]
    fn error_extension_fields_are_flattened_into_error_payload() {
        let mut buf = Vec::new();
        emit_error_with(
            &mut buf,
            "navigation_timeout",
            "no load",
            serde_json::json!({
                "retryable": true,
                "stage": "capture_text",
                "details": "scalar detail remains an explicitly named field"
            }),
            serde_json::json!({"duration_ms": 10}),
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(parsed["error"]["stage"], "capture_text");
        assert_eq!(parsed["error"]["retryable"], true);
        assert_eq!(
            parsed["error"]["details"],
            "scalar detail remains an explicitly named field"
        );
        assert!(parsed["error"].get("fields").is_none());
    }

    #[derive(Serialize)]
    struct SecretPayload {
        token_secret: &'static str,
    }

    #[test]
    fn afdata_event_redacts_secret_fields() {
        let mut buf = Vec::new();
        emit(
            &mut buf,
            "container_status",
            &SecretPayload {
                token_secret: "supersecret",
            },
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(parsed["result"]["token_secret"], "***");
    }

    #[test]
    fn envelope_uses_sdk_result_payload_without_nested_envelope() {
        let mut buf = Vec::new();
        let payload = serde_json::json!({"code": -32000, "message": "cdp error"});
        emit(&mut buf, "cdp", &payload).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(parsed["kind"], "result");
        assert_eq!(parsed["result"]["code"], "cdp");
        assert_eq!(parsed["result"]["code"], "cdp");
        assert_eq!(parsed["result"]["message"], "cdp error");
        assert!(parsed["result"].get("result").is_none());
    }
}
