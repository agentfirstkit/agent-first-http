# Agent-First HTTP

Persistent HTTP client for AI agents — one request, one JSON line.

Supported platforms: macOS, Linux, Windows.

Modes (single entrypoint):
- `--mode cli` (default)
- `--mode pipe`
- `--mode mcp`
- `--mode curl`

`curl` opens a new TCP+TLS connection for every request. An agent making 20 calls to the same API pays 20 handshakes — on a 200ms RTT link, that's over 4 seconds of pure overhead before a single byte of useful work. `afhttp` is a long-lived process: connections stay open, concurrent requests share them, and the agent never thinks about transport.

## CLI Mode

The default mode — one request, one JSON response, exit:

```bash
afhttp GET https://api.example.com/users
# {"code":"response","status":200,"body":[...],"trace":{"duration_ms":120,...}}

afhttp POST https://api.example.com/users --body '{"name":"Alice","email":"alice@example.com"}'
# {"code":"response","status":201,"body":{"id":42},...}

afhttp GET https://api.example.com/data --header "Authorization: Bearer sk-xxx"
# {"code":"response","status":200,...}
```

Exit codes: `0` = got HTTP response (any status), `1` = transport error, `2` = invalid arguments.

## Pipe Mode

For long-lived sessions with connection reuse, concurrent requests, and WebSocket — use `afhttp --mode pipe`:

```bash
afhttp --mode pipe <<'EOF'
{"code":"config","defaults":{"headers":{"x-api-key":"sk-ant-xxx","anthropic-version":"2023-06-01"}}}
{"code":"request","id":"models","method":"GET","url":"https://api.anthropic.com/v1/models"}
{"code":"request","id":"usage","method":"GET","url":"https://api.anthropic.com/v1/usage"}
{"code":"request","id":"chat","method":"POST","url":"https://api.anthropic.com/v1/messages","body":{"model":"claude-opus-4-6","max_tokens":256,"messages":[{"role":"user","content":"Hello"}],"stream":true},"options":{"chunked":true,"chunked_delimiter":"\n\n"}}
EOF
```

**output:**
```json
{"code":"config","defaults":{"headers":{"x-api-key":"[redacted]","anthropic-version":"2023-06-01"}},...}
{"code":"response","id":"models","status":200,"body":{"data":[{"id":"claude-opus-4-6",...}]},"trace":{"duration_ms":92,"http_version":"h2","redirects":0,"remote_addr":"13.32.4.10"}}
{"code":"response","id":"usage","status":403,"body":{"error":{"type":"permission_error","message":"Your API key does not have permission"}},"trace":{"duration_ms":87,"http_version":"h2","redirects":0}}
{"code":"chunk_start","id":"chat","status":200,"headers":{"content-type":"text/event-stream"}}
{"code":"chunk_data","id":"chat","data":"event: content_block_delta\ndata: {\"delta\":{\"text\":\"Hello\"}}"}
{"code":"chunk_data","id":"chat","data":"event: content_block_delta\ndata: {\"delta\":{\"text\":\" there\"}}"}
{"code":"chunk_data","id":"chat","data":"event: message_stop\ndata: {}"}
{"code":"chunk_end","id":"chat","trace":{"duration_ms":834,"chunks":8}}
```

What just happened:

- **One bash call** — the heredoc sends all requests into one `afhttp` process; afhttp exits when stdin closes
- **Auth set once** — the `config` header applies to every subsequent request; nothing is repeated
- **Three requests fired without waiting** — `models`, `usage`, and `chat` all in-flight simultaneously
- **Connection reuse is automatic** — requests to the same host can reuse pooled connections without extra agent logic
- **Out-of-order responses** — `usage` arrived before `chat` finished; the agent matches by `id`
- **Streaming inline** — `chat` delivers events as they arrive, no buffering, no special setup
- **HTTP errors are data** — `usage` returned 403; afhttp delivers it as `code: "response"` with `status: 403`; the agent checks `status`, not exception types or text patterns

## MCP Mode

`afhttp --mode mcp` runs as a [Model Context Protocol](https://modelcontextprotocol.io) server, letting AI tools like Claude Desktop make HTTP requests directly:

```json
{
  "mcpServers": {
    "afhttp": { "command": "afhttp", "args": ["--mode", "mcp"] }
  }
}
```

Claude can then call `http_request` and `http_config` tools. See [docs/mcp.md](docs/mcp.md) for the full setup guide.

## curl Compatibility

Use explicit curl mode. afhttp understands a subset of curl flags and returns structured JSON:

```bash
afhttp --mode curl -X POST https://api.example.com/users \
  -H "Authorization: Bearer sk-xxx" \
  -d '{"name":"Alice"}'
# {"code":"response","status":201,"body":{"id":42},...}
```

## Install

**macOS / Linux — Homebrew**

```bash
brew install cmnspore/tap/afhttp
```

**Windows — Scoop**

```powershell
scoop bucket add cmnspore https://github.com/cmnspore/scoop-bucket
scoop install afhttp
```

**Any platform — Cargo**

```bash
cargo install agent-first-http
```

## Docs

- [CLI Manual](docs/cli.md) — CLI, MCP, and curl compat modes
- [MCP Reference](docs/mcp.md) — MCP tool reference and Claude Desktop setup
- [Protocol Reference](docs/reference.md) — full field specification
- [Testing Strategy](docs/testing.md) — layered tests, coverage gate, regression policy
- [Design](docs/design.md) — architecture and principles

## License

MIT
