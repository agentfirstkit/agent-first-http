use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Input types (stdin)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(tag = "code")]
pub enum Input {
    #[serde(rename = "request")]
    Request {
        id: String,
        #[serde(default)]
        tag: Option<String>,
        method: String,
        url: String,
        #[serde(default)]
        headers: HashMap<String, Value>,
        body: Option<Value>,
        body_base64: Option<String>,
        body_file: Option<String>,
        body_multipart: Option<Vec<MultipartPart>>,
        body_urlencoded: Option<Vec<UrlencodedPart>>,
        #[serde(default)]
        options: RequestOptions,
    },
    #[serde(rename = "config")]
    Config(ConfigPatch),
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "send")]
    Send {
        id: String,
        data: Option<Value>,
        data_base64: Option<String>,
    },
    #[serde(rename = "cancel")]
    Cancel { id: String },
    #[serde(rename = "close")]
    Close,
}

/// All fields from a `{"code":"config",...}` command.
/// Passed directly to RuntimeConfig::apply_update.
#[derive(Deserialize, Default)]
pub struct ConfigPatch {
    pub response_save_dir: Option<String>,
    pub response_save_above_bytes: Option<u64>,
    pub request_concurrency_limit: Option<u64>,
    pub timeout_connect_s: Option<u64>,
    pub pool_idle_timeout_s: Option<u64>,
    pub retry_base_delay_ms: Option<u64>,
    pub proxy: Option<String>,
    pub tls: Option<TlsConfigPartial>,
    pub log: Option<Vec<String>>,
    pub defaults: Option<RequestDefaultsPartial>,
    pub host_defaults: Option<HashMap<String, HostDefaultsPartial>>,
}

pub enum WsCommand {
    Send {
        data: Option<Value>,
        data_base64: Option<String>,
    },
    Close,
}

#[derive(Deserialize, Default)]
pub struct RequestOptions {
    pub timeout_idle_s: Option<u64>,
    pub retry: Option<u32>,
    pub response_redirect: Option<u32>,
    pub response_parse_json: Option<bool>,
    pub response_decompress: Option<bool>,
    pub response_save_resume: Option<bool>,
    #[serde(default)]
    pub chunked: bool,
    #[serde(default = "default_chunked_delimiter")]
    pub chunked_delimiter: Value, // String = delimiter, Null = raw, absent = "\n"
    pub response_save_file: Option<String>,
    pub progress_bytes: Option<u64>,
    pub progress_ms: Option<u64>,
    pub retry_on_status: Option<Vec<u16>>,
    pub response_max_bytes: Option<u64>,
    pub upgrade: Option<String>,
    /// Per-request TLS overrides — merged on top of global TLS config.
    /// Builds a one-off HTTP client for this request (no shared connection pool).
    pub tls: Option<TlsConfigPartial>,
}

#[derive(Deserialize)]
pub struct MultipartPart {
    pub name: String,
    pub value: Option<String>,
    pub value_base64: Option<String>,
    pub file: Option<String>,
    pub filename: Option<String>,
    pub content_type: Option<String>,
}

#[derive(Deserialize)]
pub struct UrlencodedPart {
    pub name: String,
    pub value: String,
}

#[derive(Deserialize, Default)]
pub struct RequestDefaultsPartial {
    pub headers: Option<HashMap<String, Value>>,
    pub timeout_idle_s: Option<u64>,
    pub retry: Option<u32>,
    pub response_redirect: Option<u32>,
    pub response_parse_json: Option<bool>,
    pub response_decompress: Option<bool>,
    pub response_save_resume: Option<bool>,
    pub retry_on_status: Option<Vec<u16>>,
}

#[derive(Deserialize, Default)]
pub struct HostDefaultsPartial {
    pub headers: Option<HashMap<String, Value>>,
}

/// Partial TLS config used for both global config updates and per-request overrides.
/// Inline fields (`cacert`, `cert`, `key`) take precedence over file-path fields
/// (`cacert_file`, `cert_file`, `key_file`). Setting one clears the other.
#[derive(Deserialize, Default, Clone)]
pub struct TlsConfigPartial {
    pub insecure: Option<bool>,
    /// Inline CA certificate as PEM text. Takes precedence over `cacert_file`.
    pub cacert_pem: Option<String>,
    /// Path to CA certificate file (PEM) — like curl --cacert.
    pub cacert_file: Option<String>,
    /// Inline client certificate as PEM text. Takes precedence over `cert_file`.
    pub cert_pem: Option<String>,
    /// Path to client certificate file (PEM) — like curl --cert.
    pub cert_file: Option<String>,
    /// Inline client private key as PEM text (unencrypted). Takes precedence over `key_file`.
    /// Named `_secret` — redacted in all config echo output.
    pub key_pem_secret: Option<String>,
    /// Path to client private key file (PEM, unencrypted) — like curl --key.
    pub key_file: Option<String>,
}

// ---------------------------------------------------------------------------
// Output types (stdout)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(tag = "code")]
pub enum Output {
    #[serde(rename = "response")]
    Response {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
        status: u16,
        headers: HashMap<String, Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        body: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        body_base64: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        body_file: Option<String>,
        /// true when Content-Type was application/json but the body failed JSON
        /// parsing — body contains the raw text string instead.
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        body_parse_failed: bool,
        trace: Trace,
    },

    #[serde(rename = "error")]
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
        error: String,
        error_code: String,
        retryable: bool,
        trace: Trace,
    },

    #[serde(rename = "chunk_start")]
    ChunkStart {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
        status: u16,
        headers: HashMap<String, Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        content_length_bytes: Option<u64>,
    },

    #[serde(rename = "chunk_data")]
    ChunkData {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data_base64: Option<String>,
    },

    #[serde(rename = "chunk_end")]
    ChunkEnd {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        body_file: Option<String>,
        trace: Trace,
    },

    #[serde(rename = "config")]
    Config(RuntimeConfig),

    #[serde(rename = "pong")]
    Pong { trace: PongTrace },

    #[serde(rename = "close")]
    Close { message: String, trace: CloseTrace },

    #[serde(rename = "log")]
    Log {
        event: String,
        #[serde(flatten)]
        fields: HashMap<String, Value>,
    },
}

// ---------------------------------------------------------------------------
// Shared structs
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone)]
pub struct RuntimeConfig {
    pub response_save_dir: String,
    pub response_save_above_bytes: u64,
    pub request_concurrency_limit: u64,
    pub timeout_connect_s: u64,
    pub pool_idle_timeout_s: u64,
    pub retry_base_delay_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    pub tls: TlsConfig,
    pub log: Vec<String>,
    pub defaults: RequestDefaults,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub host_defaults: HashMap<String, HostDefaults>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct RequestDefaults {
    #[serde(default)]
    pub headers: HashMap<String, Value>,
    pub timeout_idle_s: u64,
    pub retry: u32,
    pub response_redirect: u32,
    pub response_parse_json: bool,
    pub response_decompress: bool,
    pub response_save_resume: bool,
    #[serde(default)]
    pub retry_on_status: Vec<u16>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct HostDefaults {
    #[serde(default)]
    pub headers: HashMap<String, Value>,
}

/// Stored TLS configuration (full, non-partial). Inline PEM fields (`*_pem`) take
/// precedence over file-path fields (`*_file`).
#[derive(Serialize, Deserialize, Clone)]
pub struct TlsConfig {
    #[serde(default)]
    pub insecure: bool,
    /// Inline CA certificate as PEM text
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cacert_pem: Option<String>,
    /// Path to CA certificate file (PEM)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cacert_file: Option<String>,
    /// Inline client certificate as PEM text
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cert_pem: Option<String>,
    /// Path to client certificate file (PEM)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cert_file: Option<String>,
    /// Inline client private key as PEM text (unencrypted). Redacted in config echo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_pem_secret: Option<String>,
    /// Path to client private key file (PEM, unencrypted)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_file: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct Trace {
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_addr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sent_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub received_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirects: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunks: Option<u32>,
}

#[derive(Serialize)]
pub struct PongTrace {
    pub uptime_s: u64,
    pub requests_total: u64,
    pub connections_active: u64,
}

#[derive(Serialize)]
pub struct CloseTrace {
    pub uptime_s: u64,
    pub requests_total: u64,
}

// ---------------------------------------------------------------------------
// Resolved options (config defaults merged with per-request options)
// ---------------------------------------------------------------------------

pub struct ResolvedOptions {
    pub timeout_idle_s: u64,
    pub retry: u32,
    pub response_redirect: u32,
    pub response_parse_json: bool,
    pub response_decompress: bool,
    pub response_save_resume: bool,
    pub chunked: bool,
    pub chunked_delimiter: Option<String>, // None = raw
    pub response_save_file: Option<String>,
    pub progress_bytes: u64,
    pub progress_ms: u64,
    pub response_save_above_bytes: u64,
    pub retry_base_delay_ms: u64,
    pub retry_on_status: Vec<u16>,
    pub response_max_bytes: Option<u64>,
}

fn default_chunked_delimiter() -> Value {
    Value::String("\n".to_string())
}

impl Trace {
    pub fn error_only(duration_ms: u64) -> Self {
        Trace {
            duration_ms,
            http_version: None,
            remote_addr: None,
            sent_bytes: None,
            received_bytes: None,
            redirects: None,
            chunks: None,
        }
    }
}

// ---------------------------------------------------------------------------
// ErrorInfo — structured error classification
// ---------------------------------------------------------------------------

pub struct ErrorInfo {
    pub error_code: &'static str,
    pub error: String,
    pub retryable: bool,
}

impl ErrorInfo {
    pub fn invalid_request(detail: impl std::fmt::Display) -> Self {
        ErrorInfo {
            error_code: "invalid_request",
            error: format!("{detail}"),
            retryable: false,
        }
    }

    pub fn cancelled() -> Self {
        ErrorInfo {
            error_code: "cancelled",
            error: "cancelled".to_string(),
            retryable: false,
        }
    }

    pub fn too_many_redirects(max: u32) -> Self {
        ErrorInfo {
            error_code: "too_many_redirects",
            error: format!("exceeded {max}"),
            retryable: false,
        }
    }

    pub fn request_timeout(detail: impl std::fmt::Display) -> Self {
        ErrorInfo {
            error_code: "request_timeout",
            error: format!("{detail}"),
            retryable: false,
        }
    }

    pub fn invalid_response(detail: impl std::fmt::Display) -> Self {
        ErrorInfo {
            error_code: "invalid_response",
            error: format!("{detail}"),
            retryable: false,
        }
    }

    pub fn chunk_disconnected(detail: impl std::fmt::Display) -> Self {
        ErrorInfo {
            error_code: "chunk_disconnected",
            error: format!("{detail}"),
            retryable: false,
        }
    }

    pub fn response_too_large(limit_bytes: u64) -> Self {
        ErrorInfo {
            error_code: "response_too_large",
            error: format!("exceeded {limit_bytes} bytes"),
            retryable: false,
        }
    }

    pub fn overloaded(detail: impl std::fmt::Display) -> Self {
        ErrorInfo {
            error_code: "overloaded",
            error: format!("{detail}"),
            retryable: true,
        }
    }

    pub fn from_reqwest(e: &reqwest::Error) -> Self {
        if e.is_timeout() {
            if e.is_connect() {
                return ErrorInfo {
                    error_code: "connect_timeout",
                    error: e.to_string(),
                    retryable: true,
                };
            }
            return ErrorInfo {
                error_code: "request_timeout",
                error: e.to_string(),
                retryable: false,
            };
        }
        if e.is_connect() {
            let msg = e.to_string().to_lowercase();
            if msg.contains("dns") || msg.contains("resolve") || msg.contains("name") {
                return ErrorInfo {
                    error_code: "dns_failed",
                    error: e.to_string(),
                    retryable: true,
                };
            }
            return ErrorInfo {
                error_code: "connect_refused",
                error: e.to_string(),
                retryable: true,
            };
        }
        let msg = e.to_string().to_lowercase();
        if msg.contains("tls") || msg.contains("ssl") || msg.contains("certificate") {
            return ErrorInfo {
                error_code: "tls_error",
                error: e.to_string(),
                retryable: false,
            };
        }
        if msg.contains("dns") || msg.contains("resolve") {
            return ErrorInfo {
                error_code: "dns_failed",
                error: e.to_string(),
                retryable: true,
            };
        }
        ErrorInfo {
            error_code: "connect_refused",
            error: e.to_string(),
            retryable: true,
        }
    }
}

/// Helper to build Output::Error from ErrorInfo
pub fn make_error(
    id: Option<String>,
    tag: Option<String>,
    info: ErrorInfo,
    trace: Trace,
) -> Output {
    Output::Error {
        id,
        tag,
        error: info.error,
        error_code: info.error_code.to_string(),
        retryable: info.retryable,
        trace,
    }
}

/// Helper to build Output::Log
pub fn make_log(event: &str, fields: Vec<(&str, Value)>) -> Output {
    Output::Log {
        event: event.to_string(),
        fields: fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_options_default_delimiter_is_newline() {
        let opts: RequestOptions = serde_json::from_value(serde_json::json!({})).expect("opts");
        assert_eq!(opts.chunked_delimiter, Value::String("\n".to_string()));
        assert!(!opts.chunked);
    }

    #[test]
    fn trace_error_only_sets_optional_fields_none() {
        let t = Trace::error_only(12);
        assert_eq!(t.duration_ms, 12);
        assert!(t.http_version.is_none());
        assert!(t.remote_addr.is_none());
        assert!(t.sent_bytes.is_none());
        assert!(t.received_bytes.is_none());
        assert!(t.redirects.is_none());
        assert!(t.chunks.is_none());
    }

    #[test]
    fn error_info_builders_and_output_helpers() {
        let version = env!("CARGO_PKG_VERSION");
        let e = ErrorInfo::invalid_request("bad");
        assert_eq!(e.error_code, "invalid_request");
        assert!(!e.retryable);
        let e = ErrorInfo::cancelled();
        assert_eq!(e.error_code, "cancelled");
        let e = ErrorInfo::too_many_redirects(5);
        assert_eq!(e.error, "exceeded 5");
        let e = ErrorInfo::request_timeout("timeout");
        assert_eq!(e.error_code, "request_timeout");
        let e = ErrorInfo::invalid_response("x");
        assert_eq!(e.error_code, "invalid_response");
        let e = ErrorInfo::chunk_disconnected("x");
        assert_eq!(e.error_code, "chunk_disconnected");
        let e = ErrorInfo::response_too_large(100);
        assert_eq!(e.error, "exceeded 100 bytes");
        let e = ErrorInfo::overloaded("busy");
        assert_eq!(e.error_code, "overloaded");
        assert!(e.retryable);

        let out = make_error(
            Some("id1".to_string()),
            Some("tag1".to_string()),
            ErrorInfo::invalid_request("bad"),
            Trace::error_only(1),
        );
        match out {
            Output::Error {
                id,
                tag,
                error_code,
                ..
            } => {
                assert_eq!(id.as_deref(), Some("id1"));
                assert_eq!(tag.as_deref(), Some("tag1"));
                assert_eq!(error_code, "invalid_request");
            }
            _ => panic!("expected Output::Error"),
        }

        let log = make_log(
            "startup",
            vec![("version", Value::String(version.to_string()))],
        );
        match log {
            Output::Log { event, fields } => {
                assert_eq!(event, "startup");
                assert_eq!(fields.get("version"), Some(&Value::String(version.into())));
            }
            _ => panic!("expected Output::Log"),
        }
    }

    #[tokio::test]
    async fn from_reqwest_classifies_connect_and_dns_errors() {
        let client = reqwest::Client::new();

        let connect_err = client
            .get("http://127.0.0.1:1")
            .send()
            .await
            .expect_err("connect should fail");
        let info = ErrorInfo::from_reqwest(&connect_err);
        assert_eq!(info.error_code, "connect_refused");
        assert!(info.retryable);

        let dns_err = client
            .get("http://definitely-not-a-real-host.invalid")
            .send()
            .await
            .expect_err("dns should fail");
        let info = ErrorInfo::from_reqwest(&dns_err);
        assert!(matches!(info.error_code, "dns_failed" | "connect_refused"));
        assert!(info.retryable);
    }
}
