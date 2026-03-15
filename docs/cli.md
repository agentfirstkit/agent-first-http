# Agent-First HTTP — CLI Manual

Practical patterns for using `afhttp`. For the full field reference see [reference.md](reference.md).

## Modes

- `--mode cli` (default)
- `--mode pipe`
- `--mode curl`

## CLI Mode (default)

One request, one JSON response, exit:

```bash
afhttp GET https://api.example.com/users
```

Output — one JSON line:
```json
{"code":"response","status":200,"headers":{"content-type":"application/json"},"body":[{"id":1,"name":"Alice"}],"trace":{"duration_ms":120,"http_version":"h2"}}
```

By default, CLI mode outputs only the response (or error). `id` and `tag` fields are omitted. Use `--verbose` or `--log <categories>` to include diagnostic events.

### Exit codes

| Code | Meaning |
|------|---------|
| 0 | HTTP response received (any status, including 4xx/5xx) |
| 1 | Transport error (DNS, connection, timeout) |
| 2 | Invalid arguments |

### Output format

```bash
# Default: single-line JSON (machine-readable)
afhttp GET https://api.example.com/users

# YAML: human-readable with suffix-aware formatting (duration_ms → "120ms", received_bytes → "1.5KB")
afhttp GET https://api.example.com/users --output yaml

# Plain: logfmt single-line with suffix-aware formatting
afhttp GET https://api.example.com/users --output plain
```

Server response body is never modified by output format — only the envelope (code, status, headers, trace) is formatted.

### Verbosity

By default, CLI mode outputs only `response`, `error`, and `chunk_*` events — no startup, no diagnostics.

```bash
# Include startup log (shows resolved config)
afhttp GET https://api.example.com/users --log startup

# Enable specific diagnostic log categories
afhttp GET https://api.example.com/users --log startup,request,retry

# Enable all log categories (startup + progress + request + retry + redirect)
afhttp GET https://api.example.com/users --verbose
```

### POST with JSON body

```bash
afhttp POST https://api.example.com/users --body '{"name":"Alice","email":"alice@example.com"}'
```

When `body` is a JSON object or array, afhttp serializes it and sets `Content-Type: application/json`. String values are sent as raw bytes with no implicit Content-Type — set it explicitly with `--header` if needed.

### Headers

```bash
afhttp GET https://api.example.com/me --header "Authorization: Bearer sk-xxx"
```

Multiple headers:
```bash
afhttp GET https://api.example.com/me --header "Authorization: Bearer sk-xxx" --header "Accept: application/json"
```

Remove default header (empty value):
```bash
afhttp GET https://api.example.com/data --header "User-Agent:"
```

### Body from file

```bash
afhttp POST https://api.example.com/upload --body @/path/to/data.json
```

Or explicitly (no implicit Content-Type — set it if needed):
```bash
afhttp PUT https://storage.example.com/file --body-file /path/to/data.bin --header "Content-Type: application/octet-stream"
```

### Binary body (base64)

No implicit Content-Type — set it explicitly:
```bash
afhttp PUT https://storage.example.com/file \
  --body-base64 SGVsbG8gd29ybGQ= \
  --header "Content-Type: application/octet-stream"
```

### Multipart form upload

```bash
afhttp POST https://api.openai.com/v1/files \
  --header "Authorization: Bearer sk-xxx" \
  --body-multipart purpose=assistants \
  --body-multipart file=@/path/to/data.jsonl;filename=data.jsonl;type=application/jsonl
```

### URL-encoded form

Sets `Content-Type: application/x-www-form-urlencoded` automatically. Values are percent-encoded (spaces → `+`):

```bash
afhttp POST https://api.example.com/token \
  --body-urlencoded grant_type=authorization_code \
  --body-urlencoded code=abc123 \
  --body-urlencoded "redirect_uri=https://app.example.com/cb"
```

Duplicate names are supported — use multiple `--body-urlencoded` flags with the same name:

```bash
afhttp POST https://api.example.com/search \
  --body-urlencoded tag=rust \
  --body-urlencoded tag=async
```

### Streaming (chunked)

NDJSON:
```bash
afhttp GET https://api.example.com/stream --chunked
```

SSE:
```bash
afhttp POST https://api.openai.com/v1/chat/completions \
  --header "Authorization: Bearer sk-xxx" \
  --body '{"model":"gpt-4o","messages":[{"role":"user","content":"Hello"}],"stream":true}' \
  --chunked-delimiter '\n\n'
```

Raw binary chunks:
```bash
afhttp GET https://example.com/binary-stream --chunked-delimiter-raw
```

Chunked output is JSONL (multiple lines):
```json
{"code":"chunk_start","status":200,"headers":{"content-type":"text/event-stream"}}
{"code":"chunk_data","data":"event: content_block_delta\ndata: {\"delta\":{\"text\":\"Hello\"}}"}
{"code":"chunk_end","trace":{"duration_ms":1200,"chunks":3}}
```

### File download with progress

```bash
afhttp GET https://example.com/large.tar.gz --response-save-file /path/to/large.tar.gz --log progress
```

Progress is a `log` event (requires `--log progress`), emitted every 10s by default:
```json
{"code":"chunk_start","status":200,"headers":{"content-length":"104857600"},"content_length_bytes":104857600}
{"code":"log","event":"progress","received_bytes":10485760,"total_bytes":104857600,"percent":10,"eta_s":27}
{"code":"chunk_end","body_file":"/path/to/large.tar.gz","trace":{"duration_ms":8200,"received_bytes":104857600}}
```

### Retry

```bash
afhttp GET https://api.example.com/data --retry 3 --retry-on-status 429,503
```

Custom backoff base delay (default: 100ms, subsequent: base * 2^(attempt-1)):
```bash
afhttp GET https://api.example.com/data --retry 3 --retry-base-delay-ms 500
```

### Timeouts

```bash
afhttp GET https://slow.example.com/data --timeout-idle-s 60 --timeout-connect-s 5
```

### Additional useful flags

Redirect handling:
```bash
afhttp GET https://example.com/redirect --response-redirect 0
```

Response parsing/decompression controls:
```bash
afhttp GET https://api.example.com/data --response-parse-json false --response-decompress false
```

Hard cap response size:
```bash
afhttp GET https://api.example.com/large --response-max-bytes 1048576
```

Resume a partially downloaded file:
```bash
afhttp GET https://example.com/large.tar.gz --response-save-file /path/to/large.tar.gz --response-save-resume
```

Tune progress event cadence for downloads:
```bash
afhttp GET https://example.com/large.tar.gz --response-save-file /path/to/large.tar.gz --log progress --progress-ms 1000 --progress-bytes 1048576
```

### TLS

Skip certificate verification:
```bash
afhttp GET https://self-signed.example.com/api --tls-insecure
```

Custom CA:
```bash
afhttp GET https://internal.corp/api --tls-cacert-file /path/to/ca.pem
```

mTLS:
```bash
afhttp GET https://mtls.example.com/api \
  --tls-cacert-file /path/to/ca.pem \
  --tls-cert-file /path/to/client.pem \
  --tls-key-file /path/to/client-key.pem
```

### WebSocket (receive-only)

```bash
afhttp GET wss://stream.example.com/ws --upgrade websocket
```

Streams `chunk_data` until server closes or SIGINT. For bidirectional WebSocket, use `--mode pipe` mode.

When non-default TLS config is active (`tls.insecure`, custom CA, or client cert/key), WebSocket connections still use system roots only. In pipe mode, afhttp emits `{"code":"log","event":"websocket_tls_config_ignored",...}` under the `request` log category.

### Auto-save directory

Large response bodies are auto-saved to a directory (default: auto-generated temp dir):
```bash
afhttp GET https://api.example.com/large --response-save-dir /path/to/downloads
```

Override when responses should be saved inline up to a larger limit (default: 10485760 = 10 MiB):
```bash
afhttp GET https://api.example.com/data --response-save-above-bytes 52428800
```

### Proxy

```bash
afhttp GET https://api.example.com/data --proxy http://proxy.corp:8080
```

### Concurrency Guard

Limit concurrent in-flight requests in pipe mode (0 = unlimited):

```bash
afhttp --mode pipe --request-concurrency-limit 200
```

## Pipe Mode

For long-lived sessions with connection reuse, concurrent requests, WebSocket send/receive, and runtime config changes:

```bash
afhttp --mode pipe
```

Reads JSONL from stdin, writes JSONL to stdout. Runtime protocol/log events are emitted on stdout only; stderr is not a protocol channel. CLI flags that set config (`--log`, `--proxy`, `--tls-*`, etc.) are applied at startup. No startup event is emitted by default — enable with `--log startup`:

```bash
afhttp --mode pipe --log startup
```

```json
{"code":"log","event":"startup","version":"<version>","argv":["afhttp","--mode","pipe","--log","startup"],"config":{"response_save_dir":"<system-temp>/afhttp/a1b2c3d4","response_save_above_bytes":10485760,"request_concurrency_limit":0,"timeout_connect_s":10,"pool_idle_timeout_s":90,"retry_base_delay_ms":100,"tls":{"insecure":false},"log":["startup"],"defaults":{"headers_for_any_hosts":{"User-Agent":"afhttp/<version>"},"timeout_idle_s":30,"retry":0,"response_redirect":10,"response_parse_json":true,"response_decompress":true,"response_save_resume":false,"retry_on_status":[]}}}
```

### HTTP Requests

#### GET

```json
→ {"code":"request","id":"1","method":"GET","url":"https://api.example.com/users"}
← {"code":"response","id":"1","status":200,"headers":{"content-type":"application/json"},"body":[{"id":1,"name":"Alice"}],"trace":{"duration_ms":120,"http_version":"h2","redirects":0}}
```

#### POST with JSON body

```json
→ {"code":"request","id":"1","method":"POST","url":"https://api.example.com/users","body":{"name":"Alice","email":"alice@example.com"}}
← {"code":"response","id":"1","status":201,"headers":{"content-type":"application/json"},"body":{"id":42,"name":"Alice"},"trace":{"duration_ms":85}}
```

#### Per-request auth header

```json
→ {"code":"request","id":"1","method":"GET","url":"https://api.example.com/me","headers":{"Authorization":"Bearer sk-xxx"}}
```

#### Binary body

Set Content-Type explicitly — none is added automatically for `body_base64` or `body_file`:
```json
→ {"code":"request","id":"1","method":"PUT","url":"https://storage.example.com/file","headers":{"Content-Type":"application/octet-stream"},"body_base64":"SGVsbG8gd29ybGQ="}
```

#### Upload a file as body

```json
→ {"code":"request","id":"1","method":"PUT","url":"https://storage.example.com/file","headers":{"Content-Type":"application/octet-stream"},"body_file":"/path/to/data.bin"}
```

#### Multipart form upload

```json
→ {"code":"request","id":"1","method":"POST","url":"https://api.openai.com/v1/files",
   "headers":{"Authorization":"Bearer sk-xxx"},
   "body_multipart":[
     {"name":"purpose","value":"assistants"},
     {"name":"file","file":"/path/to/data.jsonl","filename":"data.jsonl","content_type":"application/jsonl"}
   ]}
```

#### URL-encoded form

`afhttp` percent-encodes all names and values automatically:

```json
→ {"code":"request","id":"1","method":"POST","url":"https://api.example.com/token",
   "body_urlencoded":[
     {"name":"grant_type","value":"authorization_code"},
     {"name":"code","value":"abc123"},
     {"name":"redirect_uri","value":"https://app.example.com/cb"}
   ]}
← {"code":"response","id":"1","status":200,"headers":{"content-type":"application/json"},"body":{"access_token":"..."},"trace":{"duration_ms":85}}
```

Duplicate names are supported — use separate array entries:

```json
→ {"code":"request","id":"1","method":"POST","url":"https://api.example.com/search",
   "body_urlencoded":[{"name":"tag","value":"rust"},{"name":"tag","value":"async"}]}
```

### Configuration

Configuration changes apply immediately to subsequent requests. The full config is echoed after each `config` command.

#### Set default auth header for all requests

```json
→ {"code":"config","defaults":{"headers_for_any_hosts":{"Authorization":"Bearer sk-xxx"}}}
← {"code":"config",...}
```

#### Per-host default headers

Apply headers only to requests matching a specific host. Merge order: global defaults → host defaults → per-request headers.

```json
→ {"code":"config","host_defaults":{
     "api.openai.com":{"headers":{"Authorization":"Bearer sk-openai"}},
     "api.example.com":{"headers":{"x-api-key":"sk-ant-xxx","api-version":"2023-06-01"}}
   }}
```

#### Remove a default header for one request

Set the header to `null` in the per-request `headers` field:

```json
→ {"code":"request","id":"1","method":"GET","url":"https://public.api.com/data","headers":{"Authorization":null}}
```

#### Adjust timeouts and retries

```json
→ {"code":"config","defaults":{"timeout_idle_s":60,"retry":3}}
```

#### Set a proxy

```json
→ {"code":"config","proxy":"http://proxy.corp:8080"}
```

Environment variables `HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY` take priority over this field.

#### Retry on HTTP status codes

Configure which HTTP status codes trigger automatic retry (exponential backoff):

```json
→ {"code":"config","defaults":{"retry_on_status":[429,503]}}
```

Per-request `retry_on_status` **replaces** the config list entirely — it does not merge.

### Streaming

#### SSE (Server-Sent Events)

```json
→ {"code":"request","id":"1","method":"POST","url":"https://api.openai.com/v1/chat/completions",
   "headers":{"Authorization":"Bearer sk-xxx"},
   "body":{"model":"gpt-4o","messages":[{"role":"user","content":"Hello"}],"stream":true},
   "options":{"chunked":true,"chunked_delimiter":"\n\n"}}
← {"code":"chunk_start","id":"1","status":200,"headers":{"content-type":"text/event-stream"}}
← {"code":"chunk_data","id":"1","data":"data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}],\"finish_reason\":null}"}
← {"code":"chunk_data","id":"1","data":"data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}],\"finish_reason\":\"stop\"}"}
← {"code":"chunk_data","id":"1","data":"data: [DONE]"}
← {"code":"chunk_end","id":"1","trace":{"duration_ms":1200,"chunks":3}}
```

#### NDJSON

```json
→ {"code":"request","id":"1","method":"GET","url":"https://api.example.com/stream",
   "options":{"chunked":true}}
← {"code":"chunk_start","id":"1","status":200,"headers":{"content-type":"application/x-ndjson"}}
← {"code":"chunk_data","id":"1","data":"{\"event\":\"update\",\"value\":1}"}
← {"code":"chunk_data","id":"1","data":"{\"event\":\"update\",\"value\":2}"}
← {"code":"chunk_end","id":"1","trace":{"duration_ms":800,"chunks":2}}
```

#### Raw HTTP chunks (binary)

```json
→ {"code":"request","id":"1","method":"GET","url":"https://example.com/binary-stream",
   "options":{"chunked":true,"chunked_delimiter":null}}
← {"code":"chunk_start","id":"1","status":200,"headers":{"content-type":"application/octet-stream"}}
← {"code":"chunk_data","id":"1","data_base64":"SGVsbG8="}
← {"code":"chunk_data","id":"1","data_base64":"IHdvcmxk"}
← {"code":"chunk_end","id":"1","trace":{"duration_ms":300,"chunks":2}}
```

### File Download

#### Save response to disk with progress

Enable the `progress` log category to receive progress events (emitted every 10s by default):

```json
→ {"code":"config","log":["progress"]}
→ {"code":"request","id":"1","method":"GET","url":"https://example.com/large.tar.gz",
   "options":{"response_save_file":"/path/to/large.tar.gz","progress_bytes":10485760}}
← {"code":"chunk_start","id":"1","status":200,"headers":{"content-length":"104857600","content-type":"application/gzip"},"content_length_bytes":104857600}
← {"code":"log","event":"progress","id":"1","received_bytes":10485760,"total_bytes":104857600,"percent":10,"eta_s":27}
← {"code":"log","event":"progress","id":"1","received_bytes":20971520,"total_bytes":104857600,"percent":20,"eta_s":24}
← {"code":"chunk_end","id":"1","body_file":"/path/to/large.tar.gz","trace":{"duration_ms":8200,"received_bytes":104857600}}
```

Progress is emitted by default every 10s (`progress_ms: 10000`) and optionally by byte count (`progress_bytes`). Both triggers work simultaneously.

#### Resume an interrupted download

```json
→ {"code":"request","id":"1","method":"GET","url":"https://example.com/large.tar.gz",
   "options":{"response_save_file":"/path/to/large.tar.gz","response_save_resume":true}}
```

If the file exists, `afhttp` adds `Range: bytes=N-` and appends to the file on 206. On 200, it overwrites.

### WebSocket

#### Open, subscribe, receive, close

```json
→ {"code":"request","id":"ws-1","method":"GET","url":"wss://stream.example.com/ws",
   "headers":{"Authorization":"Bearer sk-xxx"},"options":{"upgrade":"websocket"}}
← {"code":"chunk_start","id":"ws-1","status":101,"headers":{"upgrade":"websocket","sec-websocket-accept":"..."}}
→ {"code":"send","id":"ws-1","data":{"type":"subscribe","channel":"BTC-USD"}}
← {"code":"chunk_data","id":"ws-1","data":"{\"type\":\"snapshot\",\"price\":42000}"}
← {"code":"chunk_data","id":"ws-1","data":"{\"type\":\"update\",\"price\":42001}"}
→ {"code":"cancel","id":"ws-1"}
← {"code":"chunk_end","id":"ws-1","trace":{"duration_ms":30000,"http_version":"ws","chunks":2}}
```

### Error Handling

#### Error structure

```json
{"code":"error","id":"1","error_code":"connect_refused","error":"tcp connect error: Connection refused (os error 111)","retryable":true,"trace":{"duration_ms":5000}}
```

Match on `error_code`, not `error` text. `retryable` indicates whether afhttp's auto-retry already exhausted (when `true`, the agent may try again; when `false`, retrying won't help without changing something).

#### Error code reference

| `error_code` | `retryable` | Action |
|---|---|---|
| `dns_failed` | true | Auto-retried. If persists, check URL and DNS config. |
| `connect_refused` | true | Auto-retried. Server may be down or port wrong. |
| `connect_timeout` | true | Auto-retried. Network issue or wrong host. |
| `tls_error` | false | Check cert config. Retry won't help. |
| `request_timeout` | false | Idle timeout — no data received. Increase `timeout_idle_s` if needed. |
| `too_many_redirects` | false | Increase `response_redirect` or check for redirect loop. |
| `response_too_large` | false | Increase `response_max_bytes` or use `response_save_file`. |
| `overloaded` | true | In-flight concurrency limit reached. Retry later or raise `request_concurrency_limit`. |
| `chunk_disconnected` | false | Connection lost mid-stream. Agent decides whether to retry. |
| `cancelled` | false | Request was cancelled by the agent. |
| `invalid_request` | false | Fix the request before retrying. |
| `invalid_response` | false | Server protocol violation. Report to server owner. |
| `internal_error` | false | Internal serialization/output failure (rare). Retry may succeed, but capture logs. |

### Concurrent Requests (pipe mode)

Requests don't wait for each other. Responses arrive out of order — identify them by `id`:

```json
→ {"code":"request","id":"a","method":"GET","url":"https://api1.example.com/data"}
→ {"code":"request","id":"b","method":"GET","url":"https://api2.example.com/data"}
→ {"code":"request","id":"c","method":"GET","url":"https://api1.example.com/other"}
← {"code":"response","id":"b","status":200,...}     ← may arrive first
← {"code":"response","id":"a","status":200,...}
← {"code":"response","id":"c","status":200,...}     ← connection reuse is handled internally
```

### TLS and mTLS (pipe mode)

#### Skip certificate verification (dev/test only)

```json
→ {"code":"config","tls":{"insecure":true}}
```

#### Custom CA (private/internal services)

From file:
```json
→ {"code":"config","tls":{"cacert_file":"/path/to/internal-ca.pem"}}
```

Inline (no disk I/O, useful when cert is stored in a secret manager):
```json
→ {"code":"config","tls":{"cacert_pem":"-----BEGIN CERTIFICATE-----\nMIID..."}}
```

#### Mutual TLS (mTLS)

From files:
```json
→ {"code":"config","tls":{
     "cacert_file":"/path/to/ca.pem",
     "cert_file":"/path/to/client.pem",
     "key_file":"/path/to/client-key.pem"
   }}
```

Inline (private key is auto-redacted in config echo):
```json
→ {"code":"config","tls":{
     "cert_pem":"-----BEGIN CERTIFICATE-----\n...",
     "key_pem_secret":"-----BEGIN PRIVATE KEY-----\n..."
   }}
```

### Debugging (pipe mode)

#### Enable diagnostic log events

```json
→ {"code":"config","log":["startup","request","retry","redirect","progress"]}
```

#### Ping / health check

```json
→ {"code":"ping"}
← {"code":"pong","trace":{"uptime_s":42,"requests_total":100,"connections_active":3}}
```

### Shutdown (pipe mode)

Graceful — cancels active work, waits up to 5 seconds for terminal events, then emits process close:

```json
→ {"code":"close"}
← {"code":"close","message":"shutdown","trace":{"uptime_s":300,"requests_total":156}}
```

On stdin EOF, `afhttp` also shuts down gracefully.

---

## curl Compatibility

afhttp understands a subset of curl flags in explicit curl mode.

### Mode form

```bash
afhttp --mode curl [flags] URL
```

### Supported flags

| curl flag | afhttp equivalent |
|-----------|---------------|
| `-X METHOD` / `--request METHOD` | method |
| Positional URL | url |
| `-H "Name: Value"` / `--header` | headers |
| `-d DATA` / `--data DATA` | body (JSON object/array auto-detected, else text) |
| `--data-raw DATA` | body (always text, no JSON detect) |
| `--data-urlencode K=V` | body_urlencoded |
| `-F "name=value"` | body_multipart text part |
| `-F "name=@path"` | body_multipart file part |
| `-o PATH` / `--output PATH` | response_save_file |
| `-O` / `--remote-name` | response_save_file = basename(url) |
| `-L` / `--location` | response_redirect = 10 |
| `--max-redirs N` | response_redirect |
| `-k` / `--insecure` | tls_insecure |
| `--cacert PATH` | tls_cacert_file |
| `--cert PATH` | tls_cert_file |
| `--key PATH` | tls_key_file |
| `-x URL` / `--proxy URL` | proxy |
| `--retry N` | retry |
| `--connect-timeout N` | timeout_connect_s |
| `--max-time N` | timeout_idle_s |
| `-A STR` / `--user-agent STR` | User-Agent header |
| `-u USER:PASS` / `--user` | Authorization: Basic header |
| `-I` / `--head` | method = HEAD |
| `--compressed` | (no-op, afhttp decompresses by default) |
| `-N` / `--no-buffer` | chunked = true |
| `-v` / `--verbose` | all log categories |
| `-s` / `--silent` | (no-op, afhttp always outputs JSON) |
| `-b STR` / `--cookie STR` | Cookie header |
| `-e URL` / `--referer URL` | Referer header |
| `-T PATH` / `--upload-file PATH` | body_file, method defaults to PUT |
| `-C -` / `--continue-at -` | response_save_resume (requires -o) |

Multiple `-d` flags are concatenated with `&`, matching curl behavior.

### Output

curl compat always outputs JSON (same as native CLI mode). The response is the afhttp structured JSON object, not raw bytes.

### Unsupported flags

Flags not in the table above are silently ignored. If you rely on curl features not listed (e.g. `--aws-sigv4`, `--oauth2-bearer`, cookie jar files), use native afhttp flags or pipe mode instead.
