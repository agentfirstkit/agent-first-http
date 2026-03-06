use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, RwLock};

use crate::handler;
use crate::types::*;
use crate::App;
use agent_first_data::RedactionPolicy;

const MCP_OUTPUT_CHANNEL_CAPACITY: usize = 512;

// ---------------------------------------------------------------------------
// Parameter structs
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
struct NameValue {
    /// Field name
    name: String,
    /// Field value; null removes the field from defaults
    #[serde(default)]
    value: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct HttpRequestParams {
    /// HTTP method (GET, POST, PUT, DELETE, PATCH, HEAD, OPTIONS)
    method: String,
    /// Request URL
    url: String,
    /// Request headers as name/value pairs
    #[serde(default)]
    headers: Option<Vec<NameValue>>,
    /// Request body. JSON objects/arrays are sent with Content-Type: application/json.
    /// Strings are sent as plain text.
    body: Option<Value>,
    /// Base64-encoded binary request body
    body_base64: Option<String>,
    /// Per-request idle (no-data) timeout in seconds
    timeout_idle_s: Option<u64>,
    /// Retry count (default: 0)
    retry: Option<u32>,
    /// Maximum number of redirects to follow (0=disable)
    response_redirect: Option<u32>,
    /// Parse JSON response body (default: true)
    response_parse_json: Option<bool>,
    /// Auto-decompress gzip/brotli/deflate responses (default: true)
    response_decompress: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
struct HttpConfigParams {
    /// Proxy URL (e.g. "http://proxy.example.com:8080")
    proxy: Option<String>,
    /// TCP+TLS handshake timeout in seconds
    timeout_connect_s: Option<u64>,
    /// Default idle (no-data) timeout in seconds
    timeout_idle_s: Option<u64>,
    /// Default retry count
    retry: Option<u32>,
    /// Default redirect limit
    response_redirect: Option<u32>,
    /// Default JSON response parsing
    response_parse_json: Option<bool>,
    /// Default auto-decompress
    response_decompress: Option<bool>,
    /// Skip TLS certificate verification
    tls_insecure: Option<bool>,
    /// Max concurrent in-flight requests (0 = unlimited)
    request_concurrency_limit: Option<u64>,
    /// Global default headers for any host as name/value pairs (null removes a header)
    headers_for_any_hosts: Option<Vec<NameValue>>,
}

// ---------------------------------------------------------------------------
// MCP server struct
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct AfhMcp {
    app: Arc<App>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl AfhMcp {
    pub fn new(app: Arc<App>) -> Self {
        Self {
            app,
            tool_router: Self::tool_router(),
        }
    }

    /// Make an HTTP request and return the structured afhttp response as JSON.
    #[tool(
        description = "Make an HTTP request and return the structured response as a JSON object"
    )]
    async fn http_request(
        &self,
        params: Parameters<HttpRequestParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;

        // Snapshot current config and client for this call
        let current_config = {
            let cfg = self.app.config.read().await;
            cfg.clone()
        };
        let current_client = {
            let cli = self.app.client.read().await;
            cli.clone()
        };

        let (tx, mut rx) = mpsc::channel::<Output>(MCP_OUTPUT_CHANNEL_CAPACITY);
        let call_app = Arc::new(App {
            config: RwLock::new(current_config),
            client: RwLock::new(current_client),
            writer: tx,
            in_flight: RwLock::new(HashMap::new()),
            ws_connections: RwLock::new(HashMap::new()),
            request_count: AtomicU64::new(0),
            start_time: Instant::now(),
        });

        let headers = to_header_map(p.headers);

        let options = RequestOptions {
            timeout_idle_s: p.timeout_idle_s,
            retry: p.retry,
            response_redirect: p.response_redirect,
            response_parse_json: p.response_parse_json,
            response_decompress: p.response_decompress,
            // MCP tool calls are not streamed — chunked mode is not supported
            chunked: false,
            ..RequestOptions::default()
        };

        tokio::spawn(async move {
            handler::execute_request(
                &call_app,
                "mcp".to_string(),
                None,
                p.method,
                p.url,
                headers,
                p.body,
                p.body_base64,
                None, // body_file
                None, // body_multipart
                None, // body_urlencoded
                options,
            )
            .await;
        });

        let output = loop {
            let next = rx
                .recv()
                .await
                .ok_or_else(|| McpError::internal_error("no output received from request", None))?;
            match next {
                Output::Response { .. } | Output::Error { .. } => break next,
                _ => continue,
            }
        };

        // Strip id/tag from output for a cleaner MCP response
        let mut value = serde_json::to_value(&output)
            .map_err(|e| McpError::internal_error(format!("serialize output: {e}"), None))?;
        if let Some(obj) = value.as_object_mut() {
            obj.remove("id");
            obj.remove("tag");
        }
        // Keep MCP payload schema aligned with CLI JSON mode.
        let json = match output {
            Output::Response { .. } => {
                agent_first_data::output_json_with(&value, RedactionPolicy::RedactionTraceOnly)
            }
            _ => agent_first_data::output_json(&value),
        };

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Get or update afhttp configuration defaults. Call with no arguments to view current config.
    #[tool(description = "Get or update afhttp HTTP client configuration defaults")]
    async fn http_config(
        &self,
        params: Parameters<HttpConfigParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;

        let has_changes = p.proxy.is_some()
            || p.timeout_connect_s.is_some()
            || p.tls_insecure.is_some()
            || p.request_concurrency_limit.is_some()
            || p.headers_for_any_hosts.is_some()
            || p.timeout_idle_s.is_some()
            || p.retry.is_some()
            || p.response_redirect.is_some()
            || p.response_parse_json.is_some()
            || p.response_decompress.is_some();

        if has_changes {
            // Build defaults patch if any defaults-level fields are set
            let defaults = {
                let has_defaults = p.timeout_idle_s.is_some()
                    || p.retry.is_some()
                    || p.response_redirect.is_some()
                    || p.response_parse_json.is_some()
                    || p.response_decompress.is_some()
                    || p.headers_for_any_hosts.is_some();
                if has_defaults {
                    let headers_for_any_hosts = Some(to_header_map(p.headers_for_any_hosts));
                    Some(RequestDefaultsPartial {
                        headers_for_any_hosts,
                        timeout_idle_s: p.timeout_idle_s,
                        retry: p.retry,
                        response_redirect: p.response_redirect,
                        response_parse_json: p.response_parse_json,
                        response_decompress: p.response_decompress,
                        ..RequestDefaultsPartial::default()
                    })
                } else {
                    None
                }
            };

            let tls = p.tls_insecure.map(|insecure| TlsConfigPartial {
                insecure: Some(insecure),
                ..TlsConfigPartial::default()
            });

            let patch = ConfigPatch {
                proxy: p.proxy,
                request_concurrency_limit: p.request_concurrency_limit,
                timeout_connect_s: p.timeout_connect_s,
                tls,
                defaults,
                ..ConfigPatch::default()
            };

            let (needs_rebuild, previous_config) = {
                let mut config = self.app.config.write().await;
                let previous = config.clone();
                let needs = config.apply_update(patch);
                (needs, previous)
            };

            if needs_rebuild {
                let config = self.app.config.read().await;
                match config.build_client() {
                    Ok(new_client) => {
                        drop(config);
                        let mut client = self.app.client.write().await;
                        *client = new_client;
                    }
                    Err(e) => {
                        drop(config);
                        let mut config = self.app.config.write().await;
                        *config = previous_config;
                        return Err(McpError::internal_error(
                            format!("rebuild client: {e}"),
                            None,
                        ));
                    }
                }
            }
        }

        let config = self.app.config.read().await;
        let json = serde_json::to_string(&*config)
            .map_err(|e| McpError::internal_error(format!("serialize config: {e}"), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }
}

#[tool_handler]
impl ServerHandler for AfhMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "afhttp HTTP client — make HTTP requests and configure defaults. \
            Use http_request to make HTTP calls with structured JSON responses. \
            Use http_config to view or update connection settings.",
        )
    }
}

fn to_header_map(items: Option<Vec<NameValue>>) -> HashMap<String, Value> {
    let mut headers = HashMap::new();
    if let Some(items) = items {
        for item in items {
            let value = match item.value {
                Some(v) => Value::String(v),
                None => Value::Null,
            };
            headers.insert(item.name, value);
        }
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::RuntimeConfig;
    use rmcp::ServerHandler;

    async fn test_app() -> Arc<App> {
        let save_dir = std::env::temp_dir()
            .join(format!("afhttp-mcp-test-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let config = RuntimeConfig::new(save_dir);
        let client = config.build_client().expect("build client");
        let (tx, _rx) = mpsc::channel(16);
        Arc::new(App {
            config: RwLock::new(config),
            client: RwLock::new(client),
            writer: tx,
            in_flight: RwLock::new(HashMap::new()),
            ws_connections: RwLock::new(HashMap::new()),
            request_count: AtomicU64::new(0),
            start_time: Instant::now(),
        })
    }

    #[test]
    fn to_header_map_converts_none_and_nulls() {
        let empty = to_header_map(None);
        assert!(empty.is_empty());

        let mapped = to_header_map(Some(vec![
            NameValue {
                name: "X-A".to_string(),
                value: Some("1".to_string()),
            },
            NameValue {
                name: "X-B".to_string(),
                value: None,
            },
        ]));
        assert_eq!(mapped.get("X-A"), Some(&Value::String("1".to_string())));
        assert_eq!(mapped.get("X-B"), Some(&Value::Null));
    }

    #[tokio::test]
    async fn get_info_exposes_tools_capability() {
        let app = test_app().await;
        let mcp = AfhMcp::new(app);
        let info = mcp.get_info();
        assert!(info.instructions.is_some());
    }

    #[tokio::test]
    async fn http_config_get_and_update() {
        let app = test_app().await;
        let mcp = AfhMcp::new(app.clone());

        let res = mcp
            .http_config(Parameters(HttpConfigParams {
                proxy: None,
                timeout_connect_s: None,
                timeout_idle_s: None,
                retry: None,
                response_redirect: None,
                response_parse_json: None,
                response_decompress: None,
                tls_insecure: None,
                request_concurrency_limit: None,
                headers_for_any_hosts: None,
            }))
            .await;
        assert!(res.is_ok());

        let res = mcp
            .http_config(Parameters(HttpConfigParams {
                proxy: Some("http://127.0.0.1:8080".to_string()),
                timeout_connect_s: Some(12),
                timeout_idle_s: Some(34),
                retry: Some(2),
                response_redirect: Some(4),
                response_parse_json: Some(false),
                response_decompress: Some(false),
                tls_insecure: Some(true),
                request_concurrency_limit: Some(5),
                headers_for_any_hosts: Some(vec![NameValue {
                    name: "X-Test".to_string(),
                    value: Some("yes".to_string()),
                }]),
            }))
            .await;
        assert!(res.is_ok());

        let cfg = app.config.read().await;
        assert_eq!(cfg.proxy.as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(cfg.timeout_connect_s, 12);
        assert_eq!(cfg.request_concurrency_limit, 5);
        assert_eq!(
            cfg.defaults.headers_for_any_hosts.get("X-Test"),
            Some(&Value::String("yes".to_string()))
        );
    }

    #[tokio::test]
    async fn http_request_returns_tool_result_for_invalid_url() {
        let app = test_app().await;
        let mcp = AfhMcp::new(app);
        let res = mcp
            .http_request(Parameters(HttpRequestParams {
                method: "GET".to_string(),
                url: "not-a-url".to_string(),
                headers: None,
                body: None,
                body_base64: None,
                timeout_idle_s: Some(1),
                retry: Some(0),
                response_redirect: Some(0),
                response_parse_json: Some(true),
                response_decompress: Some(true),
            }))
            .await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn http_config_rebuild_failure_rolls_back_config() {
        let app = test_app().await;
        let before = app.config.read().await.clone();
        let mcp = AfhMcp::new(app.clone());

        let res = mcp
            .http_config(Parameters(HttpConfigParams {
                proxy: Some("not a valid proxy".to_string()),
                timeout_connect_s: None,
                timeout_idle_s: None,
                retry: None,
                response_redirect: None,
                response_parse_json: None,
                response_decompress: None,
                tls_insecure: None,
                request_concurrency_limit: None,
                headers_for_any_hosts: None,
            }))
            .await;

        assert!(res.is_err());
        let after = app.config.read().await.clone();
        assert_eq!(after.proxy, before.proxy);
        assert_eq!(after.timeout_connect_s, before.timeout_connect_s);
        assert_eq!(
            after.request_concurrency_limit,
            before.request_concurrency_limit
        );
    }

    #[tokio::test]
    async fn http_request_skips_log_outputs_and_returns_terminal_result() {
        let app = test_app().await;
        {
            let mut cfg = app.config.write().await;
            cfg.log = vec!["request".to_string()];
        }
        let mcp = AfhMcp::new(app);

        let res = mcp
            .http_request(Parameters(HttpRequestParams {
                method: "POST".to_string(),
                url: "http://127.0.0.1:1".to_string(),
                headers: None,
                body: Some(serde_json::json!({"hello":"world"})),
                body_base64: None,
                timeout_idle_s: Some(1),
                retry: Some(0),
                response_redirect: Some(0),
                response_parse_json: Some(true),
                response_decompress: Some(true),
            }))
            .await
            .expect("tool result");

        let payload: Value = res.into_typed().expect("json output");
        assert!(matches!(
            payload.get("code").and_then(Value::as_str),
            Some("response") | Some("error")
        ));
        assert_ne!(payload.get("code").and_then(Value::as_str), Some("log"));
    }
}
