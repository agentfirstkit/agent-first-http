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

- infer open-ended page intent or extract business meaning
- decide that a page is a login page or a paywall; conservative security-challenge
  markers may only set `page_kind`/`next_action` so the agent knows the target
  content is unverified
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
| **browser-host** | Runs Chromium (or compatible). Holds an on-disk profile. Exposes a CDP endpoint. Optionally serves real-display takeover. |
| **agent-driver** | A CDP client. Issues commands to a browser-host endpoint. Writes artifacts to its own local disk. Comes in two flavors: programmatic (`afhttp fetch`, `afhttp cdp`, or the SDK from Rust code) and interactive (real-display takeover served by the host, opened in any browser). |

There is no third "operator UI" role. Interactive operation is a CDP client served by the browser-host, opened by a human in a normal browser. The host does not know who its CDP clients are; that is not its concern.

`afhttp profile ...` commands are local administration helpers. They inspect or modify profile directories on the machine where they run and do not join the endpoint protocol.

## 4. Topology

Connectivity is not `afhttp`'s problem. The tool assumes any two `afhttp` instances that need to talk can reach each other over the network. Mesh, VPN, SSH tunnels, or direct LAN — that is the user's infrastructure.

Process lifecycle is also not `afhttp`'s problem. `afhttp host` is a long-running foreground process. The user starts it however suits them and stops it with a normal signal; `afhttp` does not fork, does not write pidfiles, and does not maintain a registry of running hosts. That said, because the host runs a real browser and holds live sessions, the **recommended environment for the host is a container** — the container is the isolation boundary, where the image disables Chromium's own sandbox via `AFHTTP_NO_SANDBOX` (afhttp keeps the sandbox on when run natively). This spore ships one at `container/docker/`. See [deployment.md](deployment.md). The driver commands stay a thin client and run anywhere.

These two non-concerns mean `afhttp` only ever sees endpoint URLs. The CLI and SDK take an endpoint and speak CDP. Whether the endpoint is `unix:/run/afhttp/work.sock`, `ws://localhost:9222`, or `ws://browser.mesh.internal:9222` does not affect the protocol layer.

## 5. CLI Surface

The CLI has 12 commands and no stdin protocol. The command set is: `host`,
`fetch`, `upload`, `cdp`, `panel`, `ui`, `health`, `capabilities`, `profile`,
`tabs`, `skill`, and `container`.

| Command | Role | Purpose |
| --- | --- | --- |
| `afhttp host` | browser-host | Start a foreground browser host, profile, CDP/HTTP listener, and optional real-display takeover. |
| `afhttp fetch` | agent-driver | Acquire a URL through HTTP-only or browser-backed fetch and write requested artifacts. |
| `afhttp upload` | agent-driver | Set an existing `<input type=file>` from a path visible to both driver and browser through `DOM.setFileInputFiles`; no bytes are transferred. |
| `afhttp cdp` | agent-driver | Send one raw CDP method to a target tab. |
| `afhttp panel` | agent-driver/human | Print a short-lived takeover URL for a host. |
| `afhttp ui` | agent-driver/human | Open a browser takeover interface through an AFUI delivery. |
| `afhttp health` | agent-driver | Query `/health` readiness. |
| `afhttp capabilities` | agent-driver | Query `/capabilities` planning metadata. |
| `afhttp profile` | local admin | List, inspect, lock-check, list captured downloads, delete, prune, or inspect redacted cookies for local profiles. |
| `afhttp tabs` | agent-driver | List or close existing CDP targets. |
| `afhttp skill` | local admin | Install, remove, or check the embedded Agent Skill for supported coding agents. |
| `afhttp container` | local admin/operator | Build, run, inspect, log, or remove the managed local host container. |

### Common conventions

These hold across every command (the per-flag reference is generated into
[cli.md](cli.md) from `afhttp --docs`, which renders the CLI registry itself):

- Flags are long-form only; flag names map to JSON fields with hyphens for
  underscores (`--browser-bin` ↔ `browser_bin`). Boolean flags are bare presence
  toggles, never `on|off` values: a default-on behavior is disabled with `--no-x`
  (`--no-health`, `--no-network-redact`, `--no-cookie-jar`); a default-off
  behavior is enabled with bare `--x` (`--tls-insecure`).
- `--endpoint-url` accepts `ws://`, `wss://`, `http://`, `https://`, or `unix:`.
  Every driver command (`fetch`/`cdp`/`tabs`/`upload`/`health`/`capabilities`/`panel`)
  falls back to `AFHTTP_ENDPOINT_URL` when the flag is omitted, and then to the
  standard local `afhttp-host` container: it is inspected read-only for its
  token and verified over `/health` before anything is sent. That verification
  covers what discovery is entitled to decide — the host is running, its token
  is readable, its version matches the driver's, and it has a backend at all —
  and stops there. A `degraded` backend is a reading about how busy the browser
  is, not a verdict on whether the request can be served, so it does not block:
  the request goes out and fails, if it fails, with what actually broke. This is
  also what a caller who names the host with `--endpoint-url` gets, so both ways
  of reaching a host behave alike. Discovery never starts or recreates a
  container, and `fetch` without a profile or a takeover still needs no host at
  all — it runs inline.
- `--token-secret` is sent as `Authorization: Bearer <token>` for HTTP control
  requests, and the flag names a *source* rather than only a value:
  `env:NAME`, `file:PATH#DOT_PATH` (any JSON/TOML/YAML/dotenv/INI config, read
  through AFDATA's size-capped document reader), `container:NAME`, or
  `literal:TOKEN`; a bare value is the token itself. The grammar, and every
  source but `container:`, is AFDATA's `SourceSet` — the registry declares
  which sources this argument accepts, renders that syntax into `--help` and
  `--docs`, and rejects an unaccepted one with the other argv errors at exit 2,
  before anything is opened. `container:NAME` is afhttp's own: the host token
  lives in a named volume no path on this machine addresses.
  Driver commands fall back to `AFHTTP_TOKEN_SECRET`, then to the discovered
  host's own token; an explicit source always wins, including over a discovered
  host. `afhttp host` takes the same sources for the token it will *require*,
  with no fallback. A resolved token is an AFDATA `SecretString`: it reaches the
  `Authorization` header through `expose_secret` and renders as `***`
  everywhere else, including a `{:?}` on whatever holds it. Human takeover URLs
  never embed the long-lived token; they carry a short-lived `handoff_secret`
  capability instead.
- Every command emits AFDATA protocol-v1 JSON events, and results are JSON
  only — `--output` accepts no other format for a command result, though
  `--help`/`--version` also render `plain` and `yaml`. With the default
  `--output-to split`, success goes to stdout and failure goes to stderr;
  `--output-to stdout|stderr` selects a unified ordered event stream. `afhttp
  host` is the one command whose output is a stream rather than a single
  terminal event, so it defaults to `--output-to stdout`.
- The CLI is compiled from a closed `cli-spec-v1` registry. An invocation runs
  only when it matches exactly one registered shape, so mixes that used to fail
  at runtime — `--takeover` with `--render none`, `--data` with `--form`,
  `container logs --follow` without `--raw`, `--takeover-provider kasmvnc` with
  `--display headless` — are now rejected by the parser with
  `cli_unregistered_combination` before any work starts.

### `afhttp host`

Long-running foreground process. Holds exactly one browser profile identity and exposes one CDP/HTTP listener.

```text
afhttp host
  --listen <tcp:HOST:PORT|unix:/path>
  --profile <name|->
  --display headless|headful
  --takeover-provider off|<provider> # provider name (currently only kasmvnc); off = no takeover
  --takeover-quality-percent <0-100>
  --browser auto|chromium|chrome|fingerprint-chromium|edge|brave|lightpanda|camoufox
  --browser-bin <path>
  --token-secret <string>
  --no-health
  --health-public off|minimal
  --proxy-url <url>
  --engine-env K=V
  --browser-arg FLAG
  --recent-requests-cap <N>
  --tab-idle-timeout-s <seconds>
```

Lifecycle: when `--takeover-provider <provider>` (e.g. `--takeover-provider kasmvnc`) is set, starts that display provider first, waits for the X display and localhost web client, launches the browser headful on that display, then opens the listener and serves CDP plus host HTTP routes (`/takeover/panel`, `/health`, `/capabilities`). With `--takeover-provider off` (the default for the raw `host` binary), starts the browser directly and serves no takeover surface. On exit, terminates browser/display subprocesses, removes the profile dir if ephemeral, releases the listener.

Idle-tab reclamation: drivers close the targets they open, but a driver killed mid-fetch or an agent that walks away leaves tabs behind — and the host launches its browser with background throttling disabled (a live fetch needs that), so a leaked tab keeps burning CPU. The host is the only party that sees every target, so it sweeps them: once the host has been idle for `--tab-idle-timeout-s` (default 900, `0` disables) — no `/cdp` proxy client connected, no takeover credential live — leftover `http(s)` page targets are closed and a `tab_reaped` process event is emitted per tab. The connection count covers every driver (all CDP traffic flows through the proxy); credential liveness covers the human on the real display, who drives the browser without a CDP connection. Any connection or credential resets every tab's idle clock, so activity grants the whole browser a fresh timeout. The backend's per-target `attached` flag is deliberately not the signal: a chromium-family host's own launcher keeps every page attached, so it says nothing about drivers. `about:blank` and browser-internal pages are never reaped, and a sweep always leaves at least one page target so a headful browser keeps its window. A driver that finds its long-lived `--tab` gone after sitting idle past the timeout should treat that as the host reclaiming an unowned tab, not a crash.

### `afhttp fetch`

One-shot URL acquisition. With `--endpoint-url`, it drives an existing host. Without `--endpoint-url`, `--render none` uses a lightweight reqwest HTTP client and never starts a browser; `--render auto` tries that HTTP path first and lazily starts an inline ephemeral host only when escalation is needed; `--render always` starts the inline host immediately.

```text
afhttp fetch <url>
  --endpoint-url <url>
  --token-secret <string>
  --render none|auto|always
  --tab new|<id>
  --takeover                 # prepare the tab for human takeover (see below)
  --wait auto|load|idle|selector:<css>|selector-visible:<css>|ms:<n>
  --method GET|POST|PUT|PATCH|DELETE|...
  --data <string|@file>
  --form name=value
  --header NAME:VALUE
  --cookie 'name=value[; Path=/; Domain=...; Secure; HttpOnly; SameSite=Lax]'
  --user-agent <string>
  --evaluate-after-wait <js>
  --want body|rendered_html|text|content|content_json|screenshot|network|console|observation|storage   (repeatable)
  --network-bodies off|xhr|all
  --network-body-max-bytes <n>
  --readiness-idle-ms <n>
  --readiness-stable-ms <n>
  --readiness-min-text-bytes <n>
  --no-network-redact
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
  --timeout-ms <ms>
```

Output: one JSON event on the selected output stream. Successful fetches include `status`, `final_url`, `tab_id`, top-level `*_file` artifact paths, `trace` (render decision, escalation reason, wait/readiness signals, phase timings, cookie-jar path/warnings, sensitive-capture flags), and `warnings` (for non-fatal artifact failures). Fetch execution failures use the standard error envelope plus the same `trace` shape. `--no-network-redact`, `--capture-ws`, and `--capture-sse` can expose tokens or PII in artifacts and are reflected in `trace.sensitive_capture`.

Custom request options are applied before acquisition, not silently ignored. On the HTTP fast path, headers, user-agent, and applicable cookies are attached to the per-request `reqwest` request. Secure cookies are skipped for `http://` URLs rather than failing the fetch, and host-only cookies match only the exact origin host. On the browser path, ordinary headers use `Network.setExtraHTTPHeaders`, user-agent uses `Network.setUserAgentOverride`, and cookies are installed through CDP before `Page.navigate`.

`--evaluate-after-wait` executes JavaScript after the configured wait condition and before artifact capture. It only works when the final path is browser-backed; it does not by itself trigger `--render auto` escalation.

`--takeover` is the human-takeover entry point. It requires a running takeover-ready host and a browser render (`--render auto` or `always`, never `none`). When `--endpoint-url` / `AFHTTP_ENDPOINT_URL` is omitted, the driver auto-discovers the standard local `afhttp-host` container and reads its token; it does not auto-create containers. It opens or reuses a persistent tab (`--tab`) and navigates it. If `--profile` is omitted, it switches the host to the URL's registrable-domain profile (e.g. `contabo.com`) before opening the tab. If the warmed profile already reaches the target, it returns the captured content with no `next_action` — and closes the tab it opened, because no human is coming to it. Only a detected wall holds the tab: the fetch then returns `next_action` with `kind: "human_takeover"`, a complete short-lived `takeover_url_secret` (the real-display capability URL a human opens to clear a login/captcha/2FA wall), its expiry metadata, and a `recommended_command` that re-fetches the same `--tab` once the human is past the wall. When that re-fetch comes back clean, it closes the tab, completing the loop; if it still hits the wall, the tab stays for another round. A takeover tab therefore lives exactly as long as a human is expected at it, and `tab_id` on a fetch result always names a tab that is still open.

### Other endpoint commands

`afhttp cdp`, `afhttp upload`, and `afhttp tabs` are raw CDP/target-management helpers. They need a running host; when `--endpoint-url` is omitted, the driver discovers the standard local container. They do not introduce Playwright-style semantic action wrappers. `afhttp panel`, `afhttp health`, and `afhttp capabilities` are thin HTTP helpers over `/takeover/handoff`, `/health`, and `/capabilities`; panel output contains a short-lived `takeover_url_secret` with `handoff_secret=...`, not the host token.

`upload --file` validates and canonicalizes the file in the driver's filesystem,
then sends that absolute path to `DOM.setFileInputFiles`. The browser must be
able to read the same file at the same path; file bytes are not transferred.
The standard container mounts only its named volume at `/data` and provides no
host-directory sharing option. A file present only on the driver's machine
therefore cannot be uploaded through the default container, even if local
validation succeeds. Files under the container's `/data` are browser-side
files, not automatically accessible to the driver.

### Local commands

Downloads are browser session artifacts, not a standalone command: host-side `Browser.setDownloadBehavior` captures them inside the active profile's `downloads/` directory, `fetch` reports `download_file` when navigation becomes a download, and `afhttp profile downloads <name>` lists the captured files read-only for interaction-triggered downloads. `afhttp profile ...` operates only on local profile directories and never deletes or mutates profiles over a remote endpoint.

## 6. Host Health and Capabilities Endpoints

`afhttp host` serves JSON host metadata on the same listener as CDP and display takeover.

| Route | Auth | Purpose |
| --- | --- | --- |
| `GET /health` | Token required unless `--health-public minimal` is set | Liveness/readiness for agents and supervisors. |
| `GET /capabilities` | Token required | Detailed backend and artifact support for planning fetch requests. |

When `--token-secret` is configured, authenticated control requests use
`Authorization: Bearer <token>`; legacy `token_secret` query auth remains only
for non-takeover control endpoints and CDP WebSocket setup. `/takeover/handoff`
mints a short-lived URL capability with default TTL 900 seconds (allowed range
60-3600 seconds), scoped only to `/takeover/*`; host restart invalidates all
handoffs. Unauthenticated public health is intentionally minimal:
it may return only `{ "status": "ok" }` / `{ "status": "starting" }` /
`{ "status": "degraded" }` and never exposes profile names, browser versions,
paths, tabs, or network policy.

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
  "wait_modes": ["auto", "load", "idle", "selector", "ms"],
  "takeover": {
    "backend_capable": true,
    "supported": true,
    "panel_url": "/takeover/panel",
    "provider": "kasmvnc"
  },
  "profile": {"persistent": true, "ephemeral": true},
  "limits": {"network_body_max_bytes_default": 10485760}
}
```

Capabilities are descriptive, not a reservation. A later fetch can still return per-artifact warnings if the page crashes, permissions change, or a CDP method fails.

## 7. Profile Model

Profiles are Chromium user-data directories. They are host-local on disk and never copied between hosts. A profile holds cookies, localStorage, sessionStorage, IndexedDB, service worker registrations, and cached browser fingerprint state.

One `afhttp host` serves one *active* profile at a time, but it switches at runtime: a client that passes `?profile=<name>` on the `/cdp` connection (or `afhttp fetch --profile <name>`) makes the host relaunch its browser under that profile, handing off the on-disk profile lock. `fetch --takeover` derives the profile from the URL's registrable domain (eTLD+1) by default, so each site gets an isolated identity. Switching is sequential — an in-flight fetch keeps its own browser alive until it finishes, but the host has one foreground browser, so for genuinely parallel multi-domain work run multiple hosts.

- **Persistent**: `afhttp host --profile work --browser brave` loads `$XDG_DATA_HOME/afhttp/profiles/brave/work/`, creating it on first use. The same logical name under another backend, such as `--browser chromium`, uses `$XDG_DATA_HOME/afhttp/profiles/chromium/work/`. Profile persists across host restarts. The backend-scoped directory is locked while the host is running; a second host on the same backend/profile will fail with `profile_locked`.
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
  "schema_version": 2,
  "name": "work",
  "backend": "brave",
  "created_at_rfc3339": "2026-05-27T00:00:00Z",
  "last_used_at_rfc3339": "2026-05-27T01:23:45Z",
  "last_host_version": "0.6.0"
}
```

The metadata is advisory, but schema/backend mismatches are rejected because the backend directory is part of the isolation boundary. Old unscoped `$XDG_DATA_HOME/afhttp/profiles/<name>` directories are ignored as deprecated data; there is no implicit migration.

Profile lifecycle commands:

| Command | Behavior |
| --- | --- |
| `profile list` | Lists persistent profiles with backend, name, path, size, metadata status, last used time, and lock status. |
| `profile info <name> [--backend <backend>]` | Reports metadata, profile path, approximate disk usage, active lock owner when known, and browser-family hints. `--backend` is required when multiple backends have the same logical name. |
| `profile lock-status <name> [--backend <backend>]` | Returns whether the backend-scoped profile is locked and, when possible, the owning pid/start time. |
| `profile downloads <name> [--backend <backend>]` | Read-only listing of files captured under `<profile>/downloads`, with path, byte size, and completion state. |
| `profile delete <name> [--backend <backend>]` | Deletes an unlocked persistent profile after `--confirm <name>`. Refuses ephemeral profiles and locked profiles. |
| `profile prune` | Deletes unlocked persistent profiles older than `--older-than`; `--dry-run` reports the candidate list without deleting. |

`profile delete` and `profile prune` are intentionally local-only. Remote deletion over the CDP/HTTP endpoint would make a stolen token able to destroy browser identities.

## 8. Artifacts

Ten artifact tokens are identified by stable names. With no explicit `--want`,
the HTTP fast path captures `body`; when a browser render is actually used,
afhttp expands to the browser default set (`body`, `rendered_html`, `text`,
`content`, `content_json`, `screenshot`, `network`, `console`, `observation`).
`storage` is opt-in because it can expose sensitive local state.

| Token | Content | Filename | Notes |
| --- | --- | --- | --- |
| `body` | Raw HTTP response body | `body.<ext>` | Always produced when an HTTP response was received. Ext derived from content-type. |
| `rendered_html` | Post-JS DOM serialized to HTML | `rendered.html` | Only when render was used. |
| `text` | `document.body.innerText` | `text.txt` | Only when render was used. Mechanical, not heuristic. |
| `content` | Agent-oriented composed page view: visible text from open shadow DOM, same-origin frames, cards, tables, links | `content.md` | Only when render was used. The artifact an agent reads first. |
| `content_json` | Structured form of `content` with link/action candidates | `content.json` | Only when render was used. Use to choose a follow-up link/action. |
| `screenshot` | Full-page PNG | `page.png` | Only when render was used. |
| `network` | Deep request/response log from CDP `Network.*` events | `network.json` | Included in the browser default set and produced when requested. Optional captured bodies live under `network-bodies/`. |
| `console` | Console events | `console.json` | Only when render was used. |
| `observation` | Agent-readable accessibility/DOM snapshot | `observation.json` | Only when render was used. Mechanical projection of page state; no semantic ranking or intent inference. |
| `storage` | localStorage/sessionStorage/IndexedDB-name snapshot | `storage.json` | Opt-in with `--want storage`; default-off because of sensitive data risk. |

Files are written to `--out <dir>` (default: the system temporary directory under `afhttp-out/<request-id>/`) on the agent-driver's machine, not the browser-host's. The response JSON references them as absolute paths. The default temp artifacts persist for inspection; pass `--out` when you want a project-local or long-lived directory.

Each artifact can fail independently of the overall fetch. A missing screenshot returns `warnings: [{artifact: "screenshot", code: "backend_unsupported"}]` rather than failing the whole fetch. The agent decides whether the partial result is useful.

### Observation artifact

`observation.json` is the artifact meant for LLM and agent planning loops. It is smaller and more action-oriented than full HTML, but still mechanical data:

```json
{
  "schema_version": 2,
  "page_url": "https://example.com/dashboard",
  "title": "Dashboard",
  "viewport": {"width": 1280, "height": 720, "device_scale_factor": 1},
  "frames": [{"frame_id": "main", "frame_url": "https://example.com/dashboard"}],
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

- stable `request_id`, `state` (`pending`, `responded`, `finished`, or `failed`), `frame_id`, `loader_id`, parent/redirect linkage, resource type, initiator stack, URL, method, priority, timing, cache/service-worker flags, and failure text
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

Captured bodies are written under `network-bodies/<request_id>.<ext>` and referenced from `network.json` via `body_file`. With `--wait auto`, browser fetches capture XHR/fetch/EventSource bodies by default. CDP base64 bodies are decoded before being written as raw bytes. Per-entry body capture failures become `warnings` with `artifact: "network"` and do not fail the fetch.

`network.summary` also reports readiness diagnostics: `responses_total`,
`finished_total`, `incomplete_total`, `inflight_total_at_capture`, and
`pending_by_resource_type`. These are mechanical counts; they do not classify
which payload is important.

Each fetch retains at most 4,096 network entries and 4,096 console events.
WebSocket/SSE capture retains at most 4,096 request keys per stream kind and
1,024 frames/events per request. Existing network entries keep
receiving lifecycle updates after the entry limit is reached, and a slot is
reserved for the main document. Counts describe retained records rather than
all activity when capture is incomplete.

`network.summary` and `console.json` report `events_dropped` for lost CDP
events and `truncated` when a capture limit is reached. Zero/false values are
omitted and default to zero/false when reading older artifacts. Collectors
continue after event loss and stop when their fetch scope is dropped. Readiness
samples taken after network event loss or a metadata or stream capture limit
fall back to the idle window: every Network.* event still stamps activity even
when its request was not retained, so `network_quiet` means "no network event
for the idle window" rather than "no pending request", and the normal deadline
still bounds the wait. The trace
retains the last readiness sample before artifact capture. Loss or truncation
discovered later appears in warnings and the final summary without rewriting
that sample. Skipping or truncating response bodies does not change readiness.
Network warnings also expose incomplete capture when only stream payloads
were requested.

Before requesting a network response body, afhttp checks known
`encodedDataLength` and case-insensitive `Content-Length`. A known size above
`--network-body-max-bytes` skips the body request and emits
`network_body_truncated`. Unknown sizes still use the existing decoded-prefix
limit. CDP returns a complete JSON response, and a compressed size can be
smaller than its decoded body; this is an early size filter, not a peak-memory
guarantee. The separate main `body` artifact keeps its own capture contract.

## 9. Takeover

Human takeover lets a person drive the *same* browser the agent is using — for a manual login, 2FA, captcha, or any step the agent cannot complete on its own. The single takeover surface is **real-display takeover**: when `afhttp host` is launched with `--takeover-provider <provider>` (currently only `kasmvnc`), afhttp starts that display provider, runs the browser headful on its X display, and reverse-proxies the provider's web client through the authenticated listener at `/takeover/panel`. The container shipped by `afhttp container install` is takeover-ready by default (Brave + KasmVNC + an ephemeral initial profile + 2g `/dev/shm`). The takeover backend is not fixed to one vendor: `brave` and `chrome` both drive the display and both clear the same walls, so `--with chrome -- --browser chrome` is a choice of fingerprint and Shields behaviour rather than of bot-wall capability.

The driver-side entry point is `afhttp fetch <url> --takeover` against such a host (see §5). It opens or reuses a persistent tab, and either returns the content directly (if the warmed profile already reaches the target) or hands back a `takeover_url_secret` and a `recommended_command` re-fetch for the same tab. The human opens the `takeover_url_secret` in a local browser and drives the real display; the agent re-fetches once the wall is cleared.

**Risk-control honesty.** Real-display takeover drives a real X display through KasmVNC, so the human's pointer/keyboard input reaches the browser as native OS input events rather than synthesized CDP events. This is the strongest takeover fidelity afhttp offers (full input realism, IME/CJK input, real handedness/timing), but it does **not** by itself bypass captcha reputation systems or the headless/browser fingerprint surface. When real-display takeover still fails, the likely remaining causes are IP/network reputation, account state, or site policy — not the takeover surface. Takeover uses the host's active profile; by default `fetch --takeover` switches to a persistent per-site profile derived from the URL. KasmVNC stays an external GPLv2 process located on `PATH`; afhttp does not link or bundle it.

**Multi-attach.** Display takeover is a VNC/X client; the agent (via `afhttp fetch` or `afhttp cdp`) remains a CDP client. CDP supports multiple flattened sessions, so the agent stays attached while the human drives the display. Whichever side sends input is the one acting. Nothing negotiates *what* the human did — that stays the agent's concern — but there is a signal for *when they are finished*: see "Knowing when the human is done" below.

**Panel credentials.** The panel is reached with a short-lived credential minted by `POST /takeover/handoff`, not with the host's long-lived token. Each one is an [`agent-first-ui`](https://github.com/agentfirstkit/agent-first-ui) `UiAccessToken`: it is compared in constant time (the same standard the host token is already held to), it carries a fixed deadline measured from issue (default 900s, clamped 60–3600) that a busy panel cannot slide forward, and it is *revocable*. The host revokes every live panel credential when the browser those credentials pointed at goes away — a runtime profile switch, and host shutdown — so a panel someone still has open stops authorizing instead of silently outliving what it showed.

`afhttp ui takeover` exchanges that one-shot handoff for a private upstream lease. The command renews the lease while its AFUI delivery exists, revokes it when the delivery ends, and a short idle bound cleans it up if the command crashes. That lease is never emitted and is not the lifetime of the person's Link or registered Session.

**Knowing when the human is done.** `afhttp panel` and `fetch --takeover` hand back a `takeover_url_secret` and exit; whether anyone opened it, and whether they are finished, is left for the agent to guess. `afhttp ui takeover` is the other half: it holds the session open and returns when it ends. That is a *watch*-shaped session: a VNC canvas has no submit control, so there is nothing to confirm and no typed result to collect. It also accepts `--takeover-url-secret` so it can open a panel an earlier `fetch --takeover` already minted.

There are three deliveries of that one session, because the person is not always at this machine, and the way to them is not always this machine's screen. They are `agent-first-ui`'s three, named with its three words: `--mode window|link|session`. The flag has no default of its own, so an absent one is resolved against `AFUI_DELIVERY` and then a window — which is how a runner that never has a screen says so once, for everything under it, instead of every call site guessing.

- **A window here (`--mode window`).** An isolated, disposable-profile window (`agent-first-ui`'s `UiWindow` — never an afhttp-managed profile, so the isolation invariant holds). Closing it is the ending. It needs a display, so it is for an agent running where a person can see a window.
- **A URL for another device (`--mode link`).** No window opens. AFUI binds the LAN listener and returns its own `link_url`, containing exactly this takeover session behind the standard AFUI shell and proxy. The takeover credential never leaves the process, and the host may stay on loopback. The global AFUI attention policy supplies the idle warning, grace period and expiry; afhttp supplies no second clock.
- **A listing (`--mode session`).** No window and no address advertised; the panel is announced to `agent-first-ui`'s cross-process session registry, so `afui session list` sees it and `afui session open` or `afui session serve` can reach it later. The registered session has no AFUI deadline and ends when the command is stopped or the upstream host itself goes away.

All three are executed by `UiDeliveryPlan::start_upstream`, not by afhttp branches that reinterpret AFUI's mode names. The panel is announced in all three — best effort for an already-working window or Link, strict for registered-only Session. Ending the command withdraws the entry, revokes the internal source lease, and a registry reader prunes the entry if the command dies first.

**Where the panel is served from is the browser's answer, not the listener's.** The display client builds its WebSocket URL from the origin root plus a `path` setting, ignoring the path of the page it is on — so `path` has to name the panel's public prefix. Behind a reverse proxy that prefix is not `/takeover/panel/`, and nothing in the request says what it is (a proxy worth trusting forwards no `Origin`, no `Referer` and no forwarded-prefix header). So the landing page computes it in the browser from `location.pathname`. Delivered directly that produces the same value it always did; delivered under any prefix at all, it produces that prefix.

## 10. Backends

`afhttp`'s protocol layer (fetch logic, CDP escape hatch, display takeover) is CDP-generic. The launcher layer (`afhttp host`) knows specific browser families.

| Backend | Launch profile in `host` | Capabilities |
| --- | --- | --- |
| Chromium / Chrome / Edge / Brave | `chromium` (and aliases) | Full: body, rendered_html, text, content, content_json, screenshot, network, console, observation, network body capture, real-display takeover, health/capabilities, multi-attach. |
| fingerprint-chromium | `fingerprint-chromium` | Same capability matrix as Chromium, including real-display takeover. Engine surface (UA, navigator props, WebGL vendor, canvas/font enumeration, CDP-detection evasion) is spoofed per [adryfish/fingerprint-chromium](https://github.com/adryfish/fingerprint-chromium). The host derives a stable 32-bit `--fingerprint=<seed>` from the resolved profile path so identity stays consistent within a profile and diverges between profiles. Per-surface overrides (`--fingerprint-brand`, `--fingerprint-platform`, etc.) reach the engine via `--browser-arg`. |
| Lightpanda | `lightpanda` | body, rendered_html (modulo JS engine limits), text, network metadata, console, limited observation. No screenshot, no display takeover (no rendering), network body capture depends on backend support. |
| Camoufox (via foxbridge) | `camoufox` | Firefox stealth fork driven by the [foxbridge](https://foxbridge.vulpineos.com/) CDP→Juggler proxy. Same artifact subset as Lightpanda for CDP-only features: no chromium-only screenshot. Display takeover is supported because the human drives the real X display. Body, rendered_html, text, network metadata, console, observation work. Persistent profiles refused with `backend_unsupported` until Firefox profile lifecycle is wired explicitly. The host spawns foxbridge with `--binary <camoufox>` on a pre-reserved port; the SDK sees a chromium-style WebSocket. |
| Any other CDP-compatible browser | none — user launches it themselves | Whatever the backend implements. `afhttp` clients connect via `--endpoint-url`. |

Unsupported per-artifact operations return per-artifact warnings (`backend_unsupported`), not whole-fetch failures.

### Why both fingerprint-chromium and camoufox exist

Both backends add stealth, but they address different threat models and are complementary rather than redundant:

| | fingerprint-chromium | camoufox |
|---|---|---|
| **Engine** | Chromium (Blink) | Firefox (Gecko) |
| **Stealth mechanism** | Patches applied at the Chromium binary level: UA, navigator props, WebGL/Canvas entropy, CDP-detection evasion | Firefox stealth fork with Gecko-native fingerprint randomization; engine-level font/audio/WebGL divergence from stock Chromium |
| **Full artifact support** | Yes — screenshot | No — subset only (no screenshot) |
| **Target profile** | Sites that block stock headless Chromium or `navigator.webdriver` detection | Sites that actively fingerprint Blink engine characteristics (WebGL vendor strings, V8 timing side-channels) and block Chromium-family browsers regardless of stealth patches |
| **Real-display takeover** | Supported, currently backed by KasmVNC | Supported, currently backed by KasmVNC |

Sites that specifically block all Chromium-family browsers (rare but real) require camoufox. Sites that just block unpatched headless work fine with fingerprint-chromium and get the full capability matrix. Both stay so operators can choose the right tool for the threat.

## 11. Error Codes

All error events carry `error.code` (stable enum), `error.message` (human-readable detail), and `error.retryable` (bool). Agents match on `error.code` only — the message is for human logs and may change between versions.

Three categories:

- **Transport / navigation** — `navigation_timeout`, `host_unreachable`, `dns_resolution_failed`, `target_unreachable`, `tls_error`, `tab_crashed`, `browser_launch_failed`. Most are retryable; `tls_error` is not.
- **Per-artifact/readiness/classification warnings** — `backend_unsupported`, `artifact_capture_failed`, `artifact_capture_timeout`, `network_body_truncated`, `network_not_idle`, `pending_xhr_at_capture`, `artifact_empty`, `artifact_tiny`, `bot_wall_detected`, `security_challenge_detected`, `observation_empty`, `readiness_timeout`. These populate `warnings[]` on an otherwise-successful response; the fetch itself does not fail. Challenge classifications also set `page_kind` and `next_action` so agents do not treat the transport response as verified target content.
- **Configuration / profile** — `invalid_argument`, `invalid_endpoint`, `render_unavailable`, `profile_*`, `io_error`. Not retryable without fixing the configuration.

The full enum with example `error` strings and per-code agent guidance lives in [reference.md §Error Codes](reference.md#error-codes).

## 12. Multi-Client Attach

CDP allows multiple flattened sessions per target. The agent and the human (via real-display takeover) are independent clients. The browser is shared state.

Coordination is the agent's concern, not the protocol's. Common pattern: the agent emits an out-of-band signal (e.g. to its own orchestrator) saying "I need help on `<endpoint>`/tab `<id>`". A human runs `afhttp panel --endpoint-url ...`, opens the display takeover, does their part, and closes it. The agent's next `afhttp fetch --tab <id>` or `afhttp cdp` continues from the new browser state.

The Rust SDK keeps one lazy CDP WebSocket per `Client` and reuses it across `fetch` / `cdp` calls until `Client::close().await` or drop. This cache is per SDK client, not a browser-wide lease: other SDK clients still attach through their own CDP connections (and a human can drive the real display in parallel), and all of them can continue to multi-attach to the same target. Each one-shot `cdp --tab` / `fetch --tab` operation detaches its temporary flattened session when the call completes; `--tab` controls target lifetime, not connection ownership.

There is no "lease," "lock," or "active driver" in the protocol. Both clients can issue commands at any time; if they conflict, that is the user's coordination bug to solve.

## 13. Library / SDK

The Rust library exposes the same surface as the CLI, in-process. It is **not** an embedded browser engine; it is an SDK that talks to a `browser-host` over CDP/HTTP. Everything that physically requires a Chromium process — launching, active profile locking, and display takeover — stays in `afhttp host`. Local profile lifecycle helpers operate on disk and do not pull browser-launch dependencies into SDK-only consumers.

```rust
use afhttp::{Client, RenderMode, Wait, Artifact};
use afhttp::sdk::{FetchCookie, FetchCookieSameSite};

let client = Client::connect("ws://chromium-host:9222")?;

let result = client.fetch("https://example.com")
    .render(RenderMode::Always)
    .wait(Wait::Auto)
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

**What the SDK does not expose**: chromiumoxide types, host launch internals, display-takeover internals, or a remote profile-administration API.

**CLI is the first SDK consumer.** `afhttp fetch` and `afhttp cdp` parse args, call into the SDK, format the response. They are not parallel implementations.

**Cargo features.**

```toml
[features]
default  = ["sdk", "cli"]
sdk      = []                                   # client-side; what library consumers want
host     = ["dep:chromiumoxide", ...]           # browser-launch deps; only in the bin
cli      = ["sdk", "host", "dep:psl",
            "agent-first-data/skill-admin",
            "agent-first-data/stream-redirect"] # the afhttp binary
```

The CLI has no argument-parser dependency of its own: argv parsing, legal
argument combinations, help, and `docs/cli.md` all come from the
`agent-first-data` `cli-spec-v1` registry in `src/cli/spec.rs`.

External consumers (e.g. the `fetch` service) depend on the crate with `default-features = false, features = ["sdk"]` and link only the SDK weight, not chromiumoxide or any browser-launch code.

## 14. Non-Goals

- Not a browser automation framework. No semantic action API (`click`, `type`, `navigate_with_form`). Use raw CDP.
- Not NAT traversal. Mesh is the user's responsibility.
- Not service discovery. `afhttp` does not register hosts; the user resolves endpoints.
- Not a daemon manager. `afhttp host` is a long-running foreground process; the user manages lifecycle.
- Not heuristic page understanding. No Readability extraction and no open-ended "is this a login page" / business-meaning detection. Challenge pages still return the page facts (status, headers, body, screenshot); conservative security-challenge markers may add `page_kind` / `next_action`, and the agent decides what to do next.
- Not semantic observation ranking. `observation.json` reports roles, names, states, and geometry; it does not decide which element matters.
- Not remote profile administration. Profile delete/prune/list operates on local disk only.
- Not an embedded engine in the library. The library is an SDK; engines live in `afhttp host`.
- Not Firefox / WebKit. CDP-only; backends that do not speak CDP are out of scope.
