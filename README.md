# Agent-First HTTP

Give your AI agent its own private browser — so it reads the real page, past logins and bot walls, without ever touching yours.

> **Ask your agent:** "Open this link and pull the pricing table out for me."

## What problem does this solve?

Agents are bad at opening pages. Ask one to read a specific URL and it tends to:

- answer from a **search-result guess** instead of the page you actually named,
- hand back an **empty app shell** because the page needed JavaScript it never ran, or
- mistake a **bot check or login wall** for the real content.

afhttp fixes this by loading the actual URL itself — falling back to a real
browser of its own to render JavaScript when the page needs one — and returns
the page as files the agent can inspect, so it answers from verified content
instead of a guess.

That browser is **fully isolated**: it runs separately from the browser you use
every day and never touches your cookies, logins, or history. When a page needs
a login, captcha, or 2FA, you can take over that same isolated browser, clear
the wall yourself, and let the agent continue — without ever mixing it into your
own session.

## The basics: hand it a URL, get the page back as data

Give afhttp a URL; it writes the page to disk and prints one line of JSON saying what it got:

```bash
$ afhttp fetch https://example.com
{"kind":"result","result":{"code":"fetch","request_url":"https://example.com","status":200,"final_url":"https://example.com/","body_file":"/tmp/afhttp-out/<id>/body.html"},"trace":{}}
```

That is the whole job: **hand it a URL, get the page back as files an agent can read** — never a terminal blob to scrape. CLI output follows AFDATA protocol v1: command data is in `result`; failures use `kind: "error"` with stable `error.code` values rather than guesses.

By default afhttp sends a **plain HTTP request**, returns the raw `body_file`,
and only starts a **real browser** when the page actually needs one (`--render
none` forces the fast path, `--render always` forces the browser, `--render
auto` decides). A browser-backed fetch automatically captures more of what a
human would look at — an agent-oriented composed page view (`content.md`, the
one to read first), rendered HTML, a screenshot, a DOM observation, the network
and console logs — each a flat `*_file` field on the same JSON, never nested:

```json
{
  "kind": "result",
  "result": {
    "code": "fetch",
    "request_url": "https://example.com",
    "status": 200,
    "final_url": "https://example.com/",
    "body_file": "/tmp/afhttp-out/<id>/body.html",
    "content_file": "/tmp/afhttp-out/<id>/content.md",
    "content_json_file": "/tmp/afhttp-out/<id>/content.json",
    "rendered_html_file": "/tmp/afhttp-out/<id>/rendered.html",
    "text_file": "/tmp/afhttp-out/<id>/text.txt",
    "screenshot_file": "/tmp/afhttp-out/<id>/page.png",
    "network_file": "/tmp/afhttp-out/<id>/network.json",
    "console_file": "/tmp/afhttp-out/<id>/console.json",
    "observation_file": "/tmp/afhttp-out/<id>/observation.json"
  },
  "trace": {}
}
```

## What afhttp covers

The hard part for an agent is not fetching bytes. It is that many useful URLs
do not turn into a usable page from a simple shell request — they need
JavaScript rendering, cookies, session state, or a real browser fingerprint.
Where a human would open a browser and inspect, an agent needs those same facts
as data it can branch on. afhttp covers the whole range:

- **Plain HTTP fetch** when the page works without a browser.
- **Browser-backed fetch** when it does not — rendered HTML, an agent-readable observation snapshot, screenshot, and network/console logs as artifacts.
- **Deep network capture** when the visible page is only chrome and the useful data arrives through XHR/fetch/GraphQL calls.
- **Raw CDP escape hatch** when the agent needs to drive the browser directly (DOM inspection, form submission, custom waits) without going through any "click/type" abstraction layer.
- **Human takeover** when a person needs to step in on the same browser the agent is using — `afhttp fetch <url> --takeover` on a takeover-ready host (auto-discovered for the standard local container) hands a person the browser for a login, 2FA, captcha, or security challenge, then lets the agent re-fetch the same tab once the wall is cleared.
- **Host health/capabilities and local profile tools** so agents can discover backend support and operators can list, inspect, retrieve captured downloads, prune, or delete persistent profiles.

The agent never has to parse a human-readable error message. Every CLI output
is an AFDATA protocol-v1 JSON event, and every failure carries a stable
`error.code`. See [docs/architecture.md](docs/architecture.md) for the full contract.

## Two roles: host and driver

afhttp splits into two roles that are independently locatable:

| Role | Command | What it does |
| --- | --- | --- |
| **browser-host** | `afhttp host` | Long-running foreground process. Holds Chromium + a profile. Exposes a CDP endpoint and optional real-display takeover. |
| **agent-driver** | `afhttp fetch`, `afhttp upload`, `afhttp cdp`, `afhttp panel`, `afhttp health`, `afhttp capabilities`, `afhttp profile`, `afhttp tabs`, or the Rust SDK | Short-lived client. Connects to a host's endpoint when needed, does work, writes artifacts locally. |

Run the host where the browser needs to be (residential IP, GUI machine,
datacenter); run the driver wherever the agent runs. Connectivity is your mesh's
problem, not afhttp's. The CLI has 11 commands: `host`, `fetch`, `upload`,
`cdp`, `panel`, `health`, `capabilities`, `profile`, `tabs`, `skill`, and
`container`.

## Running it: inline for a one-shot, a host for sessions

`afhttp fetch <url>` with no `--endpoint-url` runs **inline** — it spins up a sandboxed browser for that one fetch and tears it down. Zero setup; use it for stateless, one-shot acquisition.

For state that outlives a single fetch — a reused login, a warmed profile, human takeover — run a long-lived **host** in a container (the isolation boundary). One command builds the image from a recipe embedded in the binary and runs it (Docker, Podman, or Apple `container`, auto-detected — override with `--runtime`); it prints structured connection metadata and a ready-to-run driver command without exposing the long-lived host token by default:

```bash
afhttp container install
```

`container install` is takeover-ready by default (Brave + KasmVNC + an
ephemeral initial profile + a 2g `/dev/shm`). A takeover fetch auto-discovers
the standard local `afhttp-host`, switches to a persistent per-site profile
derived from the URL, and lets a human clear a wall on the same browser the
agent drives:

```bash
afhttp fetch "$URL" --takeover
```

If the warmed profile already reaches the target, `fetch --takeover` just returns
the page. Otherwise it keeps a persistent tab open and returns a `next_action`
with `kind: "human_takeover"`, a `takeover_url` for a human to open, and a
`recommended_command` that re-fetches the same `--tab` once the wall is cleared.
`fetch --takeover` needs a running host (auto-discovered locally, or supplied
with `--endpoint-url` / `AFHTTP_ENDPOINT_URL`) and a browser render
(`--render auto` or `always`); it does not auto-create containers.

Building from a source checkout instead? Use `afhttp container install --from-source`
(or `docker compose -f container/docker/compose.yaml up --build` when driving the
runtime directly). See [docs/deployment.md](docs/deployment.md) for the full
container setup, upgrades, and security posture.

## Browser backends: meet each site with the engine it demands

afhttp is not "headless Chromium." How hard a site fights back decides which engine actually reaches it, so afhttp drives a whole spectrum behind one CDP contract — pick one with `--browser` (or point `--browser-bin` at a binary):

- **chromium / chrome** — the default: full rendering, screenshots, downloads, all artifacts, observation, network body capture, and optional real-display takeover, with multi-attach.
- **chrome-headless-shell** — Google's slimmer headless distribution with an identical CDP surface: a lean build for fast, low-overhead fetches, and useful when the full browser is unavailable.
- **fingerprint-chromium** — the Chromium capability matrix (including optional real-display takeover) plus engine-level fingerprint spoofing (UA, WebGL, canvas, CDP-detection evasion) for bot-walled sites; a stable seed derived from the profile path keeps identity per-profile.
- **camoufox** (via foxbridge) — a Firefox stealth fork for sites that fingerprint Chromium, driven through the [foxbridge](https://foxbridge.vulpineos.com/) CDP→Juggler proxy. Same CDP subset as Lightpanda (no Chromium screenshot), but optional real-display takeover works because the human drives the real X display.
- **lightpanda** — an ultralight engine covering a rendering subset: HTML / text / network metadata / console / limited observation only — no screenshot, no display takeover (no rendering).
- **edge** — Microsoft Edge, when a target expects that specific engine; the full Chromium capability matrix.
- **brave** — Brave, with built-in ad/tracker blocking and the full capability matrix; also the browser a human drives during takeover.
- **Any other CDP-compatible browser** — launch it yourself; drivers connect via `--endpoint-url`.

Unsupported per-artifact operations return per-artifact warnings (`backend_unsupported`), not whole-fetch failures.

## Human takeover: a person drives the same browser when a step needs it

When a fetch hits a login, captcha, or 2FA wall, `afhttp fetch <url> --takeover` keeps a persistent tab open on a takeover-ready host and hands back a complete short-lived `takeover_url` a human opens to drive the *same* browser the agent is using, via real-display takeover backed by KasmVNC. Once the human is past the wall, the agent re-fetches the same tab to continue. Without `--profile`, takeover switches to a persistent profile derived from the URL's registrable domain. The worked example below shows the full `next_action` payload.

## Worked examples

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
  "trace": {
    "render_decision": "http_only",
    "render_mode": "auto",
    "render_used": false,
    "current_stage": "complete",
    "duration_ms": 120,
    "timeout_ms": 30000,
    "stages": [
      {"name": "navigate", "status": "ok", "duration_ms": 110},
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

No `click` / `type` / `navigate` wrappers. The agent talks raw CDP; afhttp only forwards.

### Check health and capabilities

Before assigning work to a host, an agent or supervisor can ask what is alive and what the backend supports:

```bash
afhttp health --endpoint-url ws://host.mesh.internal:9222
# {"kind":"result","result":{"code":"health","status":"ok","backend":{"family":"chromium","connected":true},...},"trace":{}}

afhttp capabilities --endpoint-url ws://host.mesh.internal:9222
# {"kind":"result","result":{"code":"capabilities","artifacts":{"observation":{"supported":true},...},...},"trace":{}}
```

`/health` is for readiness. `/capabilities` is for planning artifact requests and avoiding predictable `backend_unsupported` warnings.

### Human takes over (real-display takeover)

With the default local `afhttp container install` host running, `fetch --takeover` discovers its endpoint and token automatically:

```bash
afhttp fetch "$URL" --takeover
```

If the warmed profile already reaches the target, `fetch --takeover` just returns the content. Otherwise it keeps a persistent tab open and returns a `next_action`:

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

A human opens the `takeover_url` in a local browser and drives the real display (Brave on KasmVNC). Once they are past the wall, the agent runs the `recommended_command` to re-fetch the same tab and continue. The agent can stay CDP-attached the whole time. `afhttp panel --endpoint-url …` prints the same display URL directly. See [docs/architecture.md §9](docs/architecture.md) for the risk-control honest assessment.

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

## Cross-spore collaboration

afhttp does not operate in isolation. Here is how it fits with the rest of the agentfirstkit suite:

### afmail: CAPTCHAs and mail-borne login flows

When a page requires an emailed verification link or OTP, hand off to **afmail** rather than polling IMAP yourself:

1. `afhttp fetch` navigates to the login form and submits credentials.
2. The page sends an email. The agent calls `afmail triage` (or `afmail fetch`) to find the message, extract the link or code.
3. The agent feeds the link/code back to afhttp via `--evaluate-after-wait` or a subsequent `afhttp fetch`.

afhttp handles the browser-side state; afmail handles the mailbox-side state. They share no storage and are always driven by the agent — never by each other.

### afpay: profile reuse for payment-gated pages

afhttp holds the browser session (cookies, localStorage) that proves the agent is a logged-in subscriber. **afpay** handles the wallet and transaction side. The coordination point is the persistent profile:

- Run `afhttp host --profile <name>` before any payment-gated fetch.
- After afpay completes a purchase, `afhttp fetch --endpoint-url <host>` uses the same host identity and inherits the session cookies set by the checkout flow.
- Never share `--cookie-jar` paths across profiles — the isolation invariant requires the jar to live inside the active profile directory.

### afdata: field naming alignment

afhttp response fields follow **afdata** suffix conventions (suffix-typed names: `_file`, `_ms`, `_url`). When an agent passes afhttp artifacts to afdata for extraction, the field shapes should be predictable without a schema lookup. If you add new fields to fetch responses, match the suffix table in the afdata SDK docs.

## Adopt it: hand afhttp to your agent

Supported platforms: macOS, Linux, Windows.

The quickest way to find out whether afhttp earns a place in your toolkit is to
let your agent read it and tell you. Paste this to your agent:

> Read what Agent-First HTTP is at https://agentfirstkit.com/agent-first-http,
> then tell me in plain terms what it would do for me and whether it fits what
> I'm working on. If it's a fit, install it — the prebuilt package for the quick
> path, or build from source after a quick security review of the repo if you'd
> rather read what you run — then run `afhttp skill install` so you follow its
> behavior rules.

If it's a fit, install it — a prebuilt package, or from source if you want to
read it first:

```bash
# prebuilt binary
brew install agentfirstkit/tap/afhttp   # macOS / Linux
scoop bucket add agentfirstkit https://github.com/agentfirstkit/scoop-bucket && scoop install afhttp   # Windows
cargo install agent-first-http          # any platform, from crates.io

# or build from source after reviewing the repo
git clone https://github.com/agentfirstkit/agent-first-http
cargo install --path agent-first-http
```

Then install the embedded [Agent Skill](skills/agent-first-http/SKILL.md) so the agent
follows afhttp's behavior rules — when to escalate `--render`, when to reach for
human takeover, how to read the artifacts. `skill install` targets Codex, Claude
Code, opencode, and Hermes; `skill status` reports whether each install is present,
valid, and current:

```bash
afhttp skill install
afhttp skill status
```

To remove it, run `afhttp skill uninstall`.

## Docs

- [Architecture](docs/architecture.md) — the canonical contract: roles, CLI surface, profile model, artifacts, health/capabilities endpoints, human takeover, backends, error codes, SDK
- [Deployment](docs/deployment.md) — running the host in a container: backends, security, human takeover
- [Design Principles](docs/design.md) — codebase-wide conventions
- [CLI Reference](docs/cli.md) — flag-by-flag reference for the `afhttp` binary
- [Protocol Reference](docs/reference.md) — output schemas for fetch, cdp, health, capabilities, and profile results
- [Testing](docs/testing.md) — test strategy and gates

## License

MIT
