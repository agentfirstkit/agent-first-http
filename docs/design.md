# Agent-First HTTP — Design

## Problem

AI agents call HTTP APIs through bash tool calls. With curl, every request spawns a new process, pays a full TCP+TLS handshake, and returns human-readable text that must be parsed. Agents need structured JSON output and — when making multiple calls — connection reuse.

### The cost of curl-per-request

```
Agent                       curl process              Server
  │                            │                        │
  ├─ spawn curl ──────────────→│                        │
  │                            ├─ TCP handshake ───────→│
  │                            ├─ TLS handshake ───────→│
  │                            ├─ HTTP request ────────→│
  │                            │←──── HTTP response ────┤
  │←── stdout (text) ─────────┤                        │
  │                            ╳ (process exits)        │
  │                                                     │
  ├─ spawn curl ──────────────→│                        │  ← another process
  │                            ├─ TCP handshake ───────→│  ← another handshake
  │                            ├─ TLS handshake ───────→│  ← another TLS
  │                            ├─ HTTP request ────────→│
  │                            │←──── HTTP response ────┤
  │←── stdout (text) ─────────┤                        │
  │                            ╳ (process exits)        │
```

10 requests to the same host = 10 TCP handshakes + 10 TLS negotiations. On a 200ms RTT link, that's 4 seconds of pure overhead.

## Two Modes

### CLI mode (default)

One bash tool call, one request, one JSON response, process exits:

```
Agent ──→ afhttp GET https://api.example.com/users ──→ JSON stdout ──→ Agent
```

Default output: `response` or `error` — one JSON line, process exits. For streaming: `chunk_start` → `chunk_data...` → `chunk_end`. Use `--verbose` for diagnostic output (startup, request, progress, retry, redirect).

This is how most agent tool calls work — fire a request, read the result, move on.

### Pipe mode (`--mode pipe`)

For workflows that benefit from connection reuse, concurrent requests, or WebSocket:

```
Agent ──→ afhttp --mode pipe (stdin JSONL ←→ stdout JSONL) ──→ Agent
```

A long-lived process. The agent sends request/config/send/cancel/close commands as JSONL to stdin, reads responses from stdout. Connections stay open between requests. Multiple requests in-flight simultaneously. `close` triggers shutdown by cancelling active work, waiting briefly for terminal events, then emitting a final `close` acknowledgement.

## Architecture

```
CLI mode:                           Pipe mode:

  argv ──→ parse_args()              stdin ──────────→ Request Parser (JSONL)
             │                                           │
             ▼                                           ▼
        ┌────────────┐              ┌─────────────────────────────┐
        │  reqwest    │              │  Connection Pool Manager     │
        │  Client     │              │  pool[host1] ─→ conn(h2)    │──→ host1:443
        │  (single    │──→ server    │  pool[host2] ─→ conn(h2)    │──→ host2:443
        │   request)  │              │  pool[host3] ─→ conn(h1)    │──→ host3:80
        └────────────┘              └─────────────────────────────┘
             │                                           │
             ▼                                           ▼
        stdout (JSON)                stdout ←──── Response Writer (JSONL)
```

All runtime protocol output goes to stdout as JSON. stderr is not a protocol channel.

### Shared core

Both modes share the same handler, chunked streaming, and WebSocket code. CLI mode builds a single request from argv, sends it through the same `execute_request()` path, and collects output via the same `mpsc` channel — just stripping `id`/`tag` fields before writing.

### Concurrency (pipe mode)

```
stdin reader (main task)
  │
  ├─ parse request 1 ──→ spawn tokio task ──→ client.send() ──→ write stdout
  ├─ parse request 2 ──→ spawn tokio task ──→ client.send() ──→ write stdout
  ├─ parse request 3 ──→ spawn tokio task ──→ client.send() ──→ write stdout
  │
  └─ (continues reading stdin without blocking)
```

Each request is an independent tokio task. The stdin reader never blocks on HTTP I/O. Responses are written to stdout as they complete, identified by `id`.

## Design Principles

### Server errors are errors

If the server violates HTTP protocol (e.g. sends non-ASCII bytes in a header), `afhttp` surfaces this as `code: "error"` with `error_code: "invalid_response"`. No silent patching, no lossy fallbacks. The agent receives accurate information and decides how to react.

### Errors are structured, not human text

Every error carries `error_code` (machine-readable, stable), `error` (human-readable detail), and `retryable` (bool). Agents match on `error_code` — not string-parsing `message`.

### Secret fields are redacted in config echo

All stdout lines go through `agent_first_data::output_json()` for consistent single-line JSON formatting. For config output (`startup`, `config`), this also automatically redacts fields whose names end in `_secret` — so `key_pem_secret` never appears in plain text in the config echo.

Server response data (response bodies, headers, WebSocket messages) is passed through unmodified. Redaction does not apply to server-originated content.

### Header scope safety boundary

`defaults.headers_for_any_hosts` is global and applies to every outbound host. It is restricted to non-sensitive public headers only (for example `User-Agent`, `Accept`).

Any credential material (`Authorization`, API keys, cookies, bearer tokens) must be scoped with `host_defaults[host].headers` so secrets cannot be sent to unrelated domains.

### Agent-First Data naming conventions for fields

Field names carry meaning through suffixes:

| Suffix | Meaning | Example |
|--------|---------|---------|
| `_ms` | milliseconds | `duration_ms`, `retry_base_delay_ms` |
| `_s` | seconds | `timeout_connect_s`, `timeout_idle_s` |
| `_bytes` | byte count | `response_save_above_bytes`, `received_bytes` |
| `_file` | file path | `body_file`, `cacert_file`, `key_file` |
| `_base64` | base64-encoded bytes | `body_base64`, `data_base64` |
| `_pem` | inline PEM-format text | `cacert_pem`, `cert_pem`, `key_pem_secret` |
| `_secret` (at end) | sensitive value — auto-redacted in output | `key_pem_secret` |

Inline and file-path variants are mutually exclusive per slot: setting one clears the other in stored config. Inline takes precedence when both are present in a patch.

### CLI flags: long only, no abbreviations

CLI flags use long form only (`--header`, `--body`, `--timeout-idle-s`). No single-letter short flags (`-H`, `-b`). This is deliberate — agents read and write flags by name, not by memorized shortcuts. Long flags are self-describing and less error-prone in generated commands.

CLI flag names correspond to JSON field names with hyphens replacing underscores (e.g. JSON `timeout_idle_s` → CLI `--timeout-idle-s`, JSON `body_base64` → CLI `--body-base64`).

Boolean flags that default to false are bare flags (`--verbose`, `--chunked`, `--tls-insecure`). Boolean flags that default to true take an explicit value (`--response-parse-json false`, `--response-decompress false`).

### Output formats via `--output`

CLI mode supports three output formats via `--output json|yaml|plain`:

- **json** (default): Single-line JSON via `agent_first_data::output_json()`. `_secret` fields auto-redacted.
- **yaml**: Multi-line YAML via `agent_first_data::output_yaml()`. Field name suffixes stripped (`duration_ms` → `duration`), values formatted (`10485760` → `"10.0MB"`).
- **plain**: Logfmt via `agent_first_data::output_plain()`. Same suffix stripping and value formatting as YAML but single-line.

**Server response body is never modified.** Non-string body values (parsed JSON objects/arrays) are serialized to a JSON string before passing to yaml/plain formatters, so the formatters treat them as opaque strings. This ensures the agent receives exact server data regardless of output format.

### No `unwrap` / `expect` / `panic` anywhere in the codebase

`#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]` is enforced at crate level. Every error case is handled explicitly — either propagated as a structured `error` output to the agent, or (for truly impossible cases) handled with a hardcoded fallback string rather than a panic.

## Dependencies

| Crate | Purpose |
|-------|---------|
| `tokio` | Async runtime, stdin reader, task spawning |
| `reqwest` | HTTP client with connection pooling and HTTP/2 |
| `tokio-tungstenite` | WebSocket client (upgrade handshake, framed read/write) |
| `clap` | CLI argument parsing (derive API) |
| `agent-first-data` | Agent-First Data output serialization with automatic `_secret` redaction |
| `serde_json` | JSON parsing and serialization |
| `base64` | Body encoding/decoding |
| `uuid` | Process-unique download directory |

## Future

- **HTTP/3 (QUIC)** — eliminates TCP head-of-line blocking, 0-RTT reconnection. Waiting for `hyper-h3` stabilization.
- **WebSocket TLS config** — apply custom TLS settings to WebSocket connections (currently uses system root store only).
- **Request pipelines** — declare request dependencies (`"after": "req-1"`) for sequential workflows.
- **Response caching** — optional ETag/Last-Modified caching per URL.
