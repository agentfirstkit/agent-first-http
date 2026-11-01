//! AFDATA protocol adapter.
//!
//! Every `afhttp` command emits one AFDATA protocol-v1 event per invocation:
//! a single-line `{"kind":"result","result":...}` or `{"kind":"error",
//! "error":...}` JSON value followed by a newline.

use serde::Serialize;
use std::io::Write;
use std::sync::OnceLock;

use crate::shared::error::Error;

static OUTPUT_TO: OnceLock<agent_first_data::OutputTo> = OnceLock::new();

/// Install the process-wide AFDATA stream selector for this run.
///
/// Which selector that is comes from the resolved invocation's output plan —
/// the CLI registry has already decided which destinations the matched shape
/// admits and which one applies — so nothing here re-reads argv.
pub fn install_output_to(selector: agent_first_data::OutputTo) -> Result<(), Error> {
    OUTPUT_TO.set(selector).map_err(|_| {
        Error::new(
            crate::shared::error::ErrorCode::InternalError,
            "AFDATA output routing was initialized more than once",
        )
    })
}

#[must_use]
pub fn output_to() -> agent_first_data::OutputTo {
    OUTPUT_TO
        .get()
        .copied()
        .unwrap_or(agent_first_data::OutputTo::Split)
}

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

/// Emit a result through the process-wide `--output-to` route.
pub fn emit_process<T: Serialize>(code: &str, payload: &T) -> Result<(), Error> {
    emit_process_inner(code, payload, RedactionMode::Default)
}

/// Emit a result with redaction disabled through the process-wide route.
///
/// This is reserved for explicit reveal commands whose payload contains no
/// unrelated secrets.
pub fn emit_process_unredacted<T: Serialize>(code: &str, payload: &T) -> Result<(), Error> {
    emit_process_inner(code, payload, RedactionMode::None)
}

/// Emit a non-terminal progress event through the process-wide route.
///
/// A command that blocks on a person has something to say before it can say
/// how the run ended. Progress is the only event kind that may precede the
/// terminal one, and on a split route it lands on the diagnostic stream, so a
/// caller reading only `result` is unaffected.
pub fn emit_process_progress<T: Serialize>(code: &str, payload: &T) -> Result<(), Error> {
    emit_process_progress_value(prepare_payload(code, payload)?, RedactionMode::Default)
}

/// Emit an otherwise-redacted result while intentionally revealing only the
/// short-lived takeover capability field. The explicit `panel` and
/// `fetch --takeover` operations are the only callers.
pub fn emit_process_revealing_takeover<T: Serialize>(code: &str, payload: &T) -> Result<(), Error> {
    let value = prepare_revealed_takeover_payload(code, payload)?;
    emit_process_value(value, RedactionMode::None)
}

fn emit_process_progress_value(
    value: serde_json::Value,
    redaction: RedactionMode,
) -> Result<(), Error> {
    let event = agent_first_data::json_progress(value).build();
    let mut emitter = agent_first_data::CliEmitter::from_output_to_with(
        output_to(),
        agent_first_data::OutputFormat::Json,
        output_options(redaction),
    )
    .with_strict_protocol();
    emitter.emit(event).map_err(|error| {
        Error::new(
            crate::shared::error::ErrorCode::InternalError,
            error.to_string(),
        )
    })
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
    let value = prepare_payload(code, payload)?;

    let options = output_options(redaction);
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

fn emit_process_inner<T: Serialize>(
    code: &str,
    payload: &T,
    redaction: RedactionMode,
) -> Result<(), Error> {
    emit_process_value(prepare_payload(code, payload)?, redaction)
}

fn emit_process_value(value: serde_json::Value, redaction: RedactionMode) -> Result<(), Error> {
    let mut emitter = agent_first_data::CliEmitter::from_output_to_with(
        output_to(),
        agent_first_data::OutputFormat::Json,
        output_options(redaction),
    )
    .with_strict_protocol();
    emitter.emit_result(value).map_err(|err| {
        Error::new(
            crate::shared::error::ErrorCode::InternalError,
            err.to_string(),
        )
    })
}

fn output_options(redaction: RedactionMode) -> agent_first_data::OutputOptions {
    match redaction {
        RedactionMode::Default => agent_first_data::OutputOptions {
            redaction: agent_first_data::Redactor::new(),
            style: agent_first_data::PlainStyle::Raw,
        },
        RedactionMode::None => agent_first_data::OutputOptions {
            redaction: agent_first_data::Redactor::new()
                .policy(agent_first_data::RedactionPolicy::Off),
            style: agent_first_data::PlainStyle::Raw,
        },
    }
}

fn prepare_payload<T: Serialize>(code: &str, payload: &T) -> Result<serde_json::Value, Error> {
    let value = serde_json::to_value(payload).map_err(|e| {
        Error::new(
            crate::shared::error::ErrorCode::InternalError,
            format!("AFDATA: failed to serialize payload: {e}"),
        )
    })?;
    wrap_payload(code, value).map(|value| crate::shared::redact::redact_url_fields(&value))
}

fn prepare_revealed_takeover_payload<T: Serialize>(
    code: &str,
    payload: &T,
) -> Result<serde_json::Value, Error> {
    Ok(agent_first_data::Redactor::new()
        .reveal_names(["takeover_url_secret"])
        .value(&prepare_payload(code, payload)?))
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

/// What a host answers to a request whose bearer token it does not accept.
pub const BEARER_REJECTED: &str = "missing or invalid bearer token";

static TOKEN_FROM_ENV: OnceLock<&'static str> = OnceLock::new();

/// Record that this run's token came from an environment fallback rather than
/// from argv, so a rejection can say which variable to look at: a stale value
/// left in the environment is otherwise indistinguishable from a wrong flag.
pub fn note_token_from_env(name: &'static str) {
    let _ = TOKEN_FROM_ENV.set(name);
}

fn explain_rejection<'a>(
    message: &'a str,
    token_from_env: Option<&str>,
) -> std::borrow::Cow<'a, str> {
    match token_from_env {
        Some(name) if message.ends_with(BEARER_REJECTED) => format!(
            "{message} (the token came from {name}, which takes precedence over a \
             discovered host's own token; unset it or pass --token-secret)"
        )
        .into(),
        _ => message.into(),
    }
}

// Every error emitter shares this boundary, including errors built from a raw
// message rather than an SDK `Error`.
pub(crate) fn error_builder(code: &str, message: &str) -> agent_first_data::ErrorBuilder {
    let message = explain_rejection(message, TOKEN_FROM_ENV.get().copied());
    agent_first_data::json_error(code, &crate::shared::redact::redact_urls_in_text(&message))
}

/// Convenience: emit an SDK-built AFDATA error event.
pub fn emit_error<W: Write>(writer: &mut W, err: &Error) -> Result<(), Error> {
    let mut emitter = agent_first_data::CliEmitter::with_options(
        writer,
        agent_first_data::OutputFormat::Json,
        output_options(RedactionMode::Default),
    )
    .with_strict_protocol();
    let event = error_builder(err.error_code.as_str(), &err.detail)
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

/// Emit an SDK-built AFDATA error event through `--output-to`.
pub fn emit_process_error(err: &Error) -> Result<(), Error> {
    let event = error_builder(err.error_code.as_str(), &err.detail)
        .retryable_if(err.retryable)
        .build()
        .map_err(|error| {
            Error::new(
                crate::shared::error::ErrorCode::InternalError,
                error.to_string(),
            )
        })?;
    let mut emitter = agent_first_data::CliEmitter::from_output_to_with(
        output_to(),
        agent_first_data::OutputFormat::Json,
        output_options(RedactionMode::Default),
    )
    .with_strict_protocol();
    emitter.emit(event).map_err(|error| {
        Error::new(
            crate::shared::error::ErrorCode::InternalError,
            error.to_string(),
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
    let mut emitter = agent_first_data::CliEmitter::with_options(
        writer,
        agent_first_data::OutputFormat::Json,
        output_options(RedactionMode::Default),
    )
    .with_strict_protocol();
    let retryable = fields
        .get("retryable")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let fields = crate::shared::redact::redact_url_fields(&fields);
    let trace = crate::shared::redact::redact_url_fields(&trace);
    let fields = match fields {
        serde_json::Value::Object(mut fields) => {
            fields.remove("retryable");
            serde_json::Value::Object(fields)
        }
        other => other,
    };
    let event = error_builder(code, message)
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

/// Emit an error with extension fields and trace through `--output-to`.
pub fn emit_process_error_with(
    code: &str,
    message: &str,
    fields: serde_json::Value,
    trace: serde_json::Value,
) -> Result<(), Error> {
    let retryable = fields
        .get("retryable")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let fields = crate::shared::redact::redact_url_fields(&fields);
    let trace = crate::shared::redact::redact_url_fields(&trace);
    let fields = match fields {
        serde_json::Value::Object(mut fields) => {
            fields.remove("retryable");
            serde_json::Value::Object(fields)
        }
        other => other,
    };
    let event = error_builder(code, message)
        .retryable_if(retryable)
        .fields(fields)
        .trace(trace)
        .build()
        .map_err(|error| {
            Error::new(
                crate::shared::error::ErrorCode::InternalError,
                error.to_string(),
            )
        })?;
    let mut emitter = agent_first_data::CliEmitter::from_output_to_with(
        output_to(),
        agent_first_data::OutputFormat::Json,
        output_options(RedactionMode::Default),
    )
    .with_strict_protocol();
    emitter.emit(event).map_err(|error| {
        Error::new(
            crate::shared::error::ErrorCode::InternalError,
            error.to_string(),
        )
    })
}

/// Build a strict AFDATA error event for HTTP response bodies.
pub fn error_value(code: &str, message: &str, retryable: bool) -> serde_json::Value {
    let value: serde_json::Value = error_builder(code, message)
        .retryable_if(retryable)
        .build()
        .map(Into::into)
        .unwrap_or_else(|_| serde_json::json!({}));
    crate::shared::redact::redact_value(&value)
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
    let value: serde_json::Value = agent_first_data::json_result(payload).build().into();
    crate::shared::redact::redact_value(&value)
}

/// Build a strict HTTP result while intentionally revealing only
/// `takeover_url_secret`. Used by the authenticated handoff-minting endpoint.
pub fn result_value_revealing_takeover(
    code: &str,
    mut payload: serde_json::Value,
) -> serde_json::Value {
    let payload = match &mut payload {
        serde_json::Value::Object(fields) => {
            fields
                .entry("code".to_string())
                .or_insert_with(|| serde_json::Value::String(code.to_string()));
            payload
        }
        _ => serde_json::json!({"code": code, "value": payload}),
    };
    let original: serde_json::Value = agent_first_data::json_result(payload).build().into();
    let original = crate::shared::redact::redact_url_fields(&original);
    agent_first_data::Redactor::new()
        .reveal_names(["takeover_url_secret"])
        .value(&original)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rejected_environment_token_names_its_variable() {
        let wrapped = format!("GET http://127.0.0.1:1/health: {BEARER_REJECTED}");
        let explained = explain_rejection(&wrapped, Some("AFHTTP_TOKEN_SECRET"));
        assert!(explained.starts_with(&wrapped), "{explained}");
        assert!(
            explained.contains("came from AFHTTP_TOKEN_SECRET"),
            "{explained}"
        );
        // A token from argv, or any other error, is reported as it was.
        assert_eq!(explain_rejection(&wrapped, None), wrapped);
        assert_eq!(
            explain_rejection("host unreachable", Some("AFHTTP_TOKEN_SECRET")),
            "host unreachable"
        );
    }

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
    fn every_error_emitter_redacts_url_credentials_in_raw_messages() {
        let message = "request https://reader:password-canary@example.test/path?token_secret=query-canary&token=legacy-canary&safe=ok failed";
        let expected = "request https://reader:***@example.test/path?token_secret=***&token=***&safe=ok failed";
        // Bypass `Error::new` to verify that the emitters protect their own
        // boundary rather than relying on an already-redacted SDK error.
        let err = Error {
            error_code: crate::shared::error::ErrorCode::HostUnreachable,
            detail: message.to_string(),
            retryable: true,
        };
        let mut sdk_output = Vec::new();
        emit_error(&mut sdk_output, &err).expect("emit SDK error");
        let mut extended_output = Vec::new();
        emit_error_with(
            &mut extended_output,
            "host_unreachable",
            message,
            serde_json::json!({"retryable": true, "stage": "navigate"}),
            serde_json::json!({"duration_ms": 10}),
        )
        .expect("emit extended error");
        let sdk: serde_json::Value = serde_json::from_slice(&sdk_output).expect("SDK event");
        let extended: serde_json::Value =
            serde_json::from_slice(&extended_output).expect("extended event");
        let http = error_value("host_unreachable", message, true);
        // The process emitters and CLI startup path use the same builder.
        let process: serde_json::Value = error_builder("host_unreachable", message)
            .retryable_if(true)
            .build()
            .expect("process event")
            .into();
        for value in [sdk, extended, http, process] {
            agent_first_data::validate_protocol_event(&value, true).expect("strict event");
            assert_eq!(value["error"]["message"], expected);
            assert_eq!(value["error"]["code"], "host_unreachable");
            assert_eq!(value["error"]["retryable"], true);
            let text = serde_json::to_string(&value).expect("event text");
            assert!(!text.contains("password-canary"));
            assert!(!text.contains("query-canary"));
            assert!(!text.contains("legacy-canary"));
        }
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
    fn http_result_redacts_common_url_query_credentials() {
        let value = result_value(
            "fetch",
            serde_json::json!({
                "request_url": "https://user:pass@example.test/?token=abc&safe=ok"
            }),
        );
        assert_eq!(
            value["result"]["request_url"],
            "https://user:***@example.test/?token=***&safe=ok"
        );
    }

    #[test]
    fn takeover_result_reveals_only_the_explicit_capability() {
        let value = result_value_revealing_takeover(
            "takeover_handoff",
            serde_json::json!({
                "takeover_url_secret":
                    "https://example.test/takeover?handoff_secret=short-lived",
                "host_token_secret": "long-lived",
                "request_url": "https://example.test/?token=also-secret"
            }),
        );
        assert_eq!(
            value["result"]["takeover_url_secret"],
            "https://example.test/takeover?handoff_secret=short-lived"
        );
        assert_eq!(value["result"]["host_token_secret"], "***");
        assert_eq!(
            value["result"]["request_url"],
            "https://example.test/?token=***"
        );
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
