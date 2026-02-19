use crate::config::VERSION;
use crate::types::*;
use clap::Parser;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;

// ---------------------------------------------------------------------------
// Clap argument definition
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "afhttp",
    version = VERSION,
    about = "Agent-First HTTP — persistent HTTP client for AI agents",
    after_help = "EXAMPLES:\n  afhttp GET https://api.example.com/users\n  afhttp POST https://api.example.com/users --body '{\"name\":\"Alice\"}'\n  afhttp GET https://api.example.com/stream --chunked\n  afhttp GET https://api.example.com/users --output yaml\n  afhttp --pipe    # JSONL stdin/stdout mode"
)]
pub struct Cli {
    /// HTTP method (GET, POST, PUT, DELETE, PATCH, HEAD, OPTIONS)
    pub method: Option<String>,

    /// URL to request
    pub url: Option<String>,

    // -- Request flags --
    /// Request header (repeatable). Format: "Name: Value". Empty value removes default.
    #[arg(long = "header")]
    pub header: Vec<String>,

    /// Request body. Valid JSON object/array auto-detected and sets Content-Type: application/json. @path reads from file.
    #[arg(long = "body")]
    pub body: Option<String>,

    /// Base64-encoded binary request body
    #[arg(long = "body-base64")]
    pub body_base64: Option<String>,

    /// Read request body from file
    #[arg(long = "body-file")]
    pub body_file: Option<String>,

    /// Multipart form part (repeatable). Format: name=value or name=@path[;filename=x][;type=mime]
    #[arg(long = "body-multipart")]
    pub body_multipart: Vec<String>,

    /// URL-encoded form field (repeatable). Format: name=value. Sets Content-Type: application/x-www-form-urlencoded.
    #[arg(long = "body-urlencoded")]
    pub body_urlencoded: Vec<String>,

    // -- Config flags --
    /// Directory for auto-saved large response bodies
    #[arg(long = "response-save-dir")]
    pub response_save_dir: Option<String>,

    /// Auto-save response body to response-save-dir when larger than this (default: 10485760)
    #[arg(long = "response-save-above-bytes")]
    pub response_save_above_bytes: Option<u64>,

    /// Max concurrent in-flight requests (0 = unlimited)
    #[arg(long = "request-concurrency-limit")]
    pub request_concurrency_limit: Option<u64>,

    /// TCP+TLS handshake timeout in seconds (default: 10)
    #[arg(long = "timeout-connect-s")]
    pub timeout_connect_s: Option<u64>,

    /// No-data timeout in seconds (default: 30)
    #[arg(long = "timeout-idle-s")]
    pub timeout_idle_s: Option<u64>,

    /// Retry count (default: 0, no retry)
    #[arg(long)]
    pub retry: Option<u32>,

    /// Base delay for first retry in ms (default: 100). Subsequent: base * 2^(attempt-1)
    #[arg(long = "retry-base-delay-ms")]
    pub retry_base_delay_ms: Option<u64>,

    /// Comma-separated status codes to retry (e.g. 429,503)
    #[arg(long = "retry-on-status")]
    pub retry_on_status: Option<String>,

    /// Redirect limit (default: 10, 0=disable)
    #[arg(long = "response-redirect")]
    pub response_redirect: Option<u32>,

    /// Parse JSON response body (default: true)
    #[arg(long = "response-parse-json")]
    pub response_parse_json: Option<bool>,

    /// Auto-decompress response (default: true)
    #[arg(long = "response-decompress")]
    pub response_decompress: Option<bool>,

    /// Save response body to file
    #[arg(long = "response-save-file")]
    pub response_save_file: Option<String>,

    /// Resume download if response-save-file exists
    #[arg(long = "response-save-resume")]
    pub response_save_resume: bool,

    /// Hard limit on response body size in bytes
    #[arg(long = "response-max-bytes")]
    pub response_max_bytes: Option<u64>,

    /// Stream response in chunks
    #[arg(long)]
    pub chunked: bool,

    /// Chunk delimiter (default: \n). Use \n\n for SSE. Implies --chunked
    #[arg(long = "chunked-delimiter")]
    pub chunked_delimiter: Option<String>,

    /// Raw binary chunks (null delimiter). Implies --chunked
    #[arg(long = "chunked-delimiter-raw")]
    pub chunked_delimiter_raw: bool,

    /// Time-based progress interval in ms (default: 10000, 0=disable). Works with --progress-bytes
    #[arg(long = "progress-ms")]
    pub progress_ms: Option<u64>,

    /// Byte-based progress interval (default: 0=disable). Works with --progress-ms
    #[arg(long = "progress-bytes")]
    pub progress_bytes: Option<u64>,

    // -- TLS flags --
    /// Skip certificate verification
    #[arg(long = "tls-insecure")]
    pub tls_insecure: bool,

    /// CA certificate file path
    #[arg(long = "tls-cacert-file")]
    pub tls_cacert_file: Option<String>,

    /// Client certificate file path
    #[arg(long = "tls-cert-file")]
    pub tls_cert_file: Option<String>,

    /// Client private key file path
    #[arg(long = "tls-key-file")]
    pub tls_key_file: Option<String>,

    // -- Other --
    /// Proxy URL
    #[arg(long)]
    pub proxy: Option<String>,

    /// Protocol upgrade (e.g. "websocket")
    #[arg(long)]
    pub upgrade: Option<String>,

    // -- Output flags --
    /// Output format: json (default), yaml (human-readable), plain (logfmt)
    #[arg(long, default_value = "json")]
    pub output: String,

    /// Log categories (comma-separated). Categories: startup, request, progress, retry, redirect
    #[arg(long)]
    pub log: Option<String>,

    /// Enable all log categories (equivalent to --log startup,request,progress,retry,redirect)
    #[arg(long)]
    pub verbose: bool,

    // -- Mode flags --
    /// JSONL stdin/stdout pipe mode
    #[arg(long)]
    pub pipe: bool,

    /// MCP server mode (Model Context Protocol, stdio transport)
    #[arg(long)]
    pub mcp: bool,
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
}

// ---------------------------------------------------------------------------
// Mode enum
// ---------------------------------------------------------------------------

pub enum Mode {
    Cli(Box<CliRequest>),
    Pipe(Box<ConfigPatch>),
    Mcp,
}

#[derive(Clone, Copy)]
pub enum OutputFormat {
    Json,
    Yaml,
    Plain,
}

// ---------------------------------------------------------------------------
// Parse args into Mode
// ---------------------------------------------------------------------------

pub fn parse_args() -> Mode {
    // curl compatibility detection: must happen before Cli::parse() so clap
    // doesn't choke on curl-style flags (e.g. -k, -I, --insecure).
    let raw: Vec<String> = std::env::args().collect();
    let prog = raw.first().map(String::as_str).unwrap_or("");
    let is_curl_argv0 = std::path::Path::new(prog)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.contains("curl"))
        .unwrap_or(false);
    let is_curl_subcmd = raw.get(1).map(String::as_str) == Some("curl");

    if is_curl_argv0 {
        return crate::curl_compat::parse_curl_args(&raw[1..]);
    }
    if is_curl_subcmd {
        return crate::curl_compat::parse_curl_args(&raw[2..]);
    }

    let cli = Cli::parse();

    // MCP mode: must check before method/url validation
    if cli.mcp {
        return Mode::Mcp;
    }

    if cli.pipe {
        // Build config overrides from CLI flags so --pipe --log startup,retry --proxy ...
        // all take effect at launch time.
        const ALL_CATEGORIES: &[&str] = &["startup", "request", "progress", "retry", "redirect"];
        let log_categories: Vec<String> = if cli.verbose {
            ALL_CATEGORIES.iter().map(|s| s.to_string()).collect()
        } else if let Some(ref log_str) = cli.log {
            log_str
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
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
        return Mode::Pipe(Box::new(pipe_config));
    }

    let method = match cli.method {
        Some(ref m) => m.to_uppercase(),
        None => {
            // No method and no --pipe: show help and exit 2
            let mut cmd = <Cli as clap::CommandFactory>::command();
            let _ = cmd.print_help();
            eprintln!();
            std::process::exit(2);
        }
    };

    let url = match cli.url {
        Some(ref u) => u.clone(),
        None => {
            eprintln!("error: URL is required after method");
            std::process::exit(2);
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
        ALL_CATEGORIES.iter().map(|s| s.to_string()).collect()
    } else if let Some(ref log_str) = cli.log {
        log_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
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

    // Parse output format
    let output_format = match cli.output.as_str() {
        "json" => OutputFormat::Json,
        "yaml" => OutputFormat::Yaml,
        "plain" => OutputFormat::Plain,
        other => {
            eprintln!("error: unknown --output format '{other}': expected json, yaml, or plain");
            std::process::exit(2);
        }
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

    let formatted = match format {
        OutputFormat::Json => agent_first_data::output_json(&value),
        OutputFormat::Yaml | OutputFormat::Plain => {
            // Protect server body fields from AFD suffix processing.
            // Non-string body (parsed JSON objects) is converted to a JSON string
            // so formatters treat it as opaque data.
            protect_server_body(&mut value);
            if matches!(format, OutputFormat::Yaml) {
                agent_first_data::output_yaml(&value)
            } else {
                agent_first_data::output_plain(&value)
            }
        }
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let _ = out.write_all(formatted.as_bytes());
    if !formatted.ends_with('\n') {
        let _ = out.write_all(b"\n");
    }
    let _ = out.flush();
}

/// Protect server-originated body fields from AFD suffix processing.
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
            eprintln!("error: invalid header '{s}': expected 'Name: Value'");
            std::process::exit(2);
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
        eprintln!(
            "error: --body, --body-base64, --body-file, --body-multipart, and --body-urlencoded are mutually exclusive"
        );
        std::process::exit(2);
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
            eprintln!("error: invalid --body-multipart '{s}': expected name=value");
            std::process::exit(2);
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
            eprintln!("error: invalid --body-urlencoded '{s}': expected name=value");
            std::process::exit(2);
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
