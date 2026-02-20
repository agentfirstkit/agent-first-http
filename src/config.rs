use crate::types::*;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

impl RuntimeConfig {
    pub fn new(download_dir: String) -> Self {
        let mut headers = HashMap::new();
        headers.insert(
            "User-Agent".to_string(),
            Value::String(format!("afhttp/{VERSION}")),
        );
        RuntimeConfig {
            response_save_dir: download_dir,
            response_save_above_bytes: 10_485_760, // 10 MiB
            request_concurrency_limit: 0,          // 0 = unlimited
            timeout_connect_s: 10,
            pool_idle_timeout_s: 90,
            retry_base_delay_ms: 100,
            proxy: None,
            tls: TlsConfig {
                insecure: false,
                cacert_pem: None,
                cacert_file: None,
                cert_pem: None,
                cert_file: None,
                key_pem_secret: None,
                key_file: None,
            },
            log: vec![],
            defaults: RequestDefaults {
                headers,
                timeout_idle_s: 30,
                retry: 0,
                response_redirect: 10,
                response_parse_json: true,
                response_decompress: true,
                response_save_resume: false,
                retry_on_status: vec![],
            },
            host_defaults: HashMap::new(),
        }
    }

    /// Apply a config patch. Returns true if the reqwest::Client needs to be rebuilt.
    pub fn apply_update(&mut self, patch: ConfigPatch) -> bool {
        let mut needs_rebuild = false;

        if let Some(v) = patch.response_save_dir {
            self.response_save_dir = v;
        }
        if let Some(v) = patch.response_save_above_bytes {
            self.response_save_above_bytes = v;
        }
        if let Some(v) = patch.request_concurrency_limit {
            self.request_concurrency_limit = v;
        }
        if let Some(v) = patch.timeout_connect_s {
            if v != self.timeout_connect_s {
                needs_rebuild = true;
            }
            self.timeout_connect_s = v;
        }
        if let Some(v) = patch.pool_idle_timeout_s {
            if v != self.pool_idle_timeout_s {
                needs_rebuild = true;
            }
            self.pool_idle_timeout_s = v;
        }
        if let Some(v) = patch.retry_base_delay_ms {
            self.retry_base_delay_ms = v;
        }
        if let Some(v) = patch.proxy {
            if Some(&v) != self.proxy.as_ref() {
                needs_rebuild = true;
            }
            self.proxy = Some(v);
        }

        if let Some(tls_patch) = patch.tls {
            if let Some(v) = tls_patch.insecure {
                if v != self.tls.insecure {
                    needs_rebuild = true;
                }
                self.tls.insecure = v;
            }
            // Inline and file-path are mutually exclusive per slot.
            // Setting one clears the other so the stored config stays consistent.
            if let Some(v) = tls_patch.cacert_pem {
                needs_rebuild = true;
                self.tls.cacert_pem = Some(v);
                self.tls.cacert_file = None;
            } else if let Some(v) = tls_patch.cacert_file {
                needs_rebuild = true;
                self.tls.cacert_file = Some(v);
                self.tls.cacert_pem = None;
            }
            if let Some(v) = tls_patch.cert_pem {
                needs_rebuild = true;
                self.tls.cert_pem = Some(v);
                self.tls.cert_file = None;
            } else if let Some(v) = tls_patch.cert_file {
                needs_rebuild = true;
                self.tls.cert_file = Some(v);
                self.tls.cert_pem = None;
            }
            if let Some(v) = tls_patch.key_pem_secret {
                needs_rebuild = true;
                self.tls.key_pem_secret = Some(v);
                self.tls.key_file = None;
            } else if let Some(v) = tls_patch.key_file {
                needs_rebuild = true;
                self.tls.key_file = Some(v);
                self.tls.key_pem_secret = None;
            }
        }

        if let Some(v) = patch.log {
            self.log = v;
        }

        if let Some(d) = patch.defaults {
            // Deep merge headers: key-by-key, null removes
            if let Some(new_headers) = d.headers {
                for (k, v) in new_headers {
                    if v.is_null() {
                        self.defaults.headers.remove(&k);
                    } else {
                        self.defaults.headers.insert(k, v);
                    }
                }
            }
            if let Some(v) = d.timeout_idle_s {
                self.defaults.timeout_idle_s = v;
            }
            if let Some(v) = d.retry {
                self.defaults.retry = v;
            }
            if let Some(v) = d.response_redirect {
                self.defaults.response_redirect = v;
            }
            if let Some(v) = d.response_parse_json {
                self.defaults.response_parse_json = v;
            }
            if let Some(v) = d.response_decompress {
                self.defaults.response_decompress = v;
            }
            if let Some(v) = d.response_save_resume {
                self.defaults.response_save_resume = v;
            }
            if let Some(v) = d.retry_on_status {
                self.defaults.retry_on_status = v;
            }
        }

        // Deep merge host_defaults: per-host, headers key-by-key
        if let Some(hd) = patch.host_defaults {
            for (host, partial) in hd {
                let entry = self.host_defaults.entry(host).or_default();
                if let Some(new_headers) = partial.headers {
                    for (k, v) in new_headers {
                        if v.is_null() {
                            entry.headers.remove(&k);
                        } else {
                            entry.headers.insert(k, v);
                        }
                    }
                }
            }
        }

        needs_rebuild
    }

    /// Build the shared reqwest::Client from the current config.
    pub fn build_client(&self) -> Result<reqwest::Client, String> {
        build_client_inner(self, None)
    }

    /// Build a one-off reqwest::Client with per-request TLS overrides applied on
    /// top of the current global config. Used when `options.tls` is provided.
    pub fn build_client_for_request(
        &self,
        tls_override: &TlsConfigPartial,
    ) -> Result<reqwest::Client, String> {
        build_client_inner(self, Some(tls_override))
    }

    /// Resolve per-request options by merging config defaults with request overrides.
    pub fn resolve(&self, options: &RequestOptions) -> ResolvedOptions {
        let chunked_delimiter = if options.chunked {
            match &options.chunked_delimiter {
                Value::String(s) => Some(s.clone()),
                Value::Null => None, // raw mode
                _ => Some("\n".to_string()),
            }
        } else {
            None
        };

        ResolvedOptions {
            timeout_idle_s: options
                .timeout_idle_s
                .unwrap_or(self.defaults.timeout_idle_s),
            retry: options.retry.unwrap_or(self.defaults.retry),
            response_redirect: options
                .response_redirect
                .unwrap_or(self.defaults.response_redirect),
            response_parse_json: options
                .response_parse_json
                .unwrap_or(self.defaults.response_parse_json),
            response_decompress: options
                .response_decompress
                .unwrap_or(self.defaults.response_decompress),
            response_save_resume: options
                .response_save_resume
                .unwrap_or(self.defaults.response_save_resume),
            chunked: options.chunked,
            chunked_delimiter,
            response_save_file: options.response_save_file.clone(),
            progress_bytes: options.progress_bytes.unwrap_or(0),
            progress_ms: options.progress_ms.unwrap_or(10000),
            response_save_above_bytes: self.response_save_above_bytes,
            retry_base_delay_ms: self.retry_base_delay_ms,
            retry_on_status: options
                .retry_on_status
                .clone()
                .unwrap_or_else(|| self.defaults.retry_on_status.clone()),
            response_max_bytes: options.response_max_bytes,
        }
    }

    /// Merge config default headers + host-specific headers + per-request headers.
    /// Merge order: defaults → host_defaults[host] → request headers. Null removes.
    pub fn merged_headers(
        &self,
        request_headers: &HashMap<String, Value>,
        host: Option<&str>,
    ) -> Result<HeaderMap, String> {
        let mut merged: HashMap<String, Value> = self.defaults.headers.clone();

        // Layer 2: host-specific defaults
        if let Some(host) = host {
            if let Some(hd) = self.host_defaults.get(host) {
                for (k, v) in &hd.headers {
                    if v.is_null() {
                        merged.remove(k);
                    } else {
                        merged.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        // Layer 3: per-request overrides
        for (k, v) in request_headers {
            if v.is_null() {
                merged.remove(k);
            } else {
                merged.insert(k.clone(), v.clone());
            }
        }

        let mut header_map = HeaderMap::new();
        for (k, v) in &merged {
            let name = HeaderName::from_bytes(k.as_bytes())
                .map_err(|e| format!("invalid header name '{k}': {e}"))?;
            let val_str = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let value = HeaderValue::from_str(&val_str)
                .map_err(|e| format!("invalid header value for '{k}': {e}"))?;
            header_map.insert(name, value);
        }
        Ok(header_map)
    }
}

// ---------------------------------------------------------------------------
// Internal client builder
// ---------------------------------------------------------------------------

/// Resolve PEM bytes from either an inline string or a file path.
/// Inline takes precedence. Returns None if neither is provided.
fn load_pem(
    inline: Option<&String>,
    file_path: Option<&String>,
) -> Result<Option<Vec<u8>>, String> {
    if let Some(s) = inline {
        return Ok(Some(s.as_bytes().to_vec()));
    }
    if let Some(path) = file_path {
        let bytes = std::fs::read(path).map_err(|e| format!("read '{path}': {e}"))?;
        return Ok(Some(bytes));
    }
    Ok(None)
}

/// Build a reqwest::Client using the global config and an optional per-request
/// TLS override. When `tls_override` is Some, the per-request TLS fields take
/// precedence over the global TLS config for the affected slots.
fn build_client_inner(
    cfg: &RuntimeConfig,
    tls_override: Option<&TlsConfigPartial>,
) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(cfg.timeout_connect_s))
        .pool_idle_timeout(Duration::from_secs(cfg.pool_idle_timeout_s))
        .pool_max_idle_per_host(10)
        // We handle redirects manually to track redirect count
        .redirect(reqwest::redirect::Policy::none());

    // ── insecure ──
    let insecure = tls_override
        .and_then(|o| o.insecure)
        .unwrap_or(cfg.tls.insecure);
    if insecure {
        builder = builder.danger_accept_invalid_certs(true);
    }

    // ── CA certificate ──
    // Per-request overrides global when any cacert field is present in the override.
    let ca_pem = if let Some(ov) = tls_override {
        if ov.cacert_pem.is_some() || ov.cacert_file.is_some() {
            load_pem(ov.cacert_pem.as_ref(), ov.cacert_file.as_ref())?
        } else {
            load_pem(cfg.tls.cacert_pem.as_ref(), cfg.tls.cacert_file.as_ref())?
        }
    } else {
        load_pem(cfg.tls.cacert_pem.as_ref(), cfg.tls.cacert_file.as_ref())?
    };
    if let Some(pem) = ca_pem {
        let cert =
            reqwest::Certificate::from_pem(&pem).map_err(|e| format!("parse cacert: {e}"))?;
        builder = builder.add_root_certificate(cert);
    }

    // ── Client certificate + key ──
    let cert_pem = if let Some(ov) = tls_override {
        if ov.cert_pem.is_some() || ov.cert_file.is_some() {
            load_pem(ov.cert_pem.as_ref(), ov.cert_file.as_ref())?
        } else {
            load_pem(cfg.tls.cert_pem.as_ref(), cfg.tls.cert_file.as_ref())?
        }
    } else {
        load_pem(cfg.tls.cert_pem.as_ref(), cfg.tls.cert_file.as_ref())?
    };
    let key_pem_secret = if let Some(ov) = tls_override {
        if ov.key_pem_secret.is_some() || ov.key_file.is_some() {
            load_pem(ov.key_pem_secret.as_ref(), ov.key_file.as_ref())?
        } else {
            load_pem(cfg.tls.key_pem_secret.as_ref(), cfg.tls.key_file.as_ref())?
        }
    } else {
        load_pem(cfg.tls.key_pem_secret.as_ref(), cfg.tls.key_file.as_ref())?
    };

    if let Some(cert_bytes) = cert_pem {
        // Build a PEM bundle: cert + key (key may be in the same file as cert)
        let mut bundle = cert_bytes.clone();
        bundle.push(b'\n');
        if let Some(key_bytes) = key_pem_secret {
            bundle.extend_from_slice(&key_bytes);
        } else {
            // Key expected to be in the same file as the certificate
            bundle.extend_from_slice(&cert_bytes);
        }
        let identity = reqwest::Identity::from_pem(&bundle)
            .map_err(|e| format!("parse client identity: {e}"))?;
        builder = builder.identity(identity);
    }

    // ── Proxy ──
    if let Some(ref proxy_url) = cfg.proxy {
        let proxy = reqwest::Proxy::all(proxy_url).map_err(|e| format!("invalid proxy: {e}"))?;
        builder = builder.proxy(proxy);
    }

    builder.build().map_err(|e| format!("build client: {e}"))
}

// ---------------------------------------------------------------------------
// Response header helpers
// ---------------------------------------------------------------------------

/// Convert HTTP response headers to HashMap<String, Value>.
/// Keys are always lowercase. Returns an error if the server sent a header
/// value containing non-ASCII bytes — that is a server-side protocol violation.
/// Single value → string, multiple values → array.
pub fn response_headers_to_map(
    headers: &reqwest::header::HeaderMap,
) -> Result<HashMap<String, Value>, String> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for (name, value) in headers.iter() {
        let key = name.as_str().to_string();
        let val = value
            .to_str()
            .map_err(|_| format!("server sent non-ASCII bytes in header '{key}'"))?;
        map.entry(key).or_default().push(val.to_string());
    }
    Ok(map
        .into_iter()
        .map(|(k, mut v)| {
            if v.len() == 1 {
                (k, Value::String(v.swap_remove(0)))
            } else {
                (k, Value::Array(v.into_iter().map(Value::String).collect()))
            }
        })
        .collect())
}

/// Parse Content-Length header value from response headers map.
pub fn parse_content_length(headers: &HashMap<String, Value>) -> Option<u64> {
    headers
        .get("content-length")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderValue, CONTENT_LENGTH, SET_COOKIE};

    fn tmp_file_path(name: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir()
            .join(format!("afhttp-{name}-{nanos}.tmp"))
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn runtime_config_new_has_defaults() {
        let cfg = RuntimeConfig::new("/tmp/afhttp-test".to_string());
        assert_eq!(cfg.response_save_dir, "/tmp/afhttp-test");
        assert_eq!(
            cfg.defaults.headers.get("User-Agent"),
            Some(&Value::String(format!("afhttp/{VERSION}")))
        );
        assert_eq!(cfg.defaults.timeout_idle_s, 30);
        assert!(cfg.host_defaults.is_empty());
    }

    #[test]
    fn apply_update_merges_and_marks_rebuild() {
        let mut cfg = RuntimeConfig::new("/tmp/afhttp-test".to_string());
        let mut defaults_headers = HashMap::new();
        defaults_headers.insert("X-One".to_string(), Value::String("1".to_string()));
        defaults_headers.insert("User-Agent".to_string(), Value::Null);
        let mut host_defaults = HashMap::new();
        host_defaults.insert(
            "example.com".to_string(),
            HostDefaultsPartial {
                headers: Some(
                    [("X-Host".to_string(), Value::String("yes".to_string()))]
                        .into_iter()
                        .collect(),
                ),
            },
        );

        let patch = ConfigPatch {
            timeout_connect_s: Some(11),
            pool_idle_timeout_s: Some(22),
            proxy: Some("http://127.0.0.1:8080".to_string()),
            defaults: Some(RequestDefaultsPartial {
                headers: Some(defaults_headers),
                timeout_idle_s: Some(9),
                retry_on_status: Some(vec![429, 503]),
                ..RequestDefaultsPartial::default()
            }),
            host_defaults: Some(host_defaults),
            tls: Some(TlsConfigPartial {
                insecure: Some(true),
                cacert_file: Some("/tmp/ca.pem".to_string()),
                cert_file: Some("/tmp/cert.pem".to_string()),
                key_file: Some("/tmp/key.pem".to_string()),
                ..TlsConfigPartial::default()
            }),
            ..ConfigPatch::default()
        };
        let needs_rebuild = cfg.apply_update(patch);
        assert!(needs_rebuild);
        assert_eq!(cfg.timeout_connect_s, 11);
        assert_eq!(cfg.pool_idle_timeout_s, 22);
        assert_eq!(cfg.proxy.as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(cfg.defaults.timeout_idle_s, 9);
        assert_eq!(cfg.defaults.retry_on_status, vec![429, 503]);
        assert_eq!(
            cfg.defaults.headers.get("X-One"),
            Some(&Value::String("1".into()))
        );
        assert!(!cfg.defaults.headers.contains_key("User-Agent"));
        assert_eq!(
            cfg.host_defaults
                .get("example.com")
                .and_then(|h| h.headers.get("X-Host")),
            Some(&Value::String("yes".into()))
        );
        assert!(cfg.tls.insecure);
        assert_eq!(cfg.tls.cacert_file.as_deref(), Some("/tmp/ca.pem"));
        assert_eq!(cfg.tls.cert_file.as_deref(), Some("/tmp/cert.pem"));
        assert_eq!(cfg.tls.key_file.as_deref(), Some("/tmp/key.pem"));
    }

    #[test]
    fn apply_update_inline_tls_clears_file_variants() {
        let mut cfg = RuntimeConfig::new("/tmp/afhttp-test".to_string());
        cfg.tls.cacert_file = Some("a".to_string());
        cfg.tls.cert_file = Some("b".to_string());
        cfg.tls.key_file = Some("c".to_string());

        let _ = cfg.apply_update(ConfigPatch {
            tls: Some(TlsConfigPartial {
                cacert_pem: Some("CA".to_string()),
                cert_pem: Some("CERT".to_string()),
                key_pem_secret: Some("KEY".to_string()),
                ..TlsConfigPartial::default()
            }),
            ..ConfigPatch::default()
        });
        assert_eq!(cfg.tls.cacert_pem.as_deref(), Some("CA"));
        assert!(cfg.tls.cacert_file.is_none());
        assert_eq!(cfg.tls.cert_pem.as_deref(), Some("CERT"));
        assert!(cfg.tls.cert_file.is_none());
        assert_eq!(cfg.tls.key_pem_secret.as_deref(), Some("KEY"));
        assert!(cfg.tls.key_file.is_none());
    }

    #[test]
    fn resolve_merges_defaults_and_request_options() {
        let mut cfg = RuntimeConfig::new("/tmp/afhttp-test".to_string());
        cfg.defaults.timeout_idle_s = 31;
        cfg.defaults.retry = 2;
        cfg.defaults.response_redirect = 7;
        cfg.defaults.response_parse_json = false;
        cfg.defaults.response_decompress = false;
        cfg.defaults.response_save_resume = true;
        cfg.defaults.retry_on_status = vec![500];
        cfg.response_save_above_bytes = 123;
        cfg.retry_base_delay_ms = 456;

        let opts = RequestOptions {
            chunked: true,
            chunked_delimiter: Value::Null,
            progress_bytes: Some(5),
            progress_ms: Some(6),
            response_max_bytes: Some(7),
            ..RequestOptions::default()
        };
        let resolved = cfg.resolve(&opts);
        assert_eq!(resolved.timeout_idle_s, 31);
        assert_eq!(resolved.retry, 2);
        assert_eq!(resolved.response_redirect, 7);
        assert!(!resolved.response_parse_json);
        assert!(!resolved.response_decompress);
        assert!(resolved.response_save_resume);
        assert!(resolved.chunked);
        assert!(resolved.chunked_delimiter.is_none());
        assert_eq!(resolved.progress_bytes, 5);
        assert_eq!(resolved.progress_ms, 6);
        assert_eq!(resolved.response_save_above_bytes, 123);
        assert_eq!(resolved.retry_base_delay_ms, 456);
        assert_eq!(resolved.retry_on_status, vec![500]);
        assert_eq!(resolved.response_max_bytes, Some(7));
    }

    #[test]
    fn merged_headers_applies_layers_and_null_removal() {
        let mut cfg = RuntimeConfig::new("/tmp/afhttp-test".to_string());
        cfg.defaults.headers.insert(
            "X-Default".to_string(),
            Value::String("default".to_string()),
        );
        cfg.host_defaults.insert(
            "api.example.com".to_string(),
            HostDefaults {
                headers: [
                    ("X-Host".to_string(), Value::String("host".to_string())),
                    ("X-Default".to_string(), Value::Null),
                ]
                .into_iter()
                .collect(),
            },
        );
        let req_headers: HashMap<String, Value> = [
            ("X-Req".to_string(), Value::String("req".to_string())),
            ("X-Host".to_string(), Value::Null),
        ]
        .into_iter()
        .collect();
        let merged = cfg
            .merged_headers(&req_headers, Some("api.example.com"))
            .expect("merged headers");
        assert_eq!(
            merged.get("x-req").and_then(|v| v.to_str().ok()),
            Some("req")
        );
        assert!(merged.get("x-host").is_none());
        assert!(merged.get("x-default").is_none());
    }

    #[test]
    fn merged_headers_rejects_invalid_names_or_values() {
        let cfg = RuntimeConfig::new("/tmp/afhttp-test".to_string());
        let bad_name: HashMap<String, Value> =
            [("bad name".to_string(), Value::String("x".into()))]
                .into_iter()
                .collect();
        assert!(cfg.merged_headers(&bad_name, None).is_err());

        let bad_value: HashMap<String, Value> =
            [("X".to_string(), Value::String("bad\nvalue".into()))]
                .into_iter()
                .collect();
        assert!(cfg.merged_headers(&bad_value, None).is_err());
    }

    #[test]
    fn load_pem_prefers_inline_then_file() {
        let file = tmp_file_path("pem");
        std::fs::write(&file, b"FILE").expect("write");
        let inline = "INLINE".to_string();
        let from_inline = load_pem(Some(&inline), Some(&file)).expect("inline pem");
        assert_eq!(from_inline, Some(b"INLINE".to_vec()));
        let from_file = load_pem(None, Some(&file)).expect("file pem");
        assert_eq!(from_file, Some(b"FILE".to_vec()));
        let none = load_pem(None, None).expect("none");
        assert_eq!(none, None);
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn build_client_basics_and_bad_cert_error() {
        let mut cfg = RuntimeConfig::new("/tmp/afhttp-test".to_string());
        assert!(cfg.build_client().is_ok());

        cfg.proxy = Some("not a valid proxy".to_string());
        let err = cfg
            .build_client()
            .expect_err("should fail on invalid proxy");
        assert!(err.contains("invalid proxy"));
    }

    #[test]
    fn response_headers_map_and_content_length() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("42"));
        headers.append(SET_COOKIE, HeaderValue::from_static("a=1"));
        headers.append(SET_COOKIE, HeaderValue::from_static("b=2"));
        let map = response_headers_to_map(&headers).expect("headers");
        assert_eq!(parse_content_length(&map), Some(42));
        assert_eq!(
            map.get("set-cookie"),
            Some(&Value::Array(vec![
                Value::String("a=1".to_string()),
                Value::String("b=2".to_string())
            ]))
        );
    }

    #[test]
    fn response_headers_map_rejects_non_ascii() {
        let mut headers = reqwest::header::HeaderMap::new();
        let bad = HeaderValue::from_bytes(&[0xFF]).expect("header bytes");
        headers.insert("x-bad", bad);
        assert!(response_headers_to_map(&headers).is_err());
    }
}
