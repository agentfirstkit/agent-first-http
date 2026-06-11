# Overview

`afhttp` is a URL acquisition tool for AI agents. Give it a URL; get back the rendered page and the artifacts (HTML, screenshot, network and console logs, DOM observation) an agent needs to decide what to do next.

Supported platforms: macOS, Linux, Windows.

## Install

```bash
brew install agentfirstkit/tap/afhttp                                             # macOS / Linux
scoop bucket add agentfirstkit https://github.com/agentfirstkit/scoop-bucket \
  && scoop install afhttp                                                          # Windows
cargo install agent-first-http                                                    # any platform
```

Install the embedded Agent Skill for Codex, Claude Code, and opencode straight from the
binary (check with `afhttp skill status`, remove with `afhttp skill uninstall`):

```bash
afhttp skill install
```

## What `afhttp` is for

The hard part for an agent is not fetching bytes. It is that many useful URLs do not turn into a usable page from a simple shell request — they require JavaScript rendering, cookies, session state, or a real browser fingerprint. When that happens, a human can open a browser and inspect; an agent needs the same facts as data it can branch on.

`afhttp` covers the whole range:

- **Plain HTTP fetch** when the page works without a browser.
- **Browser-backed fetch** when it does not, producing rendered HTML, an agent-readable observation snapshot, screenshot, and network/console logs as artifacts.
- **Deep network capture** when the visible page is only chrome and the useful data arrives through XHR/fetch/GraphQL calls.
- **Raw CDP escape hatch** when the agent needs to drive the browser directly (DOM inspection, form submission, custom waits) without going through any "click/type" abstraction layer.
- **Human takeover** when a human needs to step in on the same browser the agent is using — `afhttp fetch <url> --takeover` on a takeover-ready host (auto-discovered for the standard local container) hands a person the browser for a login, 2FA, captcha, or security challenge, then lets the agent re-fetch the same tab once the wall is cleared.
- **Host health/capabilities and local profile tools** so agents can discover backend support and operators can list, inspect, retrieve captured downloads, prune, or delete persistent profiles.

The agent never has to parse human-readable error messages. Every output is structured JSON. Every failure carries a stable `error_code`. See [architecture.md](architecture.md) for the full contract.

## Two roles

| Role | Command | What it does |
| --- | --- | --- |
| **browser-host** | `afhttp host` | Long-running foreground process. Holds Chromium + a profile. Exposes a CDP endpoint and optional real-display takeover. |
| **agent-driver** | `afhttp fetch`, `afhttp upload`, `afhttp cdp`, `afhttp panel`, `afhttp health`, `afhttp capabilities`, `afhttp profile`, `afhttp tabs`, or the Rust SDK | Short-lived client. Connects to a host's endpoint when needed, does work, writes artifacts locally. |

Hosts and drivers are independently locatable. Run the host where the browser needs to be (residential IP, GUI machine, datacenter); run the driver wherever the agent runs. Connectivity is your mesh's problem, not `afhttp`'s.

The CLI has 11 commands: `host`, `fetch`, `upload`, `cdp`, `panel`, `health`, `capabilities`, `profile`, `tabs`, `skill`, and `container`.

## Quick start

### One-shot fetch, no host

The shortest path starts with pure HTTP. `--render none` never starts a browser. With the default `--render auto`, afhttp tries the same HTTP fast path first and only starts an inline ephemeral host if the response needs browser rendering.

```bash
afhttp fetch https://example.com
```

```json
{
  "code": "fetch",
  "status": 200,
  "final_url": "https://example.com/",
  "body_file": "/tmp/afhttp-out/<id>/body.html",
  "rendered_html_file": "/tmp/afhttp-out/<id>/rendered.html",
  "network_file": "/tmp/afhttp-out/<id>/network.json",
  "trace": {
    "render_decision": "browser",
    "render_mode": "auto",
    "render_used": true,
    "current_stage": "complete",
    "duration_ms": 820,
    "timeout_ms": 30000,
    "stages": [
      {"name": "navigate", "status": "ok", "duration_ms": 340},
      {"name": "capture_body", "status": "ok", "duration_ms": 12}
    ]
  }
}
```

### Long-running host + remote fetch

For real workflows: start one `afhttp host`, drive it from anywhere.

```bash
# On the host machine (or in a systemd unit, tmux pane, docker container — your choice).
# A non-loopback listener (anything other than 127.0.0.1 / a unix: socket) serves
# full browser control over /cdp, so a --token-secret is required — the host refuses to
# bind otherwise:
export AFHTTP_TOKEN_SECRET=$(head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -d '=\n')
afhttp host --listen tcp:0.0.0.0:9222 --profile work --display headless \
            --token-secret "$AFHTTP_TOKEN_SECRET"

# From the agent's machine:
afhttp fetch --endpoint-url ws://host.mesh.internal:9222 --token-secret "$AFHTTP_TOKEN_SECRET" \
             --render auto --wait auto \
             --want rendered_html,observation,screenshot,network,console \
             --network-bodies xhr \
             https://target.example.com/dashboard
```

The token secret gates `/cdp`, display takeover, and `/profile`; bind `tcp:127.0.0.1:<port>` or a `unix:` socket instead when the host and driver share a machine and you want to skip it. The profile persists across host restarts. Cookies and localStorage acquired in one fetch are available to the next.

### Raw CDP escape hatch

When `fetch` is not enough — for example, evaluating arbitrary JavaScript in the target page:

```bash
afhttp cdp Runtime.evaluate \
  --endpoint-url ws://host.mesh.internal:9222 \
  --tab abc123 \
  --params '{"expression":"document.querySelectorAll(\"a\").length","returnByValue":true}'
# {"result":{"type":"number","value":42}}
```

No `click` / `type` / `navigate` wrappers. The agent talks raw CDP; `afhttp` only forwards.

### Check health and capabilities

Before assigning work to a host, an agent or supervisor can ask what is alive and what the backend supports:

```bash
afhttp health --endpoint-url ws://host.mesh.internal:9222
# {"code":"health","status":"ok","backend":{"family":"chromium","connected":true},...}

afhttp capabilities --endpoint-url ws://host.mesh.internal:9222
# {"code":"capabilities","artifacts":{"observation":{"supported":true},...},...}
```

`/health` is for readiness. `/capabilities` is for planning artifact requests and avoiding predictable `backend_unsupported` warnings.

### Human takes over (real-display takeover)

When the agent hits a login, 2FA, or captcha wall, run a takeover fetch. With
the default local `afhttp container install` host running, `fetch --takeover`
discovers its endpoint and token automatically:

```bash
afhttp fetch "$URL" --takeover
```

If the warmed profile already reaches the target, `fetch --takeover` just
returns the content. Otherwise it keeps a persistent tab open and returns a
`next_action`:

```json
{
  "code": "fetch",
  "next_action": {
    "kind": "human_takeover",
    "takeover_url": "http://host.mesh.internal:9222/takeover/panel?handoff=…",
    "takeover_url_expires_at_rfc3339": "2026-06-11T08:15:00Z",
    "takeover_url_ttl_s": 900,
    "takeover_url_scope": "takeover",
    "recommended_command": "afhttp fetch \"$URL\" --tab page-7 --endpoint-url ws://host.mesh.internal:9222 …"
  }
}
```

A human opens the `takeover_url` in a local browser and drives the real display
(Brave on KasmVNC). Once they are past the wall, the agent runs the
`recommended_command` to re-fetch the same tab and continue. The agent can stay
CDP-attached the whole time. `afhttp panel --endpoint-url …` prints the same display
URL directly. See [architecture.md §9](architecture.md) for the risk-control
honest assessment. `fetch --takeover` needs a running takeover-ready host (the
standard local `afhttp-host` is auto-discovered, remote/custom hosts use
`--endpoint-url` or `AFHTTP_ENDPOINT_URL`) and a browser render (`--render auto`
or `always`); it does not auto-create containers. Without `--profile`, takeover
switches to a persistent profile derived from the URL's registrable domain.

### Manage persistent profiles

Persistent browser profiles are local disk identities. Operators can inspect and clean them up without guessing which temp directory belongs to which host:

```bash
afhttp profile list
afhttp profile info work --backend brave
afhttp profile lock-status work --backend brave
afhttp profile downloads work --backend brave
afhttp profile prune --older-than 30d --dry-run
afhttp profile delete old-work --backend brave --confirm old-work
```

Profile lifecycle commands are local-only; `downloads` only lists captured files, and destructive commands refuse locked profiles.
Profile names are logical and persistent storage is backend-scoped, so
`work` under Brave and `work` under Chromium are different directories.

## From Rust

The library is a thin SDK over the same endpoint protocol. It is **not** an embedded browser engine; it talks to a running `afhttp host` over CDP.

```rust
use afhttp::{Client, RenderMode, Wait, Artifact};

let client = Client::connect("ws://host.mesh.internal:9222")?;

let result = client.fetch("https://target.example.com")
    .render(RenderMode::Auto)
    .wait(Wait::Auto)
    .timeout(Duration::from_secs(30))
    .want([Artifact::RenderedHtml, Artifact::Observation, Artifact::Screenshot])
    .send()
    .await?;
// result.rendered_html_file -> path on this machine's disk
// result.observation_file -> agent-readable page snapshot

// Dev / test convenience: spawn a host subprocess, use it, kill on drop.
// Requires the `host` feature — pure `features = ["sdk"]` consumers
// connect to an afhttp host started separately.
let local = Client::inline_ephemeral().await?;
```

Consumers depend on the crate with `default-features = false, features = ["sdk"]` and link only the client weight — no Chromium, no chromiumoxide, no browser-launch code.

## Backends

The protocol layer is CDP-generic. `afhttp host` knows how to launch:

| Backend | Notes |
| --- | --- |
| Chromium / Chrome / Edge / Brave | Full support: all artifacts, observation, network body capture, optional real-display takeover currently backed by KasmVNC, multi-attach. |
| chrome-headless-shell | Same as Chromium — Google's slimmer headless distribution, identical CDP surface. Useful when the full browser is unavailable. |
| fingerprint-chromium | Same capability matrix as Chromium, including optional real-display takeover, with engine-level fingerprint spoofing (UA, WebGL, canvas, CDP-detection evasion). The host derives a stable seed from the profile path so identity stays per-profile. |
| Lightpanda | HTML / text / network metadata / console / limited observation only — no screenshot, no display takeover (no rendering). |
| Camoufox (via foxbridge) | Firefox stealth fork driven through the [foxbridge](https://foxbridge.vulpineos.com/) CDP→Juggler proxy. Same CDP subset as Lightpanda — no chromium screenshot — but optional real-display takeover works because the human drives the real X display. |
| Any other CDP-compatible browser | Launch it yourself; drivers connect via `--endpoint-url`. |

Unsupported per-artifact operations return per-artifact warnings (`backend_unsupported`), not whole-fetch failures.

## Cross-spore collaboration

`afhttp` does not operate in isolation. Here is how it fits with the rest of the agentfirstkit suite:

### afmail: CAPTCHAs and mail-borne login flows

When a page requires an emailed verification link or OTP, hand off to **afmail** rather than polling IMAP yourself:

1. `afhttp fetch` navigates to the login form and submits credentials.
2. The page sends an email. The agent calls `afmail triage` (or `afmail fetch`) to find the message, extract the link or code.
3. The agent feeds the link/code back to `afhttp` via `--evaluate-after-wait` or a subsequent `afhttp fetch`.

`afhttp` handles the browser-side state; `afmail` handles the mailbox-side state. They share no storage and are always driven by the agent — never by each other.

### afpay: profile reuse for payment-gated pages

`afhttp` holds the browser session (cookies, localStorage) that proves the agent is a logged-in subscriber. **afpay** handles the wallet and transaction side. The coordination point is the persistent profile:

- Run `afhttp host --profile <name>` before any payment-gated fetch.
- After `afpay` completes a purchase, `afhttp fetch --endpoint-url <host>` uses the same host identity and inherits the session cookies set by the checkout flow.
- Never share `--cookie-jar` paths across profiles — the isolation invariant requires the jar to live inside the active profile directory.

### afdata: field naming alignment

`afhttp` response fields follow **afdata** suffix conventions (suffix-typed names: `_file`, `_ms`, `_url`). When an agent passes afhttp artifacts to afdata for extraction, the field shapes should be predictable without a schema lookup. If you add new fields to fetch responses, match the suffix table in the afdata SDK docs.

## Docs

- [Architecture](architecture.md) — the canonical contract: roles, CLI surface, profile model, artifacts, health/capabilities endpoints, human takeover, backends, error codes, SDK.
- [Design Principles](design.md) — codebase-wide conventions (field naming, structured errors, output formats, no-panic policy).
- [CLI Reference](cli.md) — flag-by-flag reference for the `afhttp` binary.
- [Protocol Reference](reference.md) — output schemas for fetch, cdp, health, capabilities, and profile results.
- [Testing](testing.md) — test strategy and coverage gates.

## License

MIT
