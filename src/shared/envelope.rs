//! Protocol envelope writer.
//!
//! Every `afhttp` command emits one structured value per invocation: a
//! single line of AFDATA JSON, wrapped with a top-level `code` field
//! (`"fetch"`, `"health"`, `"error"`, etc.) and a trailing newline. JSON is
//! the only output format — one request in, one line of structured JSON out.

use serde::Serialize;
use std::io::Write;

use crate::shared::error::Error;

/// Emit a single envelope payload to `writer`. The payload is wrapped with
/// a top-level `code` field and written as one line of JSON followed by a
/// newline.
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
            format!("envelope: failed to serialize payload: {e}"),
        )
    })?;
    value = wrap_payload(code, value)?;

    let line = match redaction {
        RedactionMode::Default => agent_first_data::output_json(&value),
        RedactionMode::None => agent_first_data::output_json_with(
            &value,
            agent_first_data::RedactionPolicy::RedactionNone,
        ),
    };
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn wrap_payload(code: &str, value: serde_json::Value) -> Result<serde_json::Value, Error> {
    let serde_json::Value::Object(mut map) = value else {
        return Err(Error::new(
            crate::shared::error::ErrorCode::InternalError,
            "envelope: payload must serialize to a JSON object",
        ));
    };

    match map.get("code") {
        Some(serde_json::Value::String(existing)) if existing == code => {}
        Some(_) => {
            return Ok(serde_json::json!({
                "code": code,
                "result": serde_json::Value::Object(map),
            }));
        }
        None => {
            map.insert("code".into(), serde_json::Value::String(code.to_string()));
        }
    }
    Ok(serde_json::Value::Object(map))
}

/// Convenience: emit an `Error` value with `code: "error"`.
pub fn emit_error<W: Write>(writer: &mut W, err: &Error) -> Result<(), Error> {
    emit(writer, "error", err)
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
    fn json_envelope_is_single_line_with_code_field() {
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
        assert_eq!(parsed["code"], "health");
        assert_eq!(parsed["status"], "ok");
        assert_eq!(parsed["uptime_s"], 42);
        assert_eq!(trimmed.lines().count(), 1);
    }

    #[test]
    fn error_envelope_uses_error_code_tag() {
        let mut buf = Vec::new();
        let err = Error::new(
            crate::shared::error::ErrorCode::NavigationTimeout,
            "no load",
        );
        emit_error(&mut buf, &err).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_slice(&buf).unwrap_or(serde_json::Value::Null);
        assert_eq!(parsed["code"], "error");
        assert_eq!(parsed["error_code"], "navigation_timeout");
        assert_eq!(parsed["error"], "no load");
        assert_eq!(parsed["retryable"], true);
    }

    #[derive(Serialize)]
    struct SecretPayload {
        token_secret: &'static str,
    }

    #[test]
    fn envelope_redacts_afdata_secret_fields() {
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
        assert_eq!(parsed["token_secret"], "***");
    }

    #[test]
    fn envelope_wraps_payload_when_code_would_collide() {
        let mut buf = Vec::new();
        let payload = serde_json::json!({"code": -32000, "message": "cdp error"});
        emit(&mut buf, "cdp", &payload).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(parsed["code"], "cdp");
        assert_eq!(parsed["result"]["code"], -32000);
    }
}
