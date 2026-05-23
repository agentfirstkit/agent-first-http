<!-- Generated. Do not edit by hand. -->

# afhttp CLI Reference

> Regenerate with `afhttp --help-markdown`.
> See [reference.md](reference.md) for field-level response details.

# Command-Line Help for `afhttp`

This document contains the help content for the `afhttp` command-line program.

**Command Overview:**

* [`afhttp`↴](#afhttp)

## `afhttp`

Agent-First HTTP — persistent HTTP client for AI agents.

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

**Usage:** `afhttp [OPTIONS] [METHOD] [URL]`

###### **Arguments:**

* `<METHOD>` — HTTP method (GET, POST, PUT, DELETE, PATCH, HEAD, OPTIONS)
* `<URL>` — URL to request

###### **Options:**

* `--header <HEADER>` — Request header (repeatable). Format: "Name: Value". Empty value removes default
* `--body <BODY>` — Request body. Valid JSON object/array auto-detected and sets Content-Type: application/json. @path reads from file
* `--body-base64 <BODY_BASE64>` — Base64-encoded binary request body
* `--body-file <BODY_FILE>` — Read request body from file
* `--body-multipart <BODY_MULTIPART>` — Multipart form part (repeatable). Format: name=value or name=@path[;filename=x][;type=mime]
* `--body-urlencoded <BODY_URLENCODED>` — URL-encoded form field (repeatable). Format: name=value. Sets Content-Type: application/x-www-form-urlencoded
* `--response-save-dir <RESPONSE_SAVE_DIR>` — Directory for auto-saved large response bodies
* `--response-save-above-bytes <RESPONSE_SAVE_ABOVE_BYTES>` — Auto-save response body to response-save-dir when larger than this (default: 10485760)
* `--request-concurrency-limit <REQUEST_CONCURRENCY_LIMIT>` — Max concurrent in-flight requests (0 = unlimited)
* `--timeout-connect-s <TIMEOUT_CONNECT_S>` — TCP+TLS handshake timeout in seconds (default: 10)
* `--timeout-idle-s <TIMEOUT_IDLE_S>` — No-data timeout in seconds (default: 30)
* `--retry <RETRY>` — Retry count (default: 0, no retry)
* `--retry-base-delay-ms <RETRY_BASE_DELAY_MS>` — Base delay for first retry in ms (default: 100). Subsequent: base * 2^(attempt-1)
* `--retry-on-status <RETRY_ON_STATUS>` — Comma-separated status codes to retry (e.g. 429,503)
* `--response-redirect <RESPONSE_REDIRECT>` — Redirect limit (default: 10, 0=disable)
* `--response-parse-json <RESPONSE_PARSE_JSON>` — Parse JSON response body (default: true)

  Possible values: `true`, `false`

* `--response-decompress <RESPONSE_DECOMPRESS>` — Auto-decompress response (default: true)

  Possible values: `true`, `false`

* `--response-save-file <RESPONSE_SAVE_FILE>` — Save response body to file
* `--response-save-resume` — Resume download if response-save-file exists
* `--response-max-bytes <RESPONSE_MAX_BYTES>` — Hard limit on response body size in bytes
* `--chunked` — Stream response in chunks
* `--chunked-delimiter <CHUNKED_DELIMITER>` — Chunk delimiter (default: \n). Use \n\n for SSE. Implies --chunked
* `--chunked-delimiter-raw` — Raw binary chunks (null delimiter). Implies --chunked
* `--progress-ms <PROGRESS_MS>` — Time-based progress interval in ms (default: 10000, 0=disable). Works with --progress-bytes
* `--progress-bytes <PROGRESS_BYTES>` — Byte-based progress interval (default: 0=disable). Works with --progress-ms
* `--tls-insecure` — Skip certificate verification
* `--tls-cacert-file <TLS_CACERT_FILE>` — CA certificate file path
* `--tls-cert-file <TLS_CERT_FILE>` — Client certificate file path
* `--tls-key-file <TLS_KEY_FILE>` — Client private key file path
* `--proxy <PROXY>` — Proxy URL
* `--upgrade <UPGRADE>` — Protocol upgrade (e.g. "websocket")
* `--output <OUTPUT>` — Output format: json (default), yaml (human-readable), plain (logfmt)

  Default value: `json`
* `--log <LOG>` — Log categories (comma-separated). Categories: startup, request, progress, retry, redirect
* `--verbose` — Enable all log categories (equivalent to --log startup,request,progress,retry,redirect)
* `--dry-run` — Preview the request without executing it
* `--mode <MODE>` — Runtime mode: cli (default), pipe, or curl

  Default value: `cli`

  Possible values: `cli`, `pipe`, `curl`
