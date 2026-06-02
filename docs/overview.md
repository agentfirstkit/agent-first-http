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

## What `afhttp` is for

The hard part for an agent is not fetching bytes. It is that many useful URLs do not turn into a usable page from a simple shell request — they require JavaScript rendering, cookies, session state, or a real browser fingerprint. When that happens, a human can open a browser and inspect; an agent needs the same facts as data it can branch on.

`afhttp` covers the whole range:

- **Plain HTTP fetch** when the page works without a browser.
- **Browser-backed fetch** when it does not, producing rendered HTML, an agent-readable observation snapshot, screenshot, and network/console logs as artifacts.
- **Deep network capture** when the visible page is only chrome and the useful data arrives through XHR/fetch/GraphQL calls.
- **Raw CDP escape hatch** when the agent needs to drive the browser directly (DOM inspection, form submission, custom waits) without going through any "click/type" abstraction layer.
- **Ops panel** when a human needs to step in (manual login, captcha, 2FA) on the same browser the agent is using — the default panel needs no remote-desktop stack, and optional KasmVNC display takeover is available for hard sites.
- **Host health/capabilities and local profile tools** so agents can discover backend support and operators can list, inspect, retrieve captured downloads, prune, or delete persistent profiles.

The agent never has to parse human-readable error messages. Every output is structured JSON. Every failure carries a stable `error_code`. See [architecture.md](architecture.md) for the full contract.

## Two roles

| Role | Command | What it does |
| --- | --- | --- |
| **browser-host** | `afhttp host` | Long-running foreground process. Holds Chromium + a profile. Exposes a CDP endpoint and the ops panel. |
| **agent-driver** | `afhttp fetch`, `afhttp upload`, `afhttp cdp`, `afhttp ui`, `afhttp health`, `afhttp capabilities`, `afhttp profile`, `afhttp tabs`, or the Rust SDK | Short-lived client. Connects to a host's endpoint when needed, does work, writes artifacts locally. |

Hosts and drivers are independently locatable. Run the host where the browser needs to be (residential IP, GUI machine, datacenter); run the driver wherever the agent runs. Connectivity is your mesh's problem, not `afhttp`'s.

The CLI has 9 commands: `host`, `fetch`, `upload`, `cdp`, `ui`, `health`, `capabilities`, `profile`, and `tabs`.

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
  "body_file": "/work/afhttp-out/<id>/body.html",
  "rendered_html_file": "/work/afhttp-out/<id>/rendered.html",
  "network_file": "/work/afhttp-out/<id>/network.json",
  "trace": {"render_decision": "browser", "render_used": true, "duration_ms": 820}
}
```

### Long-running host + remote fetch

For real workflows: start one `afhttp host`, drive it from anywhere.

```bash
# On the host machine (or in a systemd unit, tmux pane, docker container — your choice).
# A non-loopback listener (anything other than 127.0.0.1 / a unix: socket) serves
# full browser control over /cdp, so a --token-secret is required — the host refuses to
# bind otherwise:
export AFHTTP_TOKEN=$(head -c32 /dev/urandom | base64)
afhttp host --listen tcp:0.0.0.0:9222 --profile work --display headless \
            --token-secret "$AFHTTP_TOKEN"

# From the agent's machine:
afhttp fetch --endpoint-url ws://host.mesh.internal:9222 --token-secret "$AFHTTP_TOKEN" \
             --render auto --wait load \
             --want rendered_html,observation,screenshot,network,console \
             --network-bodies xhr \
             https://target.example.com/dashboard
```

The token gates `/cdp`, the ops panel, and `/profile`; bind `tcp:127.0.0.1:<port>` or a `unix:` socket instead when the host and driver share a machine and you want to skip it. The profile persists across host restarts. Cookies and localStorage acquired in one fetch are available to the next.

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

### Human takes over (ops panel)

When the agent hits a login wall or captcha:

```bash
afhttp ui --endpoint-url ws://host.mesh.internal:9222
# {"code":"ui","panel_url":"http://host.mesh.internal:9222/ops","display_url":"http://host.mesh.internal:9222/ops/display"}
# open that URL in your local browser
```

The default ops panel shows the remote browser's live screen via CDP screencast and replays local pointer/keyboard events over CDP. For captchas, IME/CJK input, camoufox, or sites where CDP-synthesized input is flaky, start the host with `--takeover kasmvnc --display headful` and open the `display_url`; this proxies an in-container KasmVNC web client so the human drives the same browser through a real X display. The agent can stay attached the whole time. See [architecture.md §9](architecture.md) for the risk-control honest assessment.

### Manage persistent profiles

Persistent browser profiles are local disk identities. Operators can inspect and clean them up without guessing which temp directory belongs to which host:

```bash
afhttp profile list
afhttp profile info work
afhttp profile lock-status work
afhttp profile downloads work
afhttp profile prune --older-than 30d --dry-run
afhttp profile delete old-work --confirm old-work
```

Profile lifecycle commands are local-only; `downloads` only lists captured files, and destructive commands refuse locked profiles.

## From Rust

The library is a thin SDK over the same endpoint protocol. It is **not** an embedded browser engine; it talks to a running `afhttp host` over CDP.

```rust
use afhttp::{Client, RenderMode, Wait, Artifact};

let client = Client::connect("ws://host.mesh.internal:9222")?;

let result = client.fetch("https://target.example.com")
    .render(RenderMode::Auto)
    .wait(Wait::Load)
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
| Chromium / Chrome / Edge / Brave | Full support: all artifacts, observation, network body capture, ops panel, optional KasmVNC display takeover, multi-attach. |
| chrome-headless-shell | Same as Chromium — Google's slimmer headless distribution, identical CDP surface. Useful when the full browser is unavailable. |
| fingerprint-chromium | Same capability matrix as Chromium, including optional display takeover, with engine-level fingerprint spoofing (UA, WebGL, canvas, CDP-detection evasion). The host derives a stable seed from the profile path so identity stays per-profile. |
| Lightpanda | HTML / text / network metadata / console / limited observation only — no screenshot, no screencast, no display takeover (no rendering). |
| Camoufox (via foxbridge) | Firefox stealth fork driven through the [foxbridge](https://foxbridge.vulpineos.com/) CDP→Juggler proxy. Same CDP subset as Lightpanda — no chromium screenshot/screencast — but optional display takeover works because the human drives the real X display. |
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

- [Architecture](architecture.md) — the canonical contract: roles, CLI surface, profile model, artifacts, health/capabilities endpoints, ops panel, backends, error codes, SDK.
- [Design Principles](design.md) — codebase-wide conventions (field naming, structured errors, output formats, no-panic policy).
- [CLI Reference](cli.md) — flag-by-flag reference for the `afhttp` binary.
- [Protocol Reference](reference.md) — output schemas for fetch, cdp, health, capabilities, and profile results.
- [Testing](testing.md) — test strategy and coverage gates.

## License

MIT
