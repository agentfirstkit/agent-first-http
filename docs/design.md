# Design Principles

This document captures the philosophy and codebase-wide conventions that hold across all of `afhttp`. For the concrete architecture (roles, CLI surface, SDK, profile model, artifacts, health/capabilities endpoints, error codes), see [architecture.md](architecture.md).

## Problem

AI agents often fail before they can reason about a page. A simple shell fetch may return a redirect chain, a 403 body, an empty HTML shell that needed JavaScript, a TLS failure, a timeout, or a large binary response. A human can inspect the terminal and decide what to try next; an agent needs the acquisition facts as structured data.

`afhttp`'s job is deterministic URL acquisition: make an attempt, preserve the facts and artifacts, and report failures in a shape the agent can branch on.

The primary cost of opaque acquisition is not milliseconds. It is that the agent loses the state it needs to answer: did a real HTTP response exist, what URL did it end on, what body was available, was the failure transport-level, and is a browser-backed path the right escalation?

## Principles

### Acquisition facts over interface mimicry

`afhttp` exposes what happened during URL acquisition: request preview, status, final URL, headers, body or `_file` artifact, rendered DOM, observation snapshot, network timeline, redirect and timing trace, and typed transport failures. The output is shaped for agents to branch on, not for humans to read. The tool does not mimic browser dev tools or any other human-facing interface for its own sake.

### Observation is mechanical, not interpretive

`observation.json` exists because full HTML is often too large and screenshots are not enough for an agent to plan. The artifact may project the accessibility tree, DOM geometry, frame structure, visible text, element states, and available mechanical actions. It must not label intent ("login page"), rank importance ("best button"), or choose selectors for the agent. Refs in the observation are scoped to one snapshot and are aids for later CDP resolution, not durable automation handles.

### Network capture is evidence, not extraction

Modern pages often carry the useful data in XHR, fetch, GraphQL, service-worker, or iframe requests. `network.json` preserves that evidence with request/response metadata, timing, initiator, headers, sizes, and optional captured bodies. It may record mechanical payload hints such as JSON validity or GraphQL operation name; it must not infer business meaning from the payload.

### No page-action wrappers — raw CDP for interaction

`afhttp` exposes acquisition (fetch a URL, capture the facts) and a raw CDP passthrough (`afhttp cdp`). It deliberately does **not** ship a Playwright/Selenium-style action library: no `click`, no `type`, no `select`, no per-element automation verbs. Complex interaction sequences are the agent's job, expressed as raw CDP recipes against a live host. This keeps the surface small and honest — the tool acquires and observes; it does not pretend to be a scripting framework.

There is exactly one exception, and it has a precise admission test: **afhttp wraps an interaction only when the browser's security model makes it impossible for the agent to do it from JavaScript.** File injection is the sole case. A `<input type=file>` value cannot be set by script (`el.value = path` is forbidden), so there is no `Runtime.evaluate` recipe an agent could write — the only path is the privileged CDP method `DOM.setFileInputFiles`. So `afhttp upload` exists: it locates the input by selector and injects the file directly, with no synthetic click and no file-chooser dialog. `click` and `type` do *not* qualify — an agent can dispatch those from `Runtime.evaluate` — so they stay recipes. The test is "JavaScript fundamentally cannot," not "it would be convenient."

Downloads follow the same shape from the other direction: a download is not a verb but an event the browser produces inside a session the agent is already driving (after login and interaction, often with no addressable URL). So there is no `download` command. The host captures browser-initiated downloads (`Browser.setDownloadBehavior`) into the profile's own download directory regardless of what triggered them; `fetch` reports a `download_file` when a navigation turns into a download, and interaction-triggered downloads are retrieved with `afhttp profile downloads <name>`, a read-only local listing of the profile's captured download files. The agent never leaves its session to "run a download."

### Sessions are state, not speed

A persistent profile (held by `afhttp host`) lets an agent reuse cookies, localStorage, and other browser state across many fetches against the same target. Connection pooling and tab reuse are convenient side effects; raw throughput is not the design center. Sessions exist because acquisition often requires identity, not because they are faster.

Identity is therefore opt-in, not the baseline. Most acquisition needs no identity at all and never touches a profile or a browser: the default path is a browserless HTTP fetch, a rendered browser is an escalation taken only when the page needs JavaScript, and a persistent profile is a further escalation taken only when the target requires a logged-in identity. An agent never pays for state, or even for a browser, that the task does not require.

### One host, one identity — scale by multiplying hosts

An `afhttp host` binds exactly one profile: one browser process tree, one identity. There is no multi-profile host, no in-process "session" or browser-context primitive, and no per-fetch fresh context. A host never multiplexes identities internally. To run many identities in parallel, run many hosts.

This is a deliberate rejection of the obvious resource optimization — packing many in-memory browser contexts into one process. That optimization does buy cheap cookie/storage isolation, but it cannot deliver **human takeover**, which is the capability that justifies a persistent identity in the first place. Two facts force the issue:

1. **Takeover must be OS-level.** A human steps in to complete a login or solve a challenge by driving a real headful browser through real-display takeover (currently backed by KasmVNC). Synthetic CDP input (`Input.dispatch*`) is insufficient for real login flows in practice, so observation-over-CDP is read-only and the interactive path is OS-level only.
2. **Headful is a process-level property.** Every browser context inside one process shares one display and one input focus. You cannot make one context headful and another headless, and you cannot scope an OS-level screen to a single context.

Together these mean that isolating *which* identity the operator sees and types into, inside a single process, requires racing the window manager against focus-stealing popups from the other identities. A lost race misroutes keystrokes — including credentials — into the wrong identity's window, which would break the isolation invariant below. So takeover is single-identity-per-host **by construction**, and identities never share a process.

The resource cost of this choice is small and bounded. Multiple hosts coexist in a single container: each gets its own process, profile directory, virtual display, and ports, all allocated automatically with no collision. Hosts in one container share only the cgroup budget and the network namespace (hence one egress IP); reach for separate containers only when an identity needs hard resource isolation or a distinct egress IP. Per-identity memory (one browser, the live pages) is paid either way — the only thing multiplexing-in-process would have saved is one process's fixed overhead per identity, which does not justify a takeover path that can leak keystrokes across the isolation boundary.

### Browsing environments are isolated

`afhttp` is not a remote control for the user's existing browser. It runs its own browser process tree against its own state directory, and never reads or writes data that belongs to anything else on the machine.

The invariant has three layers:

1. **No interaction with system-owned browser data.** The host never reads, writes, copies, references, or imports from the user's real browser profiles (`~/.config/google-chrome/`, `~/Library/Application Support/Firefox/`, `%LOCALAPPDATA%\Google\Chrome\`, etc.). The same applies to system keychains, OS cookie stores, and shared browser binaries' default state. Agents that need an identity migrate it explicitly through a fresh login inside an `afhttp` profile.

2. **Each browsing environment is independent.** One `afhttp host` instance runs one browser process tree against one profile directory. Two hosts on the same machine — even with the same backend binary — share no cookies, no cache, no localStorage, no service workers, no in-flight tabs. Crashing or killing one host cannot leak state to another.

3. **Each profile is a sandbox.** All persistent state for a profile lives under that backend-scoped profile directory (`$XDG_DATA_HOME/afhttp/profiles/<backend>/<name>/` on Linux/macOS, the platform equivalent on Windows). The cookie jar, browser user-data-dir, lockfile, and metadata are all in there. Cross-profile reads and writes are explicit programming errors — never accidental. Profile A's authenticated session cannot leak into profile B even if they target the same domain, and the same logical name under different backends is a different profile. Ephemeral profiles live in a tempdir and are wiped on host exit; they can never persist beyond a single host process.

What the invariant *does not* claim: the engine itself (Chromium, Firefox) still reads system fonts, the OS timezone, and the OS locale, because those are baked into the rendering pipeline. All browser backends launch through an explicit `env_clear` + allowlist path; only the minimal runtime variables and `--engine-env` opt-ins reach the engine. Backends like `fingerprint-chromium` and `camoufox` exist specifically to spoof browser fingerprint surfaces; the regular `chromium` backend honestly leaks engine-level surfaces such as fonts and graphics capabilities, and the documentation says so. Outbound network traffic uses the host's network stack and DNS — proxies are a deliberate opt-in, never inherited from `HTTP_PROXY`/`HTTPS_PROXY` without an explicit flag.

Concrete rules the implementation enforces:

- The `--profile <name>` argument is validated to be a flat name; path separators and `..` are rejected so a malicious name cannot escape the profile root.
- The cookie jar lives inside the profile directory. `--cookie-jar <path>` exists as a testing override only; the default path is always profile-internal and cannot be redirected to point at a different profile's jar in normal use.
- Browser traffic never inherits ambient `HTTP_PROXY` / `HTTPS_PROXY`; use `--proxy-url` explicitly. Browser state never inherits a default engine profile because afhttp always supplies `--user-data-dir`. Browser processes never inherit the parent environment wholesale; every backend uses `env_clear` plus the documented allowlist and explicit `--engine-env` entries.
- Profile deletion is local-only (`afhttp profile delete <name>`); the host's HTTP/CDP surface does not expose a "destroy any profile" endpoint, so a stolen bearer token cannot wipe another profile's state.

### Server errors are errors

If the server violates HTTP protocol (e.g. sends non-ASCII bytes in a header), `afhttp` surfaces this as `code: "error"` with a stable `error_code`. No silent patching, no lossy fallbacks. The agent receives accurate information and decides how to react.

### Errors are structured, not prose

Every error carries `error_code` (machine-readable, stable enum), `error` (human-readable detail for logs), and `retryable` (bool). Agents match on `error_code` only — never string-parsing the human message. The enum is documented in [architecture.md §11](architecture.md).

### Secrets are redacted in tool-originated output

Configuration echo, log lines, and trace output go through `agent_first_data::output_json_with()` with the `_secret` suffix redaction policy. Fields named `*_secret` (e.g. `key_pem_secret`) are replaced with `"[redacted]"` in stdout. Server response data (response bodies, headers) passes through unmodified — redaction does not apply to the `body` artifact.

Network logs are different: they are a tool-originated capture of both browser requests and server responses, and can contain cookies, bearer tokens, and `Set-Cookie`. `network.json` redacts credential-bearing headers by default. Agents that need byte-for-byte traffic capture must opt out explicitly with `--no-network-redact`, which can expose tokens and PII. `--capture-ws` and `--capture-sse` have the same risk for frame/event payloads.

### Agent-First Data field naming

Field names carry meaning through suffixes. The agent can predict the shape of a value from its key.

| Suffix | Meaning | Example |
|--------|---------|---------|
| `_ms` | milliseconds | `duration_ms`, `retry_base_delay_ms` |
| `_s` | seconds | `timeout_connect_s`, `timeout_idle_s` |
| `_bytes` | byte count | `response_max_bytes`, `received_bytes` |
| `_file` | file path | `body_file`, `screenshot_file`, `cacert_file` |
| `_url` | URL string | `final_url`, `capabilities_url` |
| `_base64` | base64-encoded bytes | `body_base64`, `data_base64` |
| `_pem` | inline PEM text | `cacert_pem`, `cert_pem`, `key_pem_secret` |
| `_secret` (suffix) | sensitive — auto-redacted | `key_pem_secret`, `token_secret` |

Inline and file-path variants are mutually exclusive per slot: setting one clears the other.

### CLI flags: long form only

CLI flags use long form only (`--render`, `--endpoint-url`, `--timeout`). No single-letter shorts (`-r`, `-e`, `-t`). Long flags are self-describing and less error-prone in agent-generated commands.

CLI flag names correspond to JSON field names with hyphens replacing underscores (e.g. `--browser-bin` ↔ JSON `browser_bin`).

Boolean flags are bare presence toggles, never `on|off` values. A behavior that is on by default is turned off with a `--no-x` flag (`--no-cookie-jar`, `--no-network-redact`, `--no-health`); a behavior that is off by default is turned on with a bare `--x` flag (`--tls-insecure`, `--capture-ws`).

### Output is always single-line JSON

CLI mode has no `--output` flag and no alternate rendering. Every command prints exactly one line of JSON on stdout via `agent_first_data::output_json_with()` with `_secret` redaction; a failure prints one JSON object carrying `error_code`, `error`, and `retryable`. Page bytes and captured data go to artifact files referenced by `*_file` fields — the envelope itself is never reshaped per request.

### No `unwrap` / `expect` / `panic` in the crate

```rust
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
)]
```

Every error case is handled explicitly: propagated as a structured `error` to the agent, or (for truly impossible cases) handled with a hardcoded fallback rather than a panic. Tests bypass the lint via `#[cfg(test)]`.

### `print_stdout` / `print_stderr` are denied at crate level

All output passes through the protocol writer, which guarantees single-line JSON on stdout. Direct `println!` / `eprintln!` would bypass redaction and structure. The lint enforces this.

## Cross-References

- [Architecture](architecture.md) — concrete roles, CLI surface, SDK, profile model, artifacts, health/capabilities endpoints, error codes.
- [Overview](overview.md) — the user-facing introduction.
- [Testing](testing.md) — test strategy and coverage gates.
