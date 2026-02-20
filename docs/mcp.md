# afhttp MCP Server

`afhttp --mode mcp` starts an [MCP (Model Context Protocol)](https://modelcontextprotocol.io) server over stdio. AI tools like Claude Desktop can then call HTTP requests directly through afhttp, getting structured JSON responses without writing curl commands.

## Start the server

```bash
afhttp --mode mcp
```

The server communicates over stdin/stdout using the MCP JSON-RPC 2.0 protocol.

## Claude Desktop setup

Add afhttp to your Claude Desktop MCP server config (`~/Library/Application Support/Claude/claude_desktop_config.json` on macOS):

```json
{
  "mcpServers": {
    "afhttp": {
      "command": "afhttp",
      "args": ["--mode", "mcp"]
    }
  }
}
```

Restart Claude Desktop. The `http_request` and `http_config` tools will appear in the tools panel.

## Tools

### `http_request`

Make an HTTP request and return the structured afhttp response as a JSON string.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `method` | string | yes | HTTP method: GET, POST, PUT, DELETE, PATCH, HEAD, OPTIONS |
| `url` | string | yes | Full URL including scheme |
| `headers` | array | no | Request headers as `[{\"name\":\"Header-Name\",\"value\":\"Header Value\"}]` (use `value: null` to remove a default header) |
| `body` | any | no | Request body. JSON objects/arrays set `Content-Type: application/json`. Strings are sent as-is. |
| `body_base64` | string | no | Base64-encoded binary request body |
| `timeout_idle_s` | integer | no | Per-request idle timeout in seconds (overrides config default) |
| `retry` | integer | no | Retry count (0 = disabled) |
| `response_redirect` | integer | no | Max redirects to follow (0 = disable) |
| `response_parse_json` | boolean | no | Parse JSON response body (default: true) |
| `response_decompress` | boolean | no | Auto-decompress gzip/brotli/deflate (default: true) |

**Returns:** A JSON string using the same envelope as CLI JSON output (`code`, `status`/`error_code`, `headers`, `body*`, `trace`), with `id`/`tag` omitted. `code` is `"response"` (transport success) or `"error"` (transport failure).

**Example response:**
```json
{
  "code": "response",
  "status": 200,
  "headers": {"content-type": "application/json"},
  "body": {"id": 1, "name": "Alice"},
  "trace": {"duration_ms": 95, "http_version": "h2"}
}
```

**Example error:**
```json
{
  "code": "error",
  "error_code": "connect_refused",
  "error": "connection refused",
  "retryable": true,
  "trace": {"duration_ms": 12}
}
```

### `http_config`

Get or update afhttp connection defaults. Call with no arguments to view the current config.

| Parameter | Type | Description |
|-----------|------|-------------|
| `proxy` | string | Proxy URL (e.g. `http://proxy.example.com:8080`) |
| `timeout_connect_s` | integer | TCP+TLS connect timeout in seconds |
| `timeout_idle_s` | integer | Default idle (no-data) timeout in seconds |
| `retry` | integer | Default retry count |
| `response_redirect` | integer | Default redirect limit |
| `response_parse_json` | boolean | Default JSON response parsing |
| `response_decompress` | boolean | Default auto-decompress |
| `tls_insecure` | boolean | Skip TLS certificate verification |
| `request_concurrency_limit` | integer | Max concurrent in-flight requests (0 = unlimited) |
| `headers` | array | Default request headers as `[{\"name\":\"Header-Name\",\"value\":\"Header Value\"}]` (use `value: null` to remove) |

**Returns:** Current `RuntimeConfig` as a JSON string.

## Usage example

Once Claude Desktop has afhttp configured as an MCP server, you can ask Claude:

> "Can you check the status of the GitHub API?"

Claude will call `http_request` with `{"method": "GET", "url": "https://api.github.com"}` and report the response.

> "Set the Authorization header to Bearer sk-xxx for all requests"

Claude will call `http_config` with `{"headers":[{"name":"Authorization","value":"Bearer sk-xxx"}]}`.

> "POST this JSON to the endpoint"

Claude will call `http_request` with the appropriate method, URL, and body.

## Limitations

- **No streaming**: MCP tool calls return a single result. `chunked` mode is not supported. Use afhttp CLI or pipe mode for SSE/streaming responses.
- **No WebSocket**: WebSocket upgrade is not available in MCP mode.
- **Config is session-scoped**: Config changes via `http_config` persist for the MCP server session but reset when the server restarts.
- **No file body**: `body_file` and multipart/urlencoded body types are not exposed (use `body` with inline content or `body_base64` for binary).
