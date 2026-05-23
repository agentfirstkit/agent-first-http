# Agent-First HTTP

An HTTP tool for AI agents — one request in, one line of structured JSON out.

## The problem: curl was built for people, not agents

Agents need to talk to the web: call an API, download a file, stream a response. The usual tool is `curl`, but `curl` was built for a person at a terminal.

Its output is text meant for human eyes. A failure is a sentence you read, not data you can branch on — and a timeout, a refused connection, and a bad URL all look different. An agent ends up scraping that text and guessing what went wrong. And because `curl` starts fresh every time, an agent making many calls pays the connection cost again and again.

## What it does: one request, one JSON line

Agent-First HTTP makes the same web requests, but reports back in a form an agent can act on. Every request produces one JSON line. Every failure is a JSON event with a stable, named error code. Success or failure, the agent gets data — never prose.

- **One request, one JSON line.** Status, headers, body, and timing all come back as structured data.
- **Failures are data too.** Timeouts, bad URLs, refused connections — all structured `{"code":"error",...}` events with stable error codes.
- **Stays connected.** A long-lived pipe mode reuses connections, runs many requests at once, and streams responses as they arrive.
- **Speaks curl.** A curl-compatible mode understands common `curl` flags and returns the same structured JSON.

## Where to use it: API calls, streaming responses, and bursts of requests

- **An agent calling REST APIs** — it checks a `status` field instead of parsing text, and handles errors by code.
- **Streaming responses** — server-sent events and chunked replies arrive as structured events, live.
- **A burst of requests to one host** — pipe mode keeps the connection warm and runs them concurrently.
- **Replacing `curl` in an agent's toolset** — similar flags, but output it can actually read.

## Install

```bash
brew install agentfirstkit/tap/afhttp   # macOS / Linux
cargo install agent-first-http          # any platform
```

## Docs

- [Overview](docs/overview.md) — the full guide: every mode, with examples
- [CLI](docs/cli.md) — command and flag reference
- [Protocol Reference](docs/reference.md) — the complete field specification
- [Design](docs/design.md) — architecture and principles
- [Testing](docs/testing.md) — test strategy and coverage

## License

MIT
