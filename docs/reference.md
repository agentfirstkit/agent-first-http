# Agent-First HTTP — Protocol Reference

Every stdin/stdout line is a JSON object with a `code` field that identifies its type.
Runtime protocol/log events are emitted on stdout only; stderr is not a protocol channel.

In **CLI mode** (`afhttp METHOD URL [flags]`), output is the same schema but `id` and `tag` fields are omitted. Use `--output yaml` or `--output plain` for human-readable output (server response body is never modified). See [cli.md](cli.md) for CLI usage.

In **pipe mode** (`afhttp --mode pipe`), all fields including `id` and `tag` are present.

## Input (stdin)

### `request`

| Field | Required | Description |
|-------|----------|-------------|
| `code` | yes | `"request"` |
| `id` | yes | Client-assigned opaque string, echoed in every output for this request. Must be unique across all currently in-flight requests and open WebSocket connections — duplicate `id` returns `error_code: "invalid_request"` immediately. |
| `tag` | no | Opaque string echoed in every output for this request — useful for grouping or correlation. Not interpreted by `afhttp`. |
| `method` | yes | `GET`, `POST`, `PUT`, `DELETE`, `PATCH`, `HEAD`, `OPTIONS` |
| `url` | yes | Full URL including scheme and host |
| `headers` | no | Merged with `defaults.headers` — request wins on key conflict, `null` value removes a default |
| `body` | no | Request body — object/array/number/bool → serialized as JSON, sets `Content-Type: application/json`; string → raw bytes, no implicit Content-Type |
| `body_base64` | no | Base64-encoded binary body — no implicit Content-Type; set explicitly via `headers` if needed |
| `body_file` | no | Path to file used as request body — no implicit Content-Type; set explicitly via `headers` if needed |
| `body_multipart` | no | Multipart form parts — sets `Content-Type: multipart/form-data; boundary=...` (see below) |
| `body_urlencoded` | no | URL-encoded form fields — sets `Content-Type: application/x-www-form-urlencoded` (see below) |
| `options` | no | Per-request options (see below) |

`body`, `body_base64`, `body_file`, `body_multipart`, and `body_urlencoded` are mutually exclusive. `body` with object/array/number/bool values automatically sets `Content-Type: application/json`; all other body types require an explicit `Content-Type` header. (`body_multipart` and `body_urlencoded` always set their respective Content-Type automatically.)

#### Multipart parts (`body_multipart`)

Each part: `name` (required), plus one of `value` (text string), `value_base64` (binary), or `file` (path). Optional `filename` and `content_type` overrides.

#### URL-encoded fields (`body_urlencoded`)

Array of `{"name": "...", "value": "..."}` objects. `afhttp` percent-encodes both name and value per the `application/x-www-form-urlencoded` spec: unreserved chars (`A-Z a-z 0-9 - _ . *`) pass through unchanged; spaces → `+`; all other bytes → `%XX`. Duplicate names are supported — use separate array entries.

```json
{"code":"request","id":"1","method":"POST","url":"https://api.example.com/token",
 "body_urlencoded":[
   {"name":"grant_type","value":"authorization_code"},
   {"name":"code","value":"abc123"},
   {"name":"redirect_uri","value":"https://app.example.com/cb"}
 ]}
```

#### Options

| Field | Default | Description |
|-------|---------|-------------|
| `timeout_idle_s` | config | No-data timeout in seconds — abort if no bytes received for this long |
| `retry` | config | Retry count for retryable transport errors |
| `response_redirect` | config | Redirect limit (0 to disable) |
| `response_parse_json` | config | Parse JSON response body into an object |
| `response_decompress` | config | Auto-decompress response body |
| `response_save_resume` | config | Resume download if `response_save_file` file exists — adds `Range` header, appends on 206 |
| `retry_on_status` | config | HTTP status codes that trigger automatic retry (e.g. `[429, 503]`). **Replaces the config list entirely — does not merge.** |
| `response_max_bytes` | — | Hard limit on response body size. Excess returns `error_code: "response_too_large"`. |
| `chunked` | false | Deliver response body in chunks instead of buffering |
| `chunked_delimiter` | `"\n"` | Split delimiter: `"\n"` (NDJSON), `"\n\n"` (SSE), `null` (raw HTTP chunks, binary `data_base64`) |
| `response_save_file` | — | Save response body to this path |
| `progress_bytes` | 0 | Emit `progress` log every N bytes (file download only, 0=disabled). Works simultaneously with `progress_ms`. |
| `progress_ms` | 10000 | Emit `progress` log every N ms (file download only, 0=disabled). Works simultaneously with `progress_bytes`. |
| `upgrade` | — | `"websocket"` to open a WebSocket connection |
| `tls` | — | Per-request TLS override — builds a one-off client, no connection pool sharing. Fields: `insecure`, `cacert_pem`, `cacert_file`, `cert_pem`, `cert_file`, `key_pem_secret`, `key_file` |

### `config`

Partial update — only provided fields change. Deep-merged for nested objects (`defaults.headers`, `host_defaults`). Echoes full config after applying.

#### Global config fields

| Field | Default | Description |
|-------|---------|-------------|
| `response_save_dir` | `<system-temp>/afh/{uuid}` | Directory for auto-saved response bodies (uses OS temp dir; e.g. `/tmp/...` on macOS/Linux, `%TEMP%\\...` on Windows). Must be writable. |
| `response_save_above_bytes` | 10485760 | Responses larger than this are auto-saved to `response_save_dir/{id}` and returned as `body_file`. |
| `request_concurrency_limit` | 0 | Max concurrent in-flight requests (0 = unlimited). New requests above the limit return `error_code: "overloaded"`. |
| `timeout_connect_s` | 10 | TCP+TLS handshake timeout. Triggers client rebuild. |
| `pool_idle_timeout_s` | 90 | How long an idle connection is kept open (seconds). Triggers client rebuild. |
| `retry_base_delay_ms` | 100 | Base delay for first retry. Subsequent: `base × 2^(attempt-1)`. |
| `proxy` | null | Proxy URL (`http://`, `https://`, `socks5://`). Env vars `HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY` take priority. Triggers client rebuild. |
| `tls.insecure` | false | Skip certificate and hostname verification. Triggers client rebuild. |
| `tls.cacert_pem` | null | Inline CA certificate (PEM text). Clears `cacert_file`. Triggers client rebuild. |
| `tls.cacert_file` | null | Path to CA certificate (PEM) — like curl `--cacert`. Clears `cacert_pem`. Triggers client rebuild. |
| `tls.cert_pem` | null | Inline client certificate (PEM text). Clears `cert_file`. Triggers client rebuild. |
| `tls.cert_file` | null | Path to client certificate (PEM) for mTLS. Clears `cert_pem`. Triggers client rebuild. |
| `tls.key_pem_secret` | null | Inline private key (PEM, unencrypted). **Redacted in config echo.** Clears `key_file`. Triggers client rebuild. |
| `tls.key_file` | null | Path to private key (PEM). If absent, key is expected in the same file as `cert_file`. Clears `key_pem_secret`. Triggers client rebuild. |
| `log` | [] | Diagnostic event categories to emit as `log` events: `startup`, `progress`, `request`, `retry`, `redirect` |

"Triggers client rebuild" — the connection pool is recreated; existing pooled connections are dropped.

**TLS settings are not applied to WebSocket connections** — `wss://` uses the system certificate store. A `log` event (`websocket_tls_config_ignored`) is emitted in the `request` log category when a WebSocket request is made while non-default TLS config is active.

#### Request defaults (`defaults`)

Applied to every request; overridable per-request via `options`.

| Field | Default | Description |
|-------|---------|-------------|
| `headers` | `{"User-Agent":"afhttp/<version>"}` | Merged into every request. `null` value removes a default header. |
| `timeout_idle_s` | 30 | No-data timeout — abort if no bytes received for this many seconds |
| `retry` | 0 | Retry attempts for retryable transport errors |
| `response_redirect` | 10 | Maximum redirects to follow (0 to disable) |
| `response_parse_json` | true | Parse JSON content-type responses into objects |
| `response_decompress` | true | Auto-add `Accept-Encoding` and decompress. Not applied when `Accept-Encoding` is set explicitly. |
| `response_save_resume` | false | Resume interrupted downloads (requires `response_save_file`) |
| `retry_on_status` | [] | HTTP status codes that trigger automatic retry |

#### Per-host defaults (`host_defaults`)

```json
{"code":"config","host_defaults":{"api.example.com":{"headers":{"Authorization":"Bearer sk-xxx"}}}}
```

Merge order for every request: global `defaults` → `host_defaults[host]` → per-request `headers`.

### Other input commands

| `code` | Fields | Description |
|--------|--------|-------------|
| `send` | `id`, `data` or `data_base64` | Send a message on an open WebSocket. `data`: object/array → JSON text frame, string → raw text frame. `data_base64` → binary frame. Mutually exclusive. |
| `cancel` | `id` | Cancel an in-flight HTTP request (→ `error` with `cancelled`) or close a WebSocket (→ `chunk_end`). |
| `ping` | — | Health check. Returns `pong`. |
| `close` | — | Graceful shutdown — cancels in-flight work, waits up to 5s, emits terminal events, then exits. |

## Output (stdout)

### Agent Consumption Contract

For each request `id`, consume events as a finite state machine:

- HTTP buffered: `response` **or** `error` (exactly one terminal event).
- Chunked/download/WebSocket: `chunk_start` → zero or more `chunk_data`/`log` → `chunk_end` **or** `error`.
- `log` events are non-terminal and may appear between lifecycle events.
- `id` values are unique among active requests/connections; duplicate active `id` returns `error_code: "invalid_request"` immediately.
- On `close`, afhttp cancels active work and attempts to emit terminal events for any remaining `id` values before process exit.

### Response codes

| `code` | Description |
|--------|-------------|
| `response` | Buffered HTTP response (any status, including 4xx/5xx) |
| `error` | Transport-level failure — see error codes below |
| `chunk_start` | Chunked/download/WebSocket opened |
| `chunk_data` | One chunk or WebSocket message |
| `chunk_end` | Chunked/download/WebSocket completed |
| `config` | Config echo (after `config` command) |
| `pong` | Reply to `ping` |
| `close` | Shutdown acknowledgement |
| `log` | Diagnostic event — includes startup, progress, request, retry, redirect events |

### `response`

| Field | Present | Description |
|-------|---------|-------------|
| `id` | pipe mode only | Echoed from request |
| `tag` | if set | Echoed from request |
| `status` | always | HTTP status code |
| `headers` | always | All keys lowercase. Single value → string. Multiple values (e.g. `Set-Cookie`) → array. |
| `body` | if present | JSON content-type + `response_parse_json: true` → parsed object; `text/*` with valid UTF-8 → string |
| `body_base64` | if present | Binary body, or `text/*`/JSON body with invalid UTF-8 bytes (original bytes preserved exactly, base64-encoded) |
| `body_file` | if present | Path where body was saved (exceeded `response_save_above_bytes` or `response_save_file` was set) |
| `body_parse_failed` | if true | Content-Type was `application/json` but parsing failed. `body` contains raw text (valid UTF-8), or `body_base64` contains original bytes (invalid UTF-8). |
| `trace` | always | See Trace below |

Body selection rules (when `response_parse_json: true`):
- JSON content-type → `body` (parsed object; parse failure + valid UTF-8 → string + `body_parse_failed: true`; parse failure + invalid UTF-8 → `body_base64` + `body_parse_failed: true`)
- `text/*` + valid UTF-8 → `body` (string)
- `text/*` + invalid UTF-8 → `body_base64` (bytes preserved exactly)
- Binary ≤ `response_save_above_bytes` → `body_base64`
- Any content-type > `response_save_above_bytes` → `body_file`
- `response_save_file` set → `body_file` always
- No body (204, 304, HEAD) → none of the above

### `error`

| Field | Description |
|-------|-------------|
| `id` | Echoed from request (absent when input is completely unparseable; absent in CLI mode) |
| `tag` | Echoed from request if set |
| `error_code` | Machine-readable code (see table below) |
| `error` | Human-readable detail |
| `retryable` | Whether retrying may help |
| `trace` | See Trace below |

`code: "response"` with any HTTP status (including 4xx/5xx) is **not** an error — it means the transport succeeded. `code: "error"` means the transport itself failed.

#### Error codes

| `error_code` | `retryable` | Cause |
|---|---|---|
| `dns_failed` | true | DNS resolution failed |
| `connect_refused` | true | TCP connection refused or reset |
| `connect_timeout` | true | TCP+TLS handshake exceeded `timeout_connect_s` |
| `tls_error` | false | TLS handshake or certificate verification failure |
| `request_timeout` | false | Idle timeout — no data received within `timeout_idle_s` |
| `too_many_redirects` | false | Exceeded `response_redirect` |
| `response_too_large` | false | Body exceeded `response_max_bytes` |
| `overloaded` | true | Request rejected because in-flight request limit was reached |
| `chunk_disconnected` | false | Connection lost during chunked/WebSocket/download |
| `cancelled` | false | Cancelled via `cancel` command |
| `invalid_request` | false | Malformed JSON, missing field, or duplicate `id` |
| `invalid_response` | false | Server protocol violation (e.g. non-ASCII header bytes) |
| `internal_error` | false | Internal serialization/output failure (rare) |

Retryable errors are automatically retried up to `retry` with exponential backoff: delay for attempt N = `retry_base_delay_ms × 2^(N-1)`.

### `chunk_start`

| Field | Description |
|-------|-------------|
| `id` | Request id (pipe mode only, omitted in CLI mode) |
| `tag` | Echoed from request if set |
| `status` | HTTP status (200 for chunked/download, 101 for WebSocket) |
| `headers` | Response headers (same format as `response`) |
| `content_length_bytes` | Parsed `Content-Length`, if present |

### `chunk_data`

| Field | Description |
|-------|-------------|
| `id` | Request id (pipe mode only, omitted in CLI mode) |
| `data` | Text chunk (valid UTF-8) or WebSocket text frame |
| `data_base64` | Binary chunk, WebSocket binary frame, or text chunk with invalid UTF-8 bytes (raw mode or binary frame) |

### `chunk_end`

| Field | Description |
|-------|-------------|
| `id` | Request id (pipe mode only, omitted in CLI mode) |
| `tag` | Echoed from request if set |
| `body_file` | Path where body was saved (file download only) |
| `trace` | See Trace below |

### Trace

`duration_ms` is always present. Other fields are best-effort.

| Field | Description |
|-------|-------------|
| `duration_ms` | Total wall-clock time including redirects and retries |
| `http_version` | `h1`, `h2`, or `ws` |
| `remote_addr` | Server IP address |
| `sent_bytes` | Request body bytes sent |
| `received_bytes` | Response body bytes received |
| `redirects` | Number of redirects followed |
| `chunks` | Number of chunks or WebSocket messages delivered |

### `pong`

```json
{"code":"pong","trace":{"uptime_s":42,"requests_total":100,"connections_active":3}}
```

### `log`

All diagnostic output uses `code: "log"` with an `event` field identifying the category. Emitted only when the category is enabled in `config.log`. In CLI mode, use `--log <categories>` or `--verbose` to enable.

```json
{"code":"log","event":"startup","version":"<version>","argv":["afhttp","--mode","pipe","--log","startup"],"config":{...}}
{"code":"log","event":"progress","id":"dl-1","received_bytes":10485760,"total_bytes":104857600,"percent":10,"eta_s":27}
{"code":"log","event":"request","id":"req-1","implicit_headers":{"Content-Type":"application/json","Accept-Encoding":"gzip, deflate, br"}}
{"code":"log","event":"retry","id":"req-3","host":"api.example.com","reason":"connection_reset","attempt":1,"delay_ms":100}
{"code":"log","event":"redirect","id":"req-5","status":301,"from":"http://example.com/api","to":"https://example.com/api"}
```

#### `startup` fields

| Field | Description |
|-------|-------------|
| `version` | afhttp version string |
| `argv` | Process arguments as an array |
| `config` | Full resolved config at startup |

#### `progress` fields

| Field | Description |
|-------|-------------|
| `id` | Request id (pipe mode only) |
| `received_bytes` | Bytes received so far |
| `total_bytes` | Total expected bytes (from `Content-Length`, if known) |
| `percent` | Download progress percentage 0–100 (if `total_bytes` known) |
| `eta_s` | Estimated seconds remaining (if `total_bytes` known and download rate measurable) |

Progress is emitted during file downloads. Both `progress_ms` (time-based) and `progress_bytes` (byte-count-based) triggers work simultaneously — whichever fires first emits a progress event.

#### `request` fields

| Field | Description |
|-------|-------------|
| `id` | Request id |
| `implicit_headers` | Headers that afhttp added automatically (not in the user-supplied headers map). Currently: `Content-Type: application/json` when body is a JSON value (object/array/number/bool); `Content-Type: application/x-www-form-urlencoded` when `body_urlencoded` is used; `Accept-Encoding` when decompress is active; `Range` when `response_save_resume` is set and the target file already exists. Only emitted when there are implicit headers to report. |
