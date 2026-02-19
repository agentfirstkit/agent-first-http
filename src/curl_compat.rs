use crate::cli::{CliRequest, Mode, OutputFormat};
use crate::types::*;
/// curl compatibility mode: parse a subset of curl command-line flags and
/// return a `Mode::Cli(...)` equivalent to what afh would produce natively.
///
/// Supported flags: see docs/cli.md for the complete table.
use base64::Engine;
use serde_json::Value;
use std::collections::HashMap;

pub fn parse_curl_args(args: &[String]) -> Mode {
    let mut method: Option<String> = None;
    let mut url: Option<String> = None;
    let mut headers: Vec<(String, String)> = Vec::new();
    // -d parts (JSON auto-detect)
    let mut data_parts: Vec<String> = Vec::new();
    // --data-raw (always text)
    let mut data_raw_parts: Vec<String> = Vec::new();
    let mut data_urlencode: Vec<UrlencodedPart> = Vec::new();
    let mut form_parts: Vec<MultipartPart> = Vec::new();
    let mut response_save_file: Option<String> = None;
    let mut response_save_file_is_basename = false;
    let mut response_save_resume = false;
    let mut tls_insecure = false;
    let mut tls_cacert_file: Option<String> = None;
    let mut tls_cert_file: Option<String> = None;
    let mut tls_key_file: Option<String> = None;
    let mut proxy: Option<String> = None;
    let mut retry: Option<u32> = None;
    let mut timeout_connect_s: Option<u64> = None;
    let mut timeout_idle_s: Option<u64> = None;
    let mut verbose = false;
    let mut chunked = false;
    let mut head_mode = false;
    let mut upload_file: Option<String> = None;
    let mut response_redirect: Option<u32> = None;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];

        // Positional argument: first non-flag becomes the URL
        if !arg.starts_with('-') || arg == "-" {
            if url.is_none() && arg != "-" {
                url = Some(arg.clone());
            }
            i += 1;
            continue;
        }

        // Long flags (--flag or --flag=value)
        if let Some(rest) = arg.strip_prefix("--") {
            // Split on '=' for --flag=value syntax
            let (flag, inline_val) = match rest.find('=') {
                Some(pos) => (&rest[..pos], Some(&rest[pos + 1..])),
                None => (rest, None),
            };

            // Helper: get next arg or use inline value
            macro_rules! next_val {
                () => {
                    if let Some(v) = inline_val {
                        i += 1;
                        Some(v.to_string())
                    } else if i + 1 < args.len() {
                        i += 1;
                        Some(args[i].clone())
                    } else {
                        None
                    }
                };
            }

            match flag {
                "request" => {
                    if let Some(v) = next_val!() {
                        method = Some(v.to_uppercase());
                    }
                }
                "header" => {
                    if let Some(v) = next_val!() {
                        push_header(&v, &mut headers);
                    }
                }
                "data" | "data-ascii" => {
                    if let Some(v) = next_val!() {
                        data_parts.push(v);
                    }
                }
                "data-raw" => {
                    if let Some(v) = next_val!() {
                        data_raw_parts.push(v);
                    }
                }
                "data-urlencode" => {
                    if let Some(v) = next_val!() {
                        push_urlencode_part(&v, &mut data_urlencode);
                    }
                }
                "form" => {
                    if let Some(v) = next_val!() {
                        push_form_part(&v, &mut form_parts);
                    }
                }
                "output" => {
                    if let Some(v) = next_val!() {
                        response_save_file = Some(v);
                    }
                }
                "remote-name" => {
                    response_save_file_is_basename = true;
                }
                "location" => {
                    if response_redirect.is_none() {
                        response_redirect = Some(10);
                    }
                }
                "max-redirs" => {
                    if let Some(v) = next_val!() {
                        response_redirect = v.parse().ok();
                    }
                }
                "head" => {
                    head_mode = true;
                }
                "insecure" => {
                    tls_insecure = true;
                }
                "cacert" => {
                    if let Some(v) = next_val!() {
                        tls_cacert_file = Some(v);
                    }
                }
                "cert" => {
                    if let Some(v) = next_val!() {
                        tls_cert_file = Some(v);
                    }
                }
                "key" => {
                    if let Some(v) = next_val!() {
                        tls_key_file = Some(v);
                    }
                }
                "proxy" => {
                    if let Some(v) = next_val!() {
                        proxy = Some(v);
                    }
                }
                "retry" => {
                    if let Some(v) = next_val!() {
                        retry = v.parse().ok();
                    }
                }
                "connect-timeout" => {
                    if let Some(v) = next_val!() {
                        // curl accepts float; truncate to integer seconds
                        timeout_connect_s = v.parse::<f64>().ok().map(|f| f as u64);
                    }
                }
                "max-time" => {
                    if let Some(v) = next_val!() {
                        timeout_idle_s = v.parse::<f64>().ok().map(|f| f as u64);
                    }
                }
                "user-agent" => {
                    if let Some(v) = next_val!() {
                        headers.push(("User-Agent".to_string(), v));
                    }
                }
                "user" => {
                    if let Some(v) = next_val!() {
                        push_basic_auth(&v, &mut headers);
                    }
                }
                "cookie" => {
                    if let Some(v) = next_val!() {
                        headers.push(("Cookie".to_string(), v));
                    }
                }
                "referer" => {
                    if let Some(v) = next_val!() {
                        headers.push(("Referer".to_string(), v));
                    }
                }
                "upload-file" => {
                    if let Some(v) = next_val!() {
                        upload_file = Some(v);
                    }
                }
                "no-buffer" => {
                    chunked = true;
                }
                "verbose" => {
                    verbose = true;
                }
                // Intentional no-ops: afh already behaves like these flags
                "silent" | "compressed" | "fail" | "fail-with-body" | "show-error" | "globoff"
                | "disable-eprt" | "ipv4" | "ipv6" => {}
                "continue-at" => {
                    if let Some(v) = next_val!() {
                        if v == "-" {
                            response_save_resume = true;
                        }
                    }
                }
                _ => {
                    // Consume a value if the flag looks like it takes one
                    // (heuristic: skip it so the parser stays in sync)
                    if inline_val.is_none() {
                        // Unknown flags: consume next arg only for known value-taking patterns
                        // We don't consume blindly to avoid eating a URL
                    }
                }
            }
            i += 1;
            continue;
        }

        // Short flags: arg is "-X", "-XMethod", "-vk", etc.
        let chars = &arg[1..]; // strip leading '-'
        if chars.is_empty() {
            i += 1;
            continue;
        }

        // Process short flags character by character (bundled flags like -vk)
        let mut j = 0;
        let char_bytes: Vec<char> = chars.chars().collect();
        while j < char_bytes.len() {
            let c = char_bytes[j];
            // Check if the rest of the arg after this char provides a value
            let rest_of_arg: String = char_bytes[j + 1..].iter().collect();

            match c {
                'X' => {
                    if !rest_of_arg.is_empty() {
                        method = Some(rest_of_arg.to_uppercase());
                        j = char_bytes.len(); // consumed rest of arg
                    } else {
                        i += 1;
                        if i < args.len() {
                            method = Some(args[i].to_uppercase());
                        }
                        j = char_bytes.len();
                    }
                }
                'H' => {
                    if !rest_of_arg.is_empty() {
                        push_header(&rest_of_arg, &mut headers);
                        j = char_bytes.len();
                    } else {
                        i += 1;
                        if i < args.len() {
                            push_header(&args[i], &mut headers);
                        }
                        j = char_bytes.len();
                    }
                }
                'd' => {
                    if !rest_of_arg.is_empty() {
                        data_parts.push(rest_of_arg.clone());
                        j = char_bytes.len();
                    } else {
                        i += 1;
                        if i < args.len() {
                            data_parts.push(args[i].clone());
                        }
                        j = char_bytes.len();
                    }
                }
                'F' => {
                    if !rest_of_arg.is_empty() {
                        push_form_part(&rest_of_arg, &mut form_parts);
                        j = char_bytes.len();
                    } else {
                        i += 1;
                        if i < args.len() {
                            push_form_part(&args[i], &mut form_parts);
                        }
                        j = char_bytes.len();
                    }
                }
                'o' => {
                    if !rest_of_arg.is_empty() {
                        response_save_file = Some(rest_of_arg.clone());
                        j = char_bytes.len();
                    } else {
                        i += 1;
                        if i < args.len() {
                            response_save_file = Some(args[i].clone());
                        }
                        j = char_bytes.len();
                    }
                }
                'O' => {
                    response_save_file_is_basename = true;
                    j += 1;
                }
                'L' => {
                    if response_redirect.is_none() {
                        response_redirect = Some(10);
                    }
                    j += 1;
                }
                'k' => {
                    tls_insecure = true;
                    j += 1;
                }
                'x' => {
                    if !rest_of_arg.is_empty() {
                        proxy = Some(rest_of_arg.clone());
                        j = char_bytes.len();
                    } else {
                        i += 1;
                        if i < args.len() {
                            proxy = Some(args[i].clone());
                        }
                        j = char_bytes.len();
                    }
                }
                'A' => {
                    if !rest_of_arg.is_empty() {
                        headers.push(("User-Agent".to_string(), rest_of_arg.clone()));
                        j = char_bytes.len();
                    } else {
                        i += 1;
                        if i < args.len() {
                            headers.push(("User-Agent".to_string(), args[i].clone()));
                        }
                        j = char_bytes.len();
                    }
                }
                'u' => {
                    if !rest_of_arg.is_empty() {
                        push_basic_auth(&rest_of_arg, &mut headers);
                        j = char_bytes.len();
                    } else {
                        i += 1;
                        if i < args.len() {
                            push_basic_auth(&args[i], &mut headers);
                        }
                        j = char_bytes.len();
                    }
                }
                'I' => {
                    head_mode = true;
                    j += 1;
                }
                'N' => {
                    chunked = true;
                    j += 1;
                }
                'v' => {
                    verbose = true;
                    j += 1;
                }
                's' => {
                    // silent — no-op
                    j += 1;
                }
                'b' => {
                    if !rest_of_arg.is_empty() {
                        headers.push(("Cookie".to_string(), rest_of_arg.clone()));
                        j = char_bytes.len();
                    } else {
                        i += 1;
                        if i < args.len() {
                            headers.push(("Cookie".to_string(), args[i].clone()));
                        }
                        j = char_bytes.len();
                    }
                }
                'e' => {
                    if !rest_of_arg.is_empty() {
                        headers.push(("Referer".to_string(), rest_of_arg.clone()));
                        j = char_bytes.len();
                    } else {
                        i += 1;
                        if i < args.len() {
                            headers.push(("Referer".to_string(), args[i].clone()));
                        }
                        j = char_bytes.len();
                    }
                }
                'T' => {
                    if !rest_of_arg.is_empty() {
                        upload_file = Some(rest_of_arg.clone());
                        j = char_bytes.len();
                    } else {
                        i += 1;
                        if i < args.len() {
                            upload_file = Some(args[i].clone());
                        }
                        j = char_bytes.len();
                    }
                }
                'C' => {
                    if rest_of_arg == "-" {
                        response_save_resume = true;
                        j = char_bytes.len();
                    } else {
                        i += 1;
                        if i < args.len() && args[i] == "-" {
                            response_save_resume = true;
                        }
                        j = char_bytes.len();
                    }
                }
                _ => {
                    j += 1;
                }
            }
        }

        i += 1;
    }

    // Resolve -O: save to URL basename
    if response_save_file_is_basename {
        let basename = url
            .as_deref()
            .and_then(remote_name_from_url)
            .unwrap_or_else(|| "output".to_string());
        response_save_file = Some(basename);
    }

    // Determine method
    let has_body_data = !data_parts.is_empty()
        || !data_raw_parts.is_empty()
        || !data_urlencode.is_empty()
        || !form_parts.is_empty();

    let method = if head_mode {
        "HEAD".to_string()
    } else if let Some(m) = method {
        m
    } else if upload_file.is_some() {
        // -T defaults to PUT
        "PUT".to_string()
    } else if has_body_data {
        "POST".to_string()
    } else {
        "GET".to_string()
    };

    // Build headers map
    let mut headers_map: HashMap<String, Value> = HashMap::new();
    for (k, v) in headers {
        headers_map.insert(k, Value::String(v));
    }

    // Build body
    let (body, body_base64, body_file, body_multipart, body_urlencoded) =
        if let Some(path) = upload_file {
            (None, None, Some(path), None, None)
        } else if !form_parts.is_empty() {
            (None, None, None, Some(form_parts), None)
        } else if !data_urlencode.is_empty() {
            (None, None, None, None, Some(data_urlencode))
        } else if !data_raw_parts.is_empty() {
            // --data-raw: always text, no JSON detection
            let combined = data_raw_parts.join("&");
            (Some(Value::String(combined)), None, None, None, None)
        } else if !data_parts.is_empty() {
            // -d: concatenate with &, then JSON auto-detect (object/array only)
            let combined = data_parts.join("&");
            let body_val = match serde_json::from_str::<Value>(combined.trim()) {
                Ok(v) if v.is_object() || v.is_array() => v,
                _ => Value::String(combined),
            };
            (Some(body_val), None, None, None, None)
        } else {
            (None, None, None, None, None)
        };

    // Build log categories from --verbose
    const ALL_CATEGORIES: &[&str] = &["startup", "request", "progress", "retry", "redirect"];
    let log_categories: Vec<String> = if verbose {
        ALL_CATEGORIES.iter().map(|s| s.to_string()).collect()
    } else {
        vec![]
    };

    // TLS goes into per-request options (one-shot mode, like native CLI mode)
    let tls = if tls_insecure
        || tls_cacert_file.is_some()
        || tls_cert_file.is_some()
        || tls_key_file.is_some()
    {
        Some(TlsConfigPartial {
            insecure: if tls_insecure { Some(true) } else { None },
            cacert_file: tls_cacert_file,
            cert_file: tls_cert_file,
            key_file: tls_key_file,
            ..TlsConfigPartial::default()
        })
    } else {
        None
    };

    let config_overrides = ConfigPatch {
        proxy,
        timeout_connect_s,
        ..ConfigPatch::default()
    };

    let chunked_delimiter = Value::String("\n".to_string());

    let options = RequestOptions {
        timeout_idle_s,
        retry,
        response_redirect,
        response_save_file,
        response_save_resume: if response_save_resume {
            Some(true)
        } else {
            None
        },
        chunked,
        chunked_delimiter,
        tls,
        ..RequestOptions::default()
    };

    Mode::Cli(Box::new(CliRequest {
        method,
        url: url.unwrap_or_default(),
        headers: headers_map,
        body,
        body_base64,
        body_file,
        body_multipart,
        body_urlencoded,
        options,
        config_overrides,
        log_categories,
        output_format: OutputFormat::Json,
    }))
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn remote_name_from_url(raw: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(raw).ok()?;
    parsed
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

fn push_header(s: &str, headers: &mut Vec<(String, String)>) {
    match s.find(':') {
        Some(pos) => {
            let name = s[..pos].trim().to_string();
            let value = s[pos + 1..].trim().to_string();
            headers.push((name, value));
        }
        None => {
            // curl treats "HeaderName;" as a header with empty value (removal signal)
            // We ignore malformed headers silently
        }
    }
}

fn push_basic_auth(s: &str, headers: &mut Vec<(String, String)>) {
    let encoded = base64::engine::general_purpose::STANDARD.encode(s.as_bytes());
    headers.push(("Authorization".to_string(), format!("Basic {encoded}")));
}

fn push_urlencode_part(s: &str, parts: &mut Vec<UrlencodedPart>) {
    // Supported formats: name=value, name@file (read from file, ignored here), name
    match s.find('=') {
        Some(pos) => parts.push(UrlencodedPart {
            name: s[..pos].to_string(),
            value: s[pos + 1..].to_string(),
        }),
        None => {
            // name without value → empty value
            parts.push(UrlencodedPart {
                name: s.to_string(),
                value: String::new(),
            });
        }
    }
}

fn push_form_part(s: &str, parts: &mut Vec<MultipartPart>) {
    match s.find('=') {
        Some(pos) => {
            let name = s[..pos].to_string();
            let rest = &s[pos + 1..];
            if let Some(file_rest) = rest.strip_prefix('@') {
                // File part: name=@path[;filename=x][;type=mime]
                let segments: Vec<&str> = file_rest.splitn(2, ';').collect();
                let file = segments[0].to_string();
                let mut filename = None;
                let mut content_type = None;
                if let Some(meta) = segments.get(1) {
                    for part in meta.split(';') {
                        if let Some(f) = part.strip_prefix("filename=") {
                            filename = Some(f.to_string());
                        } else if let Some(t) = part.strip_prefix("type=") {
                            content_type = Some(t.to_string());
                        }
                    }
                }
                parts.push(MultipartPart {
                    name,
                    value: None,
                    value_base64: None,
                    file: Some(file),
                    filename,
                    content_type,
                });
            } else {
                parts.push(MultipartPart {
                    name,
                    value: Some(rest.to_string()),
                    value_base64: None,
                    file: None,
                    filename: None,
                    content_type: None,
                });
            }
        }
        None => {
            // name without = is a text part with empty value
            parts.push(MultipartPart {
                name: s.to_string(),
                value: Some(String::new()),
                value_base64: None,
                file: None,
                filename: None,
                content_type: None,
            });
        }
    }
}
