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
    /// Default request headers as name/value pairs (null value removes a header)
    headers: Option<Vec<NameValue>>,
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

    /// Make an HTTP request and return the structured afh response as JSON.
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

        let output = rx
            .recv()
            .await
            .ok_or_else(|| McpError::internal_error("no output received from request", None))?;

        // Strip id/tag from output for a cleaner MCP response
        let mut value = serde_json::to_value(&output)
            .map_err(|e| McpError::internal_error(format!("serialize output: {e}"), None))?;
        if let Some(obj) = value.as_object_mut() {
            obj.remove("id");
            obj.remove("tag");
        }
        let json = serde_json::to_string(&value)
            .map_err(|e| McpError::internal_error(format!("serialize output: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Get or update afh configuration defaults. Call with no arguments to view current config.
    #[tool(description = "Get or update afh HTTP client configuration defaults")]
    async fn http_config(
        &self,
        params: Parameters<HttpConfigParams>,
    ) -> Result<CallToolResult, McpError> {
        let p = params.0;

        let has_changes = p.proxy.is_some()
            || p.timeout_connect_s.is_some()
            || p.tls_insecure.is_some()
            || p.request_concurrency_limit.is_some()
            || p.headers.is_some()
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
                    || p.headers.is_some();
                if has_defaults {
                    let headers = Some(to_header_map(p.headers));
                    Some(RequestDefaultsPartial {
                        headers,
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

            let needs_rebuild = {
                let mut config = self.app.config.write().await;
                config.apply_update(patch)
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
        ServerInfo {
            instructions: Some(
                "afh HTTP client — make HTTP requests and configure defaults. \
                Use http_request to make HTTP calls with structured JSON responses. \
                Use http_config to view or update connection settings."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
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
