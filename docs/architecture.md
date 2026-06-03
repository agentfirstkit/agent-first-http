# Architecture

This document is the canonical contract for `afhttp`. The philosophy-only content of `design.md` (acquisition facts, structured errors, artifact field conventions) still holds and is referenced where it applies.

## 1. Motivation

Agents fail before they can reason about a page when URL acquisition is opaque. Plain HTTP returns enough that a human at a terminal can guess the next step, but not enough that a program can branch deterministically. The interesting failure surface is concentrated where plain HTTP cannot produce usable artifacts:

- JavaScript-rendered DOM (HTTP returns an empty shell)
- Cookie- or session-bound pages (HTTP returns a redirect to login)
- Bot-walled pages (HTTP returns a challenge interstitial)
- XHR-injected content (HTTP returns the page chrome but not the data)
- Network-, TLS-, or DNS-level failure (the agent must know which kind to retry differently)

`afhttp` exists to make this whole surface deterministic for agents: the tool returns facts and artifacts that let the agent decide what to do next. It is not a browser automation framework.

The contract is deliberately observation-first. A successful fetch should leave the agent with enough raw evidence to choose the next branch: what the server returned, what the browser rendered, what the page said to the console, what network requests actually carried the data, what interactive elements exist, and what this host is capable of doing.

## 2. Boundary

The tool may:

- fetch HTTP responses
- run a browser when render is requested
- preserve browser state behind a persistent profile
- expose a low-level CDP escape hatch
- expose health and capabilities endpoints for host discovery
- manage local profile lifecycle before and after host runs
- return raw artifacts: bodies, rendered HTML, visible text, screenshots, network logs, console logs, agent-readable observation snapshots, CDP method results

The tool must not:

- infer page intent
- decide that a page is a login page, a paywall, or a captcha
- choose selectors
- rank elements by likely usefulness
- fill forms or click buttons on its own
- parse HTML into "meaning"
- run Readability / markdown extraction as a core behavior
- invent a Playwright-like click/type/navigate action API

The agent decides what a page means and what to do next. Mechanical, well-defined transformations (`innerText`, accessibility tree projection, DOM bounding boxes, gzip decompression, base64 encoding, request/response body capture) are not "thinking" and are allowed as artifacts. Heuristic transformations are not.

## 3. Roles

The networked runtime has two roles. Every command that talks to a running browser-host is one of these roles.

| Role | Responsibility |
| --- | --- |
| **browser-host** | Runs Chromium (or compatible). Holds an on-disk profile. Exposes a CDP endpoint. Optionally embeds the ops panel. |
| **agent-driver** | A CDP client. Issues commands to a browser-host endpoint. Writes artifacts to its own local disk. Comes in two flavors: programmatic (`afhttp fetch`, `afhttp cdp`, or the SDK from Rust code) and interactive (ops panel served by the host, opened in any browser). |

There is no third "operator UI" role. Interactive operation is a CDP client served by the browser-host, opened by a human in a normal browser. The host does not know who its CDP clients are; that is not its concern.

`afhttp profile ...` commands are local administration helpers. They inspect or modify profile directories on the machine where they run and do not join the endpoint protocol.

## 4. Topology

Connectivity is not `afhttp`'s problem. The tool assumes any two `afhttp` instances that need to talk can reach each other over the network. Mesh, VPN, SSH tunnels, or direct LAN — that is the user's infrastructure.

Process lifecycle is also not `afhttp`'s problem. `afhttp host` is a long-running foreground process. The user starts it however suits them and stops it with a normal signal; `afhttp` does not fork, does not write pidfiles, and does not maintain a registry of running hosts. That said, because the host runs a real browser and holds live sessions, the **recommended environment for the host is a container** — the container is the isolation boundary, where the image disables Chromium's own sandbox via `AFHTTP_NO_SANDBOX` (afhttp keeps the sandbox on when run natively). This spore ships one at `container/docker/`. See [deployment.md](deployment.md). The driver commands stay a thin client and run anywhere.

These two non-concerns mean `afhttp` only ever sees endpoint URLs. The CLI and SDK take an endpoint and speak CDP. Whether the endpoint is `unix:/run/afhttp/work.sock`, `ws://localhost:9222`, or `ws://browser.mesh.internal:9222` does not affect the protocol layer.

## 5. CLI Surface

The CLI has 9 commands and no stdin protocol. The command set is: `host`, `fetch`, `upload`, `cdp`, `ui`, `health`, `capabilities`, `profile`, and `tabs`.

| Command | Role | Purpose |
| --- | --- | --- |
| `afhttp host` | browser-host | Start a foreground browser host, profile, CDP/HTTP listener, optional ops panel, and optional display takeover. |
| `afhttp fetch` | agent-driver | Acquire a URL through HTTP-only or browser-backed fetch and write requested artifacts. |
| `afhttp upload` | agent-driver | Attach a local file to an existing `<input type=file>` through `DOM.setFileInputFiles`. |
| `afhttp cdp` | agent-driver | Send one raw CDP method to a target tab. |
| `afhttp ui` | agent-driver/human | Print the ops-panel and display-takeover URLs for a host. |
| `afhttp health` | agent-driver | Query `/health` readiness. |
| `afhttp capabilities` | agent-driver | Query `/capabilities` planning metadata. |
| `afhttp profile` | local admin | List, inspect, lock-check, list captured downloads, delete, prune, or inspect redacted cookies for local profiles. |
| `afhttp tabs` | agent-driver | List or close existing CDP targets. |

### Common conventions

These hold across every command (the per-flag reference is generated into
[cli.md](cli.md) from `afhttp --help-markdown`):

- Flags are long-form only; flag names map to JSON fields with hyphens for
  underscores (`--browser-bin` ↔ `browser_bin`). Booleans that default to false
  are bare (`--tls-insecure`); booleans that default to true take a value
  (`--health on|off`).
- `--endpoint-url` accepts `ws://`, `wss://`, `http://`, `https://`, or `unix:`.
- `--token-secret` is sent as `Authorization: Bearer <token>` (or the `token` query
  parameter for the panel URLs `afhttp ui` prints).
- Every command prints exactly one JSON object on stdout: success carries `code`;
  failure carries `error_code`, `error`, and `retryable`.

### `afhttp host`

Long-running foreground process. Holds exactly one browser profile identity and exposes one CDP/HTTP listener.

```text
afhttp host
  --listen <tcp:HOST:PORT|unix:/path>
  --profile <name|->
  --display headless|headful
  --takeover none|screencast|kasmvnc
  --display-quality-percent <0-100>
  --browser auto|chromium|chrome|chrome_shell|fingerprint_chromium|edge|brave|lightpanda|camoufox
  --browser-bin <path>
  --token-secret <string>
  --health on|off
  --health-public off|minimal
  --proxy-url <url>
  --engine-env K=V
  --browser-arg FLAG
  --recent-requests-cap <N>
```

Lifecycle: when `--takeover kasmvnc` is set, starts KasmVNC `Xvnc` first, waits for the X display and localhost web client, launches the browser headful on that display, then opens the listener and serves CDP plus host HTTP routes (`/ops`, `/ops/display`, `/health`, `/capabilities`). Without takeover, starts the browser directly. On exit, terminates browser/display subprocesses, removes the profile dir if ephemeral, releases the listener.

### `afhttp fetch`

One-shot URL acquisition. With `--endpoint-url`, it drives an existing host. Without `--endpoint-url`, `--render none` uses a lightweight reqwest HTTP client and never starts a browser; `--render auto` tries that HTTP path first and lazily starts an inline ephemeral host only when escalation is needed; `--render always` starts the inline host immediately.

```text
afhttp fetch <url>
  --endpoint-url <url>
  --token-secret <string>
  --render none|auto|always
  --tab new|<id>
  --wait load|idle|selector:<css>|selector-visible:<css>|ms:<n>
  --method GET|POST|PUT|PATCH|DELETE|...
  --data <string|@file>
  --data-file <path>
  --form key=value
  --header K:V
  --cookie 'name=value[; Path=/; Domain=...; Secure; HttpOnly; SameSite=Lax]'
  --user-agent <string>
  --evaluate-after-wait <js>
  --want body,rendered_html,text,screenshot,network,console,observation,storage
  --network-bodies off|xhr|all
  --network-body-max-bytes <n>
  --network-redact on|off
  --capture-ws
  --capture-sse
  --out <dir>
  --cookie-jar <path>
  --no-cookie-jar
  --retry <N>
  --backoff-ms <ms>
  --proxy-url <url>
  --ca-cert <path>
  --tls-insecure
  --timeout <duration>
```

Output: one JSON object on stdout. Includes `status`, `final_url`, `tab_id`, top-level `*_file` artifact paths, `trace` (render decision, escalation reason, phase timings, cookie-jar path/warnings, sensitive-capture flags), and `warnings` (for non-fatal artifact failures). `--network-redact off`, `--capture-ws`, and `--capture-sse` can expose tokens or PII in artifacts and are reflected in `trace.sensitive_capture`.

Custom request options are applied before acquisition, not silently ignored. On the HTTP fast path, headers, user-agent, and applicable cookies are attached to the per-request `reqwest` request. Secure cookies are skipped for `http://` URLs rather than failing the fetch, and host-only cookies match only the exact origin host. On the browser path, ordinary headers use `Network.setExtraHTTPHeaders`, user-agent uses `Network.setUserAgentOverride`, and cookies are installed through CDP before `Page.navigate`.

`--evaluate-after-wait` executes JavaScript after the configured wait condition and before artifact capture. It only works when the final path is browser-backed; it does not by itself trigger `--render auto` escalation.

### Other endpoint commands

`afhttp cdp`, `afhttp upload`, and `afhttp tabs` are raw CDP/target-management helpers. They require `--endpoint-url` and do not introduce Playwright-style semantic action wrappers. `afhttp ui`, `afhttp health`, and `afhttp capabilities` are thin HTTP helpers over `/ops`, `/health`, and `/capabilities`; tokens in generated URLs are percent-encoded.

### Local commands

Downloads are browser session artifacts, not a standalone command: host-side `Browser.setDownloadBehavior` captures them inside the active profile's `downloads/` directory, `fetch` reports `download_file` when navigation becomes a download, and `afhttp profile downloads <name>` lists the captured files read-only for interaction-triggered downloads. `afhttp profile ...` operates only on local profile directories and never deletes or mutates profiles over a remote endpoint.

## 6. Host Health and Capabilities Endpoints

`afhttp host` serves JSON host metadata on the same listener as CDP and the ops panel.

| Route | Auth | Purpose |
| --- | --- | --- |
| `GET /health` | Token required unless `--health-public minimal` is set | Liveness/readiness for agents and supervisors. |
| `GET /capabilities` | Token required | Detailed backend and artifact support for planning fetch requests. |

When `--token-secret` is configured, authenticated requests use `Authorization: Bearer <token>` or the same `token` query parameter accepted by `afhttp ui`. Unauthenticated public health is intentionally minimal: it may return only `{ "status": "ok" }` / `{ "status": "starting" }` / `{ "status": "degraded" }` and never exposes profile names, browser versions, paths, tabs, or network policy.

`/health` response shape:

```json
{
  "code": "health",
  "status": "ok",
  "version": "0.5.0",
  "uptime_s": 42,
  "backend": {"family": "chromium", "version": "124.0.0.0", "connected": true},
  "profile": {"kind": "persistent", "name": "work", "locked": true},
  "tabs_active": 3,
  "capabilities_url": "/capabilities"
}
```

`/capabilities` response shape:

```json
{
  "code": "capabilities",
  "backend": {"family": "chromium", "version": "124.0.0.0"},
  "artifacts": {
    "body": {"supported": true},
    "rendered_html": {"supported": true},
    "text": {"supported": true},
    "screenshot": {"supported": true},
    "network": {"supported": true, "body_capture": ["off", "xhr", "all"]},
    "console": {"supported": true},
    "observation": {"supported": true, "source": "accessibility+dom"}
  },
  "wait_modes": ["load", "idle", "selector", "ms"],
  "display_takeover": true,
  "ops_panel": {"supported": true, "screencast": true},
  "profile": {"persistent": true, "ephemeral": true},
  "limits": {"network_body_max_bytes_default": 1048576}
}
```

Capabilities are descriptive, not a reservation. A later fetch can still return per-artifact warnings if the page crashes, permissions change, or a CDP method fails.

## 7. Profile Model

Profiles are Chromium user-data directories. They are host-local on disk and never copied between hosts. A profile holds cookies, localStorage, sessionStorage, IndexedDB, service worker registrations, and cached browser fingerprint state.

One `afhttp host` binds one profile. Run multiple hosts to use multiple identities in parallel.

- **Persistent**: `afhttp host --profile work` loads `$XDG_DATA_HOME/afhttp/profiles/work/`, creating it on first use. Profile persists across host restarts. The directory is locked while the host is running; a second `afhttp host --profile work` on the same machine will fail with `profile_locked`.
- **Ephemeral**: `afhttp host --profile -` (or `--profile` omitted) uses a tempdir, removed on exit.
- **Inline fetch**: always ephemeral. The `afhttp fetch URL` shorthand path spawns a short-lived host with a tempdir profile, uses it, kills it.

Profile portability is explicitly out of scope. Sessions bound to a specific IP/device fingerprint should remain on a single host; the right way to "move a session" is to put `afhttp host` where the session needs to be and connect to it remotely.

### Isolation invariant

Per [design.md](design.md) "Browsing environments are isolated", every browsing environment afhttp creates is sandboxed:

- **No interaction with system-owned browser data.** afhttp never reads, writes, copies, or imports from the user's real browser profiles (`~/.config/google-chrome/`, `~/Library/Application Support/Firefox/`, the Windows equivalents) or from system keychains. Backend binaries are looked up only as executables; their default `--user-data-dir` is never honored.
- **One host = one independent environment.** Two `afhttp host` instances on the same machine share no cookies, cache, storage, or in-flight tabs even when they target the same backend. The browser subprocess uses an explicit `--user-data-dir <profile-dir>` so engine defaults cannot reach external state.
- **Profiles do not cross-contaminate.** All persistent state for a profile — the browser user-data-dir, the cookie jar (`<profile-dir>/cookies.jar.json`), the lockfile, the PID file, the metadata — lives inside the profile directory. Profile names are validated as flat tokens (no path separators, no `..`) so a malicious name cannot escape the profile root.
- **Proxy, user-data-dir, and environment isolation.** The host never silently honors ambient `HTTP_PROXY` / `HTTPS_PROXY` for browser traffic and always supplies an explicit `--user-data-dir <profile-dir>`. Every browser backend is spawned through `env_clear` plus a minimal allowlist and explicit `--engine-env` entries. Network egress configuration is an explicit per-host or per-fetch opt-in.
- **No remote profile destruction.** `profile delete` and `profile prune` are local-only CLI subcommands; the host's HTTP/CDP surface exposes no "destroy any profile" endpoint, so a stolen bearer token cannot wipe another profile.

What the invariant honestly does **not** cover: the rendering engine itself reads system fonts, the OS timezone, the OS locale, and graphics/device surfaces. The `fingerprint-chromium` and `camoufox` backends address parts of that fingerprint surface; the default `chromium` backend leaks them and §10 says so.

### Profile lifecycle metadata

Persistent profile directories include a small `afhttp-profile.json` metadata file maintained by `afhttp host` and `afhttp profile ...` commands:

```json
{
  "schema_version": 1,
  "name": "work",
  "created_at_rfc3339": "2026-05-27T00:00:00Z",
  "last_used_at_rfc3339": "2026-05-27T01:23:45Z",
  "last_host_version": "0.5.0"
}
```

The metadata is advisory. If an existing Chromium profile directory has no metadata file, `afhttp profile list` still reports it with `metadata_present: false` and infers size/mtime from the filesystem.

Profile lifecycle commands:

| Command | Behavior |
| --- | --- |
| `profile list` | Lists persistent profiles with kind, path, size, metadata status, last used time, and lock status. |
| `profile info <name>` | Reports metadata, profile path, approximate disk usage, active lock owner when known, and browser-family hints. |
| `profile lock-status <name>` | Returns whether the profile is locked and, when possible, the owning pid/start time. |
| `profile downloads <name>` | Read-only listing of files captured under `<profile>/downloads`, with path, byte size, and completion state. |
| `profile delete <name>` | Deletes an unlocked persistent profile after `--confirm <name>`. Refuses ephemeral profiles and locked profiles. |
| `profile prune` | Deletes unlocked persistent profiles older than `--older-than`; `--dry-run` reports the candidate list without deleting. |

`profile delete` and `profile prune` are intentionally local-only. Remote deletion over the CDP/HTTP endpoint would make a stolen token able to destroy browser identities.

## 8. Artifacts

Eight artifact tokens are identified by stable names. Seven are default artifacts; `storage` is opt-in because it can expose sensitive local state.

| Token | Content | Filename | Notes |
| --- | --- | --- | --- |
| `body` | Raw HTTP response body | `body.<ext>` | Always produced when an HTTP response was received. Ext derived from content-type. |
| `rendered_html` | Post-JS DOM serialized to HTML | `rendered.html` | Only when render was used. |
| `text` | `document.body.innerText` | `text.txt` | Only when render was used. Mechanical, not heuristic. |
| `screenshot` | Full-page PNG | `page.png` | Only when render was used. |
| `network` | Deep request/response log from CDP `Network.*` events | `network.json` | Always produced when a browser was used; HTTP-only fetches produce a one-entry log. Optional captured bodies live under `network-bodies/`. |
| `console` | Console events | `console.json` | Only when render was used. |
| `observation` | Agent-readable accessibility/DOM snapshot | `observation.json` | Only when render was used. Mechanical projection of page state; no semantic ranking or intent inference. |
| `storage` | localStorage/sessionStorage/IndexedDB-name snapshot | `storage.json` | Opt-in with `--want storage`; default-off because of sensitive data risk. |

Files are written to `--out <dir>` (default `./afhttp-out/<request-id>/`) on the agent-driver's machine, not the browser-host's. The response JSON references them as absolute paths.

Each artifact can fail independently of the overall fetch. A missing screenshot returns `warnings: [{artifact: "screenshot", code: "backend_unsupported"}]` rather than failing the whole fetch. The agent decides whether the partial result is useful.

### Observation artifact

`observation.json` is the artifact meant for LLM and agent planning loops. It is smaller and more action-oriented than full HTML, but still mechanical data:

```json
{
  "schema_version": 1,
  "url": "https://example.com/dashboard",
  "title": "Dashboard",
  "viewport": {"width": 1280, "height": 720, "device_scale_factor": 1},
  "frames": [{"frame_id": "main", "url": "https://example.com/dashboard"}],
  "nodes": [
    {
      "ref": "obs-17",
      "frame_id": "main",
      "role": "button",
      "name": "Export",
      "text": "Export",
      "visible": true,
      "enabled": true,
      "bbox": {"x": 1032, "y": 88, "width": 91, "height": 36},
      "actions": ["click"]
    }
  ],
  "forms": [],
  "focused_ref": null
}
```

Refs are stable only within one observation snapshot and the current DOM revision. They are not durable selectors. An agent that wants to act still uses raw CDP and may resolve a ref by coordinates, accessibility node id, backend DOM node id, or a best-effort selector hint included in the node when available. Selector hints are scoped to the node's real context: iframe descendants carry the child `frame_id` and frame-relative hints; open-shadow descendants use `host >> shadow >> inner` chains; cross-origin iframes expose only the iframe box plus `frame_ref`.

Allowed observation fields are mechanical: accessibility role/name/state, visible text, bounding box, frame id, href/src/action URLs, form ownership, enabled/checked/selected/focused states, input type, and redacted input value metadata. Disallowed fields: "important", "likely login", "best button", "captcha", "paywall", or any page-intent label.

Observation node collection walks the main document, open shadow roots, and same-origin iframe documents. It starts with native interactive elements (`a[href]`, `button`, form controls, `summary`, `iframe`) plus explicit interaction markers (`role`, `tabindex`, `contenteditable=true`). It also appends non-semantic elements whose computed `cursor` is `pointer`, scanning at most 2,000 elements and emitting at most 100 nodes across the whole tree. If either cap stops traversal, `truncated` records the mechanical reason and limits; truncation is never silent.

### Network artifact depth

`network.json` is a structured capture, not a flat HAR dump. It keeps enough information for an agent to discover whether the useful data came from an XHR/fetch request, GraphQL endpoint, document load, script, iframe, service worker, cache hit, or failed resource.

Each entry includes, when available:

- stable `request_id`, `frame_id`, `loader_id`, parent/redirect linkage, resource type, initiator stack, URL, method, priority, timing, cache/service-worker flags, and failure text
- request headers and response headers with sensitive fields redacted by default (`cookie`, `authorization`, `proxy-authorization`, `set-cookie`, and `*-token`/`*-secret`-like headers)
- request post data metadata (`present`, `size_bytes`, optional `post_data_file` when captured)
- response status, mime type, protocol, remote address, encoded/decoded sizes, and body capture reference when enabled
- mechanical payload hints for JSON and GraphQL bodies (`json_valid`, `graphql_operation_name`, `graphql_operation_type`) without interpreting business meaning

Response body capture is opt-in because network logs often contain credentials, PII, and large binary resources.

| Mode | Behavior |
| --- | --- |
| `--network-bodies off` | Default. Metadata only; no response bodies saved. |
| `--network-bodies xhr` | Saves text/JSON/XHR/fetch response bodies up to `--network-body-max-bytes` each. |
| `--network-bodies all` | Attempts to save every response body up to `--network-body-max-bytes` each, including documents/scripts/images when CDP exposes them. |

Captured bodies are written under `network-bodies/<request_id>.<ext>` and referenced from `network.json` via `body_file`. Binary bodies may be base64 files if the original bytes cannot be represented as UTF-8. Per-entry body capture failures become `warnings` with `artifact: "network"` and do not fail the fetch.

## 9. Ops Panel

The default ops panel is a small static HTML+JS application embedded in the `afhttp` binary and served by `afhttp host` at `/ops`. It exists to let a human drive the browser without VNC, X server, or any system-level remote-desktop stack on the host machine. For hard sites, `--takeover kasmvnc` enables a separate real-display path at `/ops/display`: afhttp spawns an external KasmVNC `Xvnc` process inside the container, runs the browser headful on that X display, and reverse-proxies the KasmVNC web client through the authenticated listener.

**Architecture.** The panel page loaded in the operator's local browser opens two WebSocket flows against the host:

- **Inbound (host → operator)**: `Page.startScreencast` JPEG frames, decoded into a canvas.
- **Outbound (operator → host)**: pointer (`pointermove` / `pointerdown` / `pointerup` / `wheel`) and keyboard (`keydown` / `keyup` / `compositionupdate`) events captured locally with `performance.now()` timestamps. The host replays them via CDP `Input.dispatch*` with the original inter-event timing preserved.

**Risk-control honesty.** Capturing real human pointer/keyboard events and replaying them via CDP gives substantially higher fingerprint fidelity than synthesized events from a coarse UI. Specifically:

- *Solved*: trajectory entropy, sub-millimeter jitter, sampling density, inter-event timing distribution, key dwell time, click hesitation, scroll-rate patterns, behavior signals from real handedness.
- *Partial*: network jitter adds a small additional skew on top of human timing (statistically detectable but indistinguishable from a human on a poor connection); `getCoalescedEvents()` may not return identical micro-event sequences.
- *Not solved*: headless browser fingerprint (`navigator.webdriver`, GPU strings, Canvas/Audio entropy, font fingerprints). These are orthogonal to input fidelity and are mitigated by `--display=headful` plus standard stealth patches, not by the ops panel.

For sites where the residual CDP ops-panel fingerprint is still detected, use `afhttp host --takeover kasmvnc --display headful` inside the container. This improves display/input fidelity and covers camoufox, IME/CJK input, and flaky key sites, but it does not bypass captcha reputation systems by itself: pair it with the right proxy, stealth backend, and a warmed persistent profile. KasmVNC stays an external GPLv2 process located on `PATH`; afhttp does not link or bundle it.

**Multi-attach.** The default ops panel is a CDP client; display takeover is a VNC/X client; the agent (via `afhttp fetch` or `afhttp cdp`) remains a CDP client. CDP supports multiple flattened sessions, so both can be connected to the same browser at the same time. Whichever client sends commands is the one acting. There is no handoff protocol; coordination between agent and human is the agent's concern.

## 10. Backends

`afhttp`'s protocol layer (fetch logic, CDP escape hatch, ops panel) is CDP-generic. The launcher layer (`afhttp host`) knows specific browser families.

| Backend | Launch profile in `host` | Capabilities |
| --- | --- | --- |
| Chromium / Chrome / Edge / Brave | `chromium` (and aliases) | Full: body, rendered_html, text, screenshot, network, console, observation, network body capture, ops panel, display takeover, health/capabilities, multi-attach. |
| chrome-headless-shell | `chromium` (binary `chrome-headless-shell`) | Same capability matrix as Chromium — chrome-headless-shell is Google's slimmer headless distribution of the same engine, identical CDP surface. Use when the full Chrome/Chromium browser is unavailable or too heavy. |
| fingerprint-chromium | `fingerprint-chromium` | Same capability matrix as Chromium, including display takeover. Engine surface (UA, navigator props, WebGL vendor, canvas/font enumeration, CDP-detection evasion) is spoofed per [adryfish/fingerprint-chromium](https://github.com/adryfish/fingerprint-chromium). The host derives a stable 32-bit `--fingerprint=<seed>` from the resolved profile path so identity stays consistent within a profile and diverges between profiles. Per-surface overrides (`--fingerprint-brand`, `--fingerprint-platform`, etc.) reach the engine via `--browser-arg`. |
| Lightpanda | `lightpanda` | body, rendered_html (modulo JS engine limits), text, network metadata, console, limited observation. No screenshot, no screencast, no usable ops panel, no display takeover (no rendering), network body capture depends on backend support. |
| Camoufox (via foxbridge) | `camoufox` | Firefox stealth fork driven by the [foxbridge](https://foxbridge.vulpineos.com/) CDP→Juggler proxy. Same artifact subset as Lightpanda for CDP-only features: no chromium-only screenshot/screencast and no default ops-panel screencast. Display takeover is supported because the human drives the real X display instead of CDP screencast. Body, rendered_html, text, network metadata, console, observation work. Persistent profiles refused with `backend_unsupported` until Firefox profile lifecycle is wired explicitly. The host spawns foxbridge with `--binary <camoufox>` on a pre-reserved port; the SDK sees a chromium-style WebSocket. |
| Any other CDP-compatible browser | none — user launches it themselves | Whatever the backend implements. `afhttp` clients connect via `--endpoint-url`. |

Unsupported per-artifact operations return per-artifact warnings (`backend_unsupported`), not whole-fetch failures.

### Why both fingerprint-chromium and camoufox exist

Both backends add stealth, but they address different threat models and are complementary rather than redundant:

| | fingerprint-chromium | camoufox |
|---|---|---|
| **Engine** | Chromium (Blink) | Firefox (Gecko) |
| **Stealth mechanism** | Patches applied at the Chromium binary level: UA, navigator props, WebGL/Canvas entropy, CDP-detection evasion | Firefox stealth fork with Gecko-native fingerprint randomization; engine-level font/audio/WebGL divergence from stock Chromium |
| **Full artifact support** | Yes — screenshot, screencast, ops panel | No — subset only (no screenshot/screencast) |
| **Target profile** | Sites that block stock headless Chromium or `navigator.webdriver` detection | Sites that actively fingerprint Blink engine characteristics (WebGL vendor strings, V8 timing side-channels) and block Chromium-family browsers regardless of stealth patches |
| **Ops panel** | Supported, plus optional KasmVNC display takeover | Default CDP panel not supported; optional KasmVNC display takeover supported |

Sites that specifically block all Chromium-family browsers (rare but real) require camoufox. Sites that just block unpatched headless work fine with fingerprint-chromium and get the full capability matrix. Both stay so operators can choose the right tool for the threat.

## 11. Error Codes

All errors carry `error_code` (stable enum), `error` (human-readable detail), and `retryable` (bool). Agents match on `error_code` only — the `error` string is for human logs and may change between versions.

Three categories:

- **Transport / navigation** — `navigation_timeout`, `host_unreachable`, `dns_resolution_failed`, `target_unreachable`, `tls_error`, `tab_crashed`, `browser_launch_failed`. Most are retryable; `tls_error` is not.
- **Per-artifact warnings** — `backend_unsupported`, `artifact_capture_failed`, `network_body_truncated`. These populate `warnings[]` on an otherwise-successful response; the fetch itself does not fail.
- **Configuration / profile** — `invalid_argument`, `invalid_endpoint`, `render_unavailable`, `profile_*`, `io_error`. Not retryable without fixing the configuration.

The full enum with example `error` strings and per-code agent guidance lives in [reference.md §Error Codes](reference.md#error-codes).

## 12. Multi-Client Attach

CDP allows multiple flattened sessions per target. The agent and the ops panel are independent clients. The browser is shared state.

Coordination is the agent's concern, not the protocol's. Common pattern: the agent emits an out-of-band signal (e.g. to its own orchestrator) saying "I need help on `<endpoint>`/tab `<id>`". A human runs `afhttp ui --endpoint-url ...`, does their part, closes the panel. The agent's next `afhttp fetch --tab <id>` or `afhttp cdp` continues from the new browser state.

The Rust SDK keeps one lazy CDP WebSocket per `Client` and reuses it across `fetch` / `cdp` calls until `Client::close().await` or drop. This cache is per SDK client, not a browser-wide lease: the ops panel and other SDK clients still attach through their own CDP connections, and all of them can continue to multi-attach to the same target. Each one-shot `cdp --tab` / `fetch --tab` operation detaches its temporary flattened session when the call completes; `--tab` controls target lifetime, not connection ownership.

There is no "lease," "lock," or "active driver" in the protocol. Both clients can issue commands at any time; if they conflict, that is the user's coordination bug to solve.

## 13. Library / SDK

The Rust library exposes the same surface as the CLI, in-process. It is **not** an embedded browser engine; it is an SDK that talks to a `browser-host` over CDP/HTTP. Everything that physically requires a Chromium process — launching, active profile locking, and the ops panel — stays in `afhttp host`. Local profile lifecycle helpers operate on disk and do not pull browser-launch dependencies into SDK-only consumers.

```rust
use afhttp::{Client, RenderMode, Wait, Artifact};
use afhttp::sdk::{FetchCookie, FetchCookieSameSite};

let client = Client::connect("ws://chromium-host:9222")?;

let result = client.fetch("https://example.com")
    .render(RenderMode::Always)
    .wait(Wait::Load)
    .user_agent("agent-script/1")
    .cookie_full(
        FetchCookie::build(("session", "abc"))
            .path("/")
            .http_only(true)
            .same_site(FetchCookieSameSite::Lax)
            .build()
    )
    .evaluate_after_wait("document.body.dataset.agentReady = '1'")
    .timeout(Duration::from_secs(30))
    .want([Artifact::RenderedHtml, Artifact::Observation, Artifact::Screenshot])
    .network_bodies(NetworkBodies::Xhr)
    .send()
    .await?;
// result.rendered_html_file -> path on the caller's local disk
// result.observation_file -> agent-readable page snapshot

let health = client.health().await?;
let capabilities = client.capabilities().await?;

let cdp = client.cdp("Runtime.evaluate")
    .tab(tab_id)
    .params(json!({ "expression": "document.title" }))
    .send()
    .await?;

client.close().await; // optional: closes the cached CDP connection

// Dev / test convenience: spawn a private host in-process, use it, kill it
// on drop. Requires the `host` feature; pure `features = ["sdk"]` consumers
// connect to an externally started afhttp host instead.
let local = Client::inline_ephemeral().await?;
```

**What the SDK exposes**: `Client`, fetch/cdp/health/capabilities builders, the artifact and error enums, request/response/cookie/render-mode/network-capture types, and local profile-store helpers.

**What the SDK does not expose**: chromiumoxide types, host launch internals, ops panel internals, or a remote profile-administration API.

**CLI is the first SDK consumer.** `afhttp fetch` and `afhttp cdp` parse args, call into the SDK, format the response. They are not parallel implementations.

**Cargo features.**

```toml
[features]
default  = ["sdk", "cli"]
sdk      = []                                   # client-side; what library consumers want
host     = ["dep:chromiumoxide", ...]           # browser-launch deps; only in the bin
cli      = ["sdk", "host", "dep:clap",
            "dep:agent-first-data"]             # the afhttp binary
```

External consumers (e.g. the `fetch` service) depend on the crate with `default-features = false, features = ["sdk"]` and link only the SDK weight, not chromiumoxide or any browser-launch code.

## 14. Non-Goals

- Not a browser automation framework. No semantic action API (`click`, `type`, `navigate_with_form`). Use raw CDP.
- Not NAT traversal. Mesh is the user's responsibility.
- Not service discovery. `afhttp` does not register hosts; the user resolves endpoints.
- Not a daemon manager. `afhttp host` is a long-running foreground process; the user manages lifecycle.
- Not heuristic page understanding. No Readability extraction, no "is this a login page" detection, no interstitial / captcha / bot-challenge classification. Challenge pages return the page facts (status, headers, body, screenshot) like any other page; the agent decides what they mean.
- Not semantic observation ranking. `observation.json` reports roles, names, states, and geometry; it does not decide which element matters.
- Not remote profile administration. Profile delete/prune/list operates on local disk only.
- Not an embedded engine in the library. The library is an SDK; engines live in `afhttp host`.
- Not Firefox / WebKit. CDP-only; backends that do not speak CDP are out of scope.
