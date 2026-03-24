use crate::config::VERSION;
use crate::types::*;
use agent_first_data::{
    build_cli_error, cli_output, cli_parse_log_filters, cli_parse_output, OutputFormat,
    RedactionPolicy,
};
use clap::{error::ErrorKind, Parser, ValueEnum};
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;

// ---------------------------------------------------------------------------
// Clap argument definition
// ---------------------------------------------------------------------------

#[doc = r#"Agent-First HTTP — persistent HTTP client for AI agents.

### Modes

- `--mode cli` (default): one request, one structured response, then exit
- `--mode pipe`: long-lived JSONL stdin/stdout session for agents
- `--mode curl`: parse a focused subset of curl flags, then execute through the same runtime

### Output and Exit Codes

- default output is one JSON object on stdout
- `--output yaml` and `--output plain` only reformat the envelope; server response bodies are not rewritten
- exit code `0`: HTTP response received
- exit code `1`: transport/runtime error
- exit code `2`: invalid arguments

### Request Body Rules

- `--body` with a JSON object or array auto-sets `Content-Type: application/json`
- string bodies are sent as raw bytes; set `--header "Content-Type: ..."` yourself when needed
- `--body`, `--body-base64`, `--body-file`, `--body-multipart`, and `--body-urlencoded` are mutually exclusive

### Streaming and Files

- `--chunked` emits `chunk_start`, repeated `chunk_data`, then `chunk_end`
- use `--chunked-delimiter '\n\n'` for SSE and `--chunked-delimiter-raw` for binary frames
- `--response-save-file` writes the body to disk; `--response-save-resume` resumes partial downloads
- progress logs are opt-in via `--log progress`

### Examples

```text
afhttp GET https://api.example.com/users
afhttp POST https://api.example.com/users --body '{"name":"Alice"}'
afhttp POST https://api.openai.com/v1/files \
  --header "Authorization: Bearer sk-xxx" \
  --body-multipart purpose=assistants \
  --body-multipart file=@/tmp/data.jsonl;filename=data.jsonl;type=application/jsonl
afhttp GET https://api.example.com/stream --chunked-delimiter '\n\n'
afhttp GET https://example.com/large.tar.gz \
  --response-save-file /tmp/large.tar.gz \
  --log progress
afhttp --mode pipe
```
"#]
#[derive(Parser)]
#[command(name = "afhttp", version = VERSION, verbatim_doc_comment)]
pub struct Cli {
    /// HTTP method (GET, POST, PUT, DELETE, PATCH, HEAD, OPTIONS)
    pub method: Option<String>,

    /// URL to request
    pub url: Option<String>,

    // -- Request flags --
    /// Request header (repeatable). Format: "Name: Value". Empty value removes default.
    #[arg(long = "header", help_heading = "Request")]
    pub header: Vec<String>,

    /// Request body. Valid JSON object/array auto-detected and sets Content-Type: application/json. @path reads from file.
    #[arg(long = "body", help_heading = "Request")]
    pub body: Option<String>,

    /// Base64-encoded binary request body
    #[arg(long = "body-base64", help_heading = "Request")]
    pub body_base64: Option<String>,

    /// Read request body from file
    #[arg(long = "body-file", help_heading = "Request")]
    pub body_file: Option<String>,

    /// Multipart form part (repeatable). Format: name=value or name=@path[;filename=x][;type=mime]
    #[arg(long = "body-multipart", help_heading = "Request")]
    pub body_multipart: Vec<String>,

    /// URL-encoded form field (repeatable). Format: name=value. Sets Content-Type: application/x-www-form-urlencoded.
    #[arg(long = "body-urlencoded", help_heading = "Request")]
    pub body_urlencoded: Vec<String>,

    // -- Config flags --
    /// Directory for auto-saved large response bodies
    #[arg(long = "response-save-dir", help_heading = "Config")]
    pub response_save_dir: Option<String>,

    /// Auto-save response body to response-save-dir when larger than this (default: 10485760)
    #[arg(long = "response-save-above-bytes", help_heading = "Config")]
    pub response_save_above_bytes: Option<u64>,

    /// Max concurrent in-flight requests (0 = unlimited)
    #[arg(long = "request-concurrency-limit", help_heading = "Config")]
    pub request_concurrency_limit: Option<u64>,

    /// TCP+TLS handshake timeout in seconds (default: 10)
    #[arg(long = "timeout-connect-s", help_heading = "Config")]
    pub timeout_connect_s: Option<u64>,

    /// No-data timeout in seconds (default: 30)
    #[arg(long = "timeout-idle-s", help_heading = "Config")]
    pub timeout_idle_s: Option<u64>,

    /// Retry count (default: 0, no retry)
    #[arg(long, help_heading = "Config")]
    pub retry: Option<u32>,

    /// Base delay for first retry in ms (default: 100). Subsequent: base * 2^(attempt-1)
    #[arg(long = "retry-base-delay-ms", help_heading = "Config")]
    pub retry_base_delay_ms: Option<u64>,

    /// Comma-separated status codes to retry (e.g. 429,503)
    #[arg(long = "retry-on-status", help_heading = "Config")]
    pub retry_on_status: Option<String>,

    /// Redirect limit (default: 10, 0=disable)
    #[arg(long = "response-redirect", help_heading = "Config")]
    pub response_redirect: Option<u32>,

    /// Parse JSON response body (default: true)
    #[arg(long = "response-parse-json", help_heading = "Config")]
    pub response_parse_json: Option<bool>,

    /// Auto-decompress response (default: true)
    #[arg(long = "response-decompress", help_heading = "Config")]
    pub response_decompress: Option<bool>,

    /// Save response body to file
    #[arg(long = "response-save-file", help_heading = "Config")]
    pub response_save_file: Option<String>,

    /// Resume download if response-save-file exists
    #[arg(long = "response-save-resume", help_heading = "Config")]
    pub response_save_resume: bool,

    /// Hard limit on response body size in bytes
    #[arg(long = "response-max-bytes", help_heading = "Config")]
    pub response_max_bytes: Option<u64>,

    /// Stream response in chunks
    #[arg(long, help_heading = "Config")]
    pub chunked: bool,

    /// Chunk delimiter (default: \n). Use \n\n for SSE. Implies --chunked
    #[arg(long = "chunked-delimiter", help_heading = "Config")]
    pub chunked_delimiter: Option<String>,

    /// Raw binary chunks (null delimiter). Implies --chunked
    #[arg(long = "chunked-delimiter-raw", help_heading = "Config")]
    pub chunked_delimiter_raw: bool,

    /// Time-based progress interval in ms (default: 10000, 0=disable). Works with --progress-bytes
    #[arg(long = "progress-ms", help_heading = "Config")]
    pub progress_ms: Option<u64>,

    /// Byte-based progress interval (default: 0=disable). Works with --progress-ms
    #[arg(long = "progress-bytes", help_heading = "Config")]
    pub progress_bytes: Option<u64>,

    // -- TLS flags --
    /// Skip certificate verification
    #[arg(long = "tls-insecure", help_heading = "TLS")]
    pub tls_insecure: bool,

    /// CA certificate file path
    #[arg(long = "tls-cacert-file", help_heading = "TLS")]
    pub tls_cacert_file: Option<String>,

    /// Client certificate file path
    #[arg(long = "tls-cert-file", help_heading = "TLS")]
    pub tls_cert_file: Option<String>,

    /// Client private key file path
    #[arg(long = "tls-key-file", help_heading = "TLS")]
    pub tls_key_file: Option<String>,

    // -- Other --
    /// Proxy URL
    #[arg(long, help_heading = "Other")]
    pub proxy: Option<String>,

    /// Protocol upgrade (e.g. "websocket")
    #[arg(long, help_heading = "Other")]
    pub upgrade: Option<String>,

    // -- Output flags --
    /// Output format: json (default), yaml (human-readable), plain (logfmt)
    #[arg(long, default_value = "json", help_heading = "Output")]
    pub output: String,

    /// Log categories (comma-separated). Categories: startup, request, progress, retry, redirect
    #[arg(long, help_heading = "Output")]
    pub log: Option<String>,

    /// Enable all log categories (equivalent to --log startup,request,progress,retry,redirect)
    #[arg(long, help_heading = "Output")]
    pub verbose: bool,

    /// Preview the request without executing it
    #[arg(long, help_heading = "Output")]
    pub dry_run: bool,

    // -- Mode --
    /// Runtime mode: cli (default), pipe, or curl
    #[arg(long, value_enum, default_value = "cli", help_heading = "Mode")]
    pub mode: CliMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum CliMode {
    Cli,
    Pipe,
    Curl,
}

// ---------------------------------------------------------------------------
// Parsed CLI request
// ---------------------------------------------------------------------------

pub struct CliRequest {
    pub method: String,
    pub url: String,
    pub headers: HashMap<String, Value>,
    pub body: Option<Value>,
    pub body_base64: Option<String>,
    pub body_file: Option<String>,
    pub body_multipart: Option<Vec<MultipartPart>>,
    pub body_urlencoded: Option<Vec<UrlencodedPart>>,
    pub options: RequestOptions,
    pub config_overrides: ConfigPatch,
    /// Log categories enabled via --log or --verbose. Includes "startup" if requested.
    pub log_categories: Vec<String>,
    /// Output format for CLI output
    pub output_format: OutputFormat,
    /// Preview the request without executing it
    pub dry_run: bool,
}

// ---------------------------------------------------------------------------
// Mode enum
// ---------------------------------------------------------------------------

pub enum Mode {
    Cli(Box<CliRequest>),
    Pipe(Box<PipeInit>),
}

pub struct PipeInit {
    pub config: ConfigPatch,
    pub output_format: OutputFormat,
}

fn emit_cli_usage_error_and_exit(message: impl AsRef<str>, hint: Option<&str>) -> ! {
    let json = cli_output(&build_cli_error(message.as_ref(), hint), OutputFormat::Json);
    let _ = writeln!(std::io::stdout(), "{json}");
    std::process::exit(2);
}

fn raw_mode_is_curl(raw: &[String]) -> bool {
    let mut i = 1;
    while i < raw.len() {
        if raw[i] == "--mode" {
            return raw.get(i + 1).map(String::as_str) == Some("curl");
        }
        if let Some(v) = raw[i].strip_prefix("--mode=") {
            return v == "curl";
        }
        i += 1;
    }
    false
}

fn strip_mode_flag(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--mode" {
            i += 1;
            if i < args.len() {
                i += 1;
            }
            continue;
        }
        if args[i].starts_with("--mode=") {
            i += 1;
            continue;
        }
        out.push(args[i].clone());
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Parse args into Mode
// ---------------------------------------------------------------------------

pub fn parse_args() -> Mode {
    // Curl mode must be handled before Clap parsing so curl-style flags
    // (e.g. -k, -I, --insecure) do not fail clap validation.
    let raw: Vec<String> = std::env::args().collect();
    if raw_mode_is_curl(&raw) {
        let curl_args = strip_mode_flag(&raw[1..]);
        return crate::curl_compat::parse_curl_args(&curl_args);
    }

    let cli = Cli::try_parse().unwrap_or_else(|e| {
        if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
            e.exit();
        }
        emit_cli_usage_error_and_exit(e.to_string(), None);
    });
    let output_format = match cli_parse_output(&cli.output) {
        Ok(f) => f,
        Err(e) => emit_cli_usage_error_and_exit(e, None),
    };

    match cli.mode {
        CliMode::Pipe => {
            // Build config overrides from CLI flags so --mode pipe --log startup,retry --proxy ...
            // all take effect at launch time.
            const ALL_CATEGORIES: &[&str] =
                &["startup", "request", "progress", "retry", "redirect"];
            let log_categories: Vec<String> = if cli.verbose {
                cli_parse_log_filters(ALL_CATEGORIES)
            } else if let Some(ref log_str) = cli.log {
                let entries: Vec<&str> = log_str.split(',').collect();
                cli_parse_log_filters(&entries)
            } else {
                vec![]
            };
            let has_log_flag = cli.verbose || cli.log.is_some();
            let tls = build_tls_partial(&cli);
            let pipe_config = ConfigPatch {
                response_save_dir: cli.response_save_dir.clone(),
                response_save_above_bytes: cli.response_save_above_bytes,
                request_concurrency_limit: cli.request_concurrency_limit,
                timeout_connect_s: cli.timeout_connect_s,
                retry_base_delay_ms: cli.retry_base_delay_ms,
                proxy: cli.proxy.clone(),
                tls,
                log: if has_log_flag {
                    Some(log_categories)
                } else {
                    None
                },
                ..ConfigPatch::default()
            };
            return Mode::Pipe(Box::new(PipeInit {
                config: pipe_config,
                output_format,
            }));
        }
        CliMode::Curl => {
            let curl_args = strip_mode_flag(&raw[1..]);
            return crate::curl_compat::parse_curl_args(&curl_args);
        }
        CliMode::Cli => {}
    }

    let method = match cli.method {
        Some(ref m) => m.to_uppercase(),
        None => {
            // No method in cli mode: show help and exit 2
            let mut cmd = <Cli as clap::CommandFactory>::command();
            let _ = cmd.print_help();
            let _ = writeln!(std::io::stdout());
            std::process::exit(2);
        }
    };

    let url = match cli.url {
        Some(ref u) => u.clone(),
        None => {
            emit_cli_usage_error_and_exit(
                "URL is required after method",
                Some("usage: afhttp METHOD URL [flags]"),
            );
        }
    };

    // Parse headers
    let mut headers = HashMap::new();
    for h in &cli.header {
        let (name, value) = parse_header_flag(h);
        headers.insert(name, value);
    }

    // Parse body
    let (body, body_base64, body_file, body_multipart, body_urlencoded) = parse_body_flags(&cli);

    // Build chunked options
    let mut chunked = cli.chunked;
    let chunked_delimiter = if cli.chunked_delimiter_raw {
        chunked = true;
        Value::Null
    } else if let Some(ref d) = cli.chunked_delimiter {
        chunked = true;
        Value::String(unescape_delimiter(d))
    } else {
        Value::String("\n".to_string())
    };

    // Build TLS partial (borrows cli)
    let tls = build_tls_partial(&cli);

    // Parse log categories from --verbose or --log
    const ALL_CATEGORIES: &[&str] = &["startup", "request", "progress", "retry", "redirect"];
    let log_categories: Vec<String> = if cli.verbose {
        cli_parse_log_filters(ALL_CATEGORIES)
    } else if let Some(ref log_str) = cli.log {
        let entries: Vec<&str> = log_str.split(',').collect();
        cli_parse_log_filters(&entries)
    } else {
        vec![]
    };

    // Build config overrides — non-startup log categories flow through here
    let has_log_flag = cli.verbose || cli.log.is_some();
    let config_overrides = ConfigPatch {
        response_save_dir: cli.response_save_dir.clone(),
        response_save_above_bytes: cli.response_save_above_bytes,
        request_concurrency_limit: cli.request_concurrency_limit,
        timeout_connect_s: cli.timeout_connect_s,
        retry_base_delay_ms: cli.retry_base_delay_ms,
        proxy: cli.proxy.clone(),
        log: if has_log_flag {
            Some(
                log_categories
                    .iter()
                    .filter(|c| *c != "startup")
                    .cloned()
                    .collect(),
            )
        } else {
            None
        },
        ..ConfigPatch::default()
    };

    // Parse retry_on_status from comma-separated string
    let retry_on_status = cli.retry_on_status.as_deref().map(|s| {
        s.split(',')
            .filter_map(|c| c.trim().parse::<u16>().ok())
            .collect()
    });

    // Build request options (consumes remaining cli fields)
    let options = RequestOptions {
        timeout_idle_s: cli.timeout_idle_s,
        retry: cli.retry,
        response_redirect: cli.response_redirect,
        response_parse_json: cli.response_parse_json,
        response_decompress: cli.response_decompress,
        response_save_resume: if cli.response_save_resume {
            Some(true)
        } else {
            None
        },
        chunked,
        chunked_delimiter,
        response_save_file: cli.response_save_file,
        progress_bytes: cli.progress_bytes,
        progress_ms: cli.progress_ms,
        retry_on_status,
        response_max_bytes: cli.response_max_bytes,
        upgrade: cli.upgrade,
        tls,
    };

    Mode::Cli(Box::new(CliRequest {
        method,
        url,
        headers,
        body,
        body_base64,
        body_file,
        body_multipart,
        body_urlencoded,
        options,
        config_overrides,
        log_categories,
        output_format,
        dry_run: cli.dry_run,
    }))
}

// ---------------------------------------------------------------------------
// CLI output writer (strips id/tag from Output)
// ---------------------------------------------------------------------------

pub fn write_cli_output(output: &Output, format: OutputFormat) {
    let mut value = match serde_json::to_value(output) {
        Ok(v) => v,
        Err(_) => {
            let fallback = r#"{"code":"error","error_code":"internal_error","error":"output serialization failed","retryable":false,"trace":{"duration_ms":0}}"#;
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            let _ = out.write_all(fallback.as_bytes());
            let _ = out.write_all(b"\n");
            let _ = out.flush();
            return;
        }
    };

    // Strip id and tag fields for CLI output
    if let Some(obj) = value.as_object_mut() {
        obj.remove("id");
        obj.remove("tag");
    }

    let formatted = if matches!(format, OutputFormat::Json) {
        match json_redaction_policy_for_output(output) {
            Some(policy) => agent_first_data::output_json_with(&value, policy),
            None => agent_first_data::output_json(&value),
        }
    } else {
        // Protect server body fields from Agent-First Data suffix processing.
        // Non-string body (parsed JSON objects) is converted to a JSON string
        // so formatters treat them as opaque data.
        protect_server_body(&mut value);
        cli_output(&value, format)
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let _ = out.write_all(formatted.as_bytes());
    if !formatted.ends_with('\n') {
        let _ = out.write_all(b"\n");
    }
    let _ = out.flush();
}

fn json_redaction_policy_for_output(output: &Output) -> Option<RedactionPolicy> {
    match output {
        // Keep server payload raw in response body; only trace metadata is redacted.
        Output::Response { .. } => Some(RedactionPolicy::RedactionTraceOnly),
        // Stream chunks are opaque server data.
        Output::ChunkData { .. } => Some(RedactionPolicy::RedactionNone),
        // Other events keep existing safe default.
        _ => None,
    }
}

/// Protect server-originated body fields from Agent-First Data suffix processing.
/// Converts non-string body/data values to JSON string representation
/// so yaml/plain formatters treat them as opaque strings.
fn protect_server_body(value: &mut Value) {
    if let Some(obj) = value.as_object_mut() {
        for key in &["body", "data"] {
            if let Some(v) = obj.get(*key).cloned() {
                if !v.is_null() && !v.is_string() {
                    if let Ok(json_str) = serde_json::to_string(&v) {
                        obj.insert((*key).to_string(), Value::String(json_str));
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn parse_header_flag(s: &str) -> (String, Value) {
    let colon_pos = match s.find(':') {
        Some(p) => p,
        None => {
            emit_cli_usage_error_and_exit(
                format!("invalid header '{s}'"),
                Some("expected format: Name: Value"),
            );
        }
    };
    let name = s[..colon_pos].trim().to_string();
    let value = s[colon_pos + 1..].trim();
    if value.is_empty() {
        (name, Value::Null) // null removes default
    } else {
        (name, Value::String(value.to_string()))
    }
}

#[allow(clippy::type_complexity)]
fn parse_body_flags(
    cli: &Cli,
) -> (
    Option<Value>,
    Option<String>,
    Option<String>,
    Option<Vec<MultipartPart>>,
    Option<Vec<UrlencodedPart>>,
) {
    let has_body = cli.body.is_some();
    let has_base64 = cli.body_base64.is_some();
    let has_file = cli.body_file.is_some();
    let has_multipart = !cli.body_multipart.is_empty();
    let has_urlencoded = !cli.body_urlencoded.is_empty();

    let count = [
        has_body,
        has_base64,
        has_file,
        has_multipart,
        has_urlencoded,
    ]
    .iter()
    .filter(|&&b| b)
    .count();
    if count > 1 {
        emit_cli_usage_error_and_exit(
            "--body, --body-base64, --body-file, --body-multipart, and --body-urlencoded are mutually exclusive",
            Some("use only one body flag per request"),
        );
    }

    if let Some(ref b) = cli.body {
        // @path -> body_file
        if let Some(path) = b.strip_prefix('@') {
            return (None, None, Some(path.to_string()), None, None);
        }
        // JSON auto-detect: full parse, object or array only — numbers/booleans/null are ambiguous
        let trimmed = b.trim();
        if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
            if v.is_object() || v.is_array() {
                return (Some(v), None, None, None, None);
            }
        }
        // Plain text
        return (Some(Value::String(b.clone())), None, None, None, None);
    }

    if let Some(ref b64) = cli.body_base64 {
        return (None, Some(b64.clone()), None, None, None);
    }

    if let Some(ref path) = cli.body_file {
        return (None, None, Some(path.clone()), None, None);
    }

    if !cli.body_multipart.is_empty() {
        let parts: Vec<MultipartPart> = cli
            .body_multipart
            .iter()
            .map(|s| parse_form_flag(s))
            .collect();
        return (None, None, None, Some(parts), None);
    }

    if !cli.body_urlencoded.is_empty() {
        let parts: Vec<UrlencodedPart> = cli
            .body_urlencoded
            .iter()
            .map(|s| parse_urlencoded_flag(s))
            .collect();
        return (None, None, None, None, Some(parts));
    }

    (None, None, None, None, None)
}

fn parse_form_flag(s: &str) -> MultipartPart {
    let eq_pos = match s.find('=') {
        Some(p) => p,
        None => {
            emit_cli_usage_error_and_exit(
                format!("invalid --body-multipart '{s}'"),
                Some("expected format: name=value or name=@filepath"),
            );
        }
    };
    let name = s[..eq_pos].to_string();
    let rest = &s[eq_pos + 1..];

    if let Some(file_rest) = rest.strip_prefix('@') {
        // File part: name=@path[;filename=x][;type=mime]
        let parts: Vec<&str> = file_rest.split(';').collect();
        let file = parts[0].to_string();
        let mut filename = None;
        let mut content_type = None;
        for p in &parts[1..] {
            if let Some(f) = p.strip_prefix("filename=") {
                filename = Some(f.to_string());
            } else if let Some(t) = p.strip_prefix("type=") {
                content_type = Some(t.to_string());
            }
        }
        MultipartPart {
            name,
            value: None,
            value_base64: None,
            file: Some(file),
            filename,
            content_type,
        }
    } else {
        // Text part
        MultipartPart {
            name,
            value: Some(rest.to_string()),
            value_base64: None,
            file: None,
            filename: None,
            content_type: None,
        }
    }
}

fn parse_urlencoded_flag(s: &str) -> UrlencodedPart {
    match s.find('=') {
        Some(pos) => UrlencodedPart {
            name: s[..pos].to_string(),
            value: s[pos + 1..].to_string(),
        },
        None => {
            emit_cli_usage_error_and_exit(
                format!("invalid --body-urlencoded '{s}'"),
                Some("expected format: name=value"),
            );
        }
    }
}

fn build_tls_partial(cli: &Cli) -> Option<TlsConfigPartial> {
    if cli.tls_insecure
        || cli.tls_cacert_file.is_some()
        || cli.tls_cert_file.is_some()
        || cli.tls_key_file.is_some()
    {
        Some(TlsConfigPartial {
            insecure: if cli.tls_insecure { Some(true) } else { None },
            cacert_pem: None,
            cacert_file: cli.tls_cacert_file.clone(),
            cert_pem: None,
            cert_file: cli.tls_cert_file.clone(),
            key_pem_secret: None,
            key_file: cli.tls_key_file.clone(),
        })
    } else {
        None
    }
}

/// Unescape common delimiter literals: \\n -> \n
fn unescape_delimiter(s: &str) -> String {
    s.replace("\\n", "\n")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn empty_cli() -> Cli {
        Cli {
            method: None,
            url: None,
            header: vec![],
            body: None,
            body_base64: None,
            body_file: None,
            body_multipart: vec![],
            body_urlencoded: vec![],
            response_save_dir: None,
            response_save_above_bytes: None,
            request_concurrency_limit: None,
            timeout_connect_s: None,
            timeout_idle_s: None,
            retry: None,
            retry_base_delay_ms: None,
            retry_on_status: None,
            response_redirect: None,
            response_parse_json: None,
            response_decompress: None,
            response_save_file: None,
            response_save_resume: false,
            response_max_bytes: None,
            chunked: false,
            chunked_delimiter: None,
            chunked_delimiter_raw: false,
            progress_ms: None,
            progress_bytes: None,
            tls_insecure: false,
            tls_cacert_file: None,
            tls_cert_file: None,
            tls_key_file: None,
            proxy: None,
            upgrade: None,
            output: "json".to_string(),
            log: None,
            verbose: false,
            dry_run: false,
            mode: CliMode::Cli,
        }
    }

    #[test]
    fn parse_header_flag_normal_and_remove_default() {
        let (name, value) = parse_header_flag("X-Test: abc");
        assert_eq!(name, "X-Test");
        assert_eq!(value, Value::String("abc".to_string()));

        let (name, value) = parse_header_flag("X-Remove:   ");
        assert_eq!(name, "X-Remove");
        assert_eq!(value, Value::Null);
    }

    #[test]
    fn parse_body_flags_object_array_string_and_files() {
        let mut cli = empty_cli();
        cli.body = Some("{\"a\":1}".to_string());
        let (body, b64, file, mp, ue) = parse_body_flags(&cli);
        assert_eq!(body, Some(serde_json::json!({"a":1})));
        assert!(b64.is_none() && file.is_none() && mp.is_none() && ue.is_none());

        let mut cli = empty_cli();
        cli.body = Some("[1,2]".to_string());
        let (body, _, _, _, _) = parse_body_flags(&cli);
        assert_eq!(body, Some(serde_json::json!([1, 2])));

        let mut cli = empty_cli();
        cli.body = Some("hello".to_string());
        let (body, _, _, _, _) = parse_body_flags(&cli);
        assert_eq!(body, Some(Value::String("hello".to_string())));

        let mut cli = empty_cli();
        cli.body = Some("@/tmp/body.txt".to_string());
        let (_, _, file, _, _) = parse_body_flags(&cli);
        assert_eq!(file.as_deref(), Some("/tmp/body.txt"));

        let mut cli = empty_cli();
        cli.body_base64 = Some("aGVsbG8=".to_string());
        let (_, b64, _, _, _) = parse_body_flags(&cli);
        assert_eq!(b64.as_deref(), Some("aGVsbG8="));

        let mut cli = empty_cli();
        cli.body_file = Some("/tmp/f.bin".to_string());
        let (_, _, file, _, _) = parse_body_flags(&cli);
        assert_eq!(file.as_deref(), Some("/tmp/f.bin"));
    }

    #[test]
    fn parse_body_flags_multipart_and_urlencoded() {
        let mut cli = empty_cli();
        cli.body_multipart = vec![
            "name=roger".to_string(),
            "upload=@/tmp/a.txt;filename=x.txt;type=text/plain".to_string(),
        ];
        let (_, _, _, mp, _) = parse_body_flags(&cli);
        let parts = mp.expect("multipart");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].name, "name");
        assert_eq!(parts[0].value.as_deref(), Some("roger"));
        assert_eq!(parts[1].file.as_deref(), Some("/tmp/a.txt"));
        assert_eq!(parts[1].filename.as_deref(), Some("x.txt"));
        assert_eq!(parts[1].content_type.as_deref(), Some("text/plain"));

        let mut cli = empty_cli();
        cli.body_urlencoded = vec!["a=1".to_string(), "b=".to_string()];
        let (_, _, _, _, ue) = parse_body_flags(&cli);
        let parts = ue.expect("urlencoded");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].name, "a");
        assert_eq!(parts[0].value, "1");
        assert_eq!(parts[1].name, "b");
        assert_eq!(parts[1].value, "");
    }

    #[test]
    fn parse_form_and_urlencoded_flags() {
        let p = parse_form_flag("n=v");
        assert_eq!(p.name, "n");
        assert_eq!(p.value.as_deref(), Some("v"));
        assert!(p.file.is_none());

        let p = parse_form_flag("f=@/tmp/a.bin;filename=b.bin;type=application/octet-stream");
        assert_eq!(p.file.as_deref(), Some("/tmp/a.bin"));
        assert_eq!(p.filename.as_deref(), Some("b.bin"));
        assert_eq!(p.content_type.as_deref(), Some("application/octet-stream"));

        let p = parse_urlencoded_flag("x=1");
        assert_eq!(p.name, "x");
        assert_eq!(p.value, "1");
    }

    #[test]
    fn build_tls_partial_and_unescape_delimiter() {
        let mut cli = empty_cli();
        assert!(build_tls_partial(&cli).is_none());

        cli.tls_insecure = true;
        cli.tls_cacert_file = Some("/tmp/ca.pem".to_string());
        cli.tls_cert_file = Some("/tmp/cert.pem".to_string());
        cli.tls_key_file = Some("/tmp/key.pem".to_string());
        let tls = build_tls_partial(&cli).expect("tls");
        assert_eq!(tls.insecure, Some(true));
        assert_eq!(tls.cacert_file.as_deref(), Some("/tmp/ca.pem"));
        assert_eq!(tls.cert_file.as_deref(), Some("/tmp/cert.pem"));
        assert_eq!(tls.key_file.as_deref(), Some("/tmp/key.pem"));

        assert_eq!(unescape_delimiter("\\n\\n"), "\n\n");
    }

    #[test]
    fn protect_server_body_stringifies_non_string() {
        let mut value = serde_json::json!({
            "body": {"a": 1},
            "data": [1,2],
            "other": true
        });
        protect_server_body(&mut value);
        assert_eq!(
            value.get("body"),
            Some(&Value::String("{\"a\":1}".to_string()))
        );
        assert_eq!(value.get("data"), Some(&Value::String("[1,2]".to_string())));
        assert_eq!(value.get("other"), Some(&Value::Bool(true)));
    }

    #[test]
    fn json_redaction_policy_for_response_and_log() {
        let resp = Output::Response {
            id: "1".to_string(),
            tag: None,
            status: 200,
            headers: HashMap::new(),
            body: Some(serde_json::json!({"api_key_secret":"sk-live-123"})),
            body_base64: None,
            body_file: None,
            body_parse_failed: false,
            trace: Trace::error_only(1),
        };
        assert_eq!(
            json_redaction_policy_for_output(&resp),
            Some(RedactionPolicy::RedactionTraceOnly)
        );

        let log = Output::Log {
            event: "startup".to_string(),
            fields: HashMap::from([(
                "api_key_secret".to_string(),
                Value::String("sk-live-123".to_string()),
            )]),
        };
        assert_eq!(json_redaction_policy_for_output(&log), None);
    }

    #[test]
    fn curl_mode_helpers() {
        let raw = vec![
            "afhttp".to_string(),
            "--mode".to_string(),
            "curl".to_string(),
        ];
        assert!(raw_mode_is_curl(&raw));
        assert_eq!(strip_mode_flag(&raw[1..]), Vec::<String>::new());

        let raw = vec![
            "afhttp".to_string(),
            "--mode=curl".to_string(),
            "-X".to_string(),
            "GET".to_string(),
            "https://example.com".to_string(),
        ];
        assert!(raw_mode_is_curl(&raw));
        assert_eq!(
            strip_mode_flag(&raw[1..]),
            vec![
                "-X".to_string(),
                "GET".to_string(),
                "https://example.com".to_string()
            ]
        );

        let raw = vec![
            "afhttp".to_string(),
            "--mode".to_string(),
            "pipe".to_string(),
        ];
        assert!(!raw_mode_is_curl(&raw));
    }
}
