---
name: agent-first-http
description: "URL acquisition for agents: one fetch returns the page plus structured artifacts (rendered HTML, DOM observation, screenshot, network/console logs), escalating from a plain HTTP request to a real browser when the page needs it. Use when a plain request can't turn a URL into a usable page — JS rendering, cookies, sessions, bot walls — or when an agent needs the page as data rather than raw bytes."
disable-model-invocation: true
allowed-tools: Bash, Read
---

# Agent-First HTTP

Use this skill when an agent needs to acquire a URL and a plain HTTP request is
not enough: the page renders with JavaScript, depends on cookies/session state,
sits behind a bot wall, or the useful data arrives over XHR/WebSocket/SSE. `afhttp`
returns one line of structured JSON plus artifact files the agent can branch on,
and escalates to a real browser only when needed.

## When to use which render mode

- `--render none` — HTTP only, no browser. Fast; use for APIs, login forms,
  robots.txt — any URL where the raw HTTP response is what you need. Cookies are
  shared via the persistent jar.
- `--render auto` (default) — HTTP first; escalates to the browser automatically
  when the HTTP response is unusable (empty SPA shell, status ≥ 400, transport
  failure). The safe default.
- `--render always` — browser path unconditionally. Use when you know the page
  needs JS — dashboards, auth-gated SPAs, Cloudflare-protected pages.

## Profiles are per host

A host binds exactly **one** profile (`afhttp host --profile <name>`); a driver
cannot switch profiles per fetch. To use a different identity, run another host
on its own profile. Isolation is strict — one profile's cookies/state are
invisible to another. `afhttp profile cookies <name>` shows a profile's jar
(values redacted) without connecting to the host.

## Multi-step flows

After a login fetch, the session cookie is in the jar; later fetches on the same
host reuse it automatically:

```sh
afhttp fetch https://example.com/login --method POST --data '{"user":"x","pass":"y"}' \
  --render none --endpoint-url ws://host:9222 --token-secret "$TOKEN"
afhttp fetch https://example.com/dashboard --render always --endpoint-url ws://host:9222 --token-secret "$TOKEN"
```

The jar is keyed to the profile directory. Use `--no-cookie-jar` for recon
traffic that must not carry session state.

## When to drop to `cdp`

Use `afhttp cdp <method> --params ...` when:

- you need a CDP action `fetch` doesn't expose (inject credentials via
  `Input.dispatchKeyEvent`, emulate geolocation via `Emulation.*`);
- you need to read browser state (cookies, localStorage, performance entries) directly;
- you're building a multi-step flow where `fetch` would re-navigate an already-logged-in tab.

## Choosing a takeover mode

`afhttp host --takeover` picks the human-takeover surface, like `--render`:

- `screencast` (default) — the CDP screencast panel at `/ops` (`afhttp ui` prints
  the URL). Works headless, no VNC/X. Use for quick manual login, 2FA, simple clicks.
- `kasmvnc` — a real KasmVNC X display at `/ops/display` (implies headful). Use when
  real captcha solving, slider/image precision, IME/CJK/composed input, uncommon
  keys, or flaky CDP keypresses matter.
- `none` — no takeover panel at all.

Other notes:
- Prefer display takeover for `--browser camoufox`: camoufox has no CDP screencast
  panel, but it can be driven through the in-container KasmVNC X display.
- Warm a persistent profile once through display takeover, then let the agent
  reuse that same profile.
- Don't oversell it as captcha bypass — it fixes display/input fidelity, not IP or
  fingerprint reputation; pair it with an appropriate proxy and backend.

## Escalation reason vocabulary

When `--render auto` escalates, `trace.escalation_reason` is one of:

| value | meaning |
|---|---|
| `empty_html_shell` | HTML response had no visible text — SPA bootstrap |
| `http_status <N>` | HTTP returned status ≥ 400 |
| `http_failed: <code>` | transport-level error (DNS, TLS, connect) |

## Recovery after `tab_crashed`

1. `afhttp health --endpoint-url <ep> --token-secret <t>` — if `status` is `"starting"`, wait and retry.
2. `GET /diagnostics` (auth required) — `stderr_lines` shows the last browser stderr (OOM, GPU crash, segfault).
3. If the browser is gone, restart the host. Persistent profiles, cookie jar, and login state survive restarts.
4. For intermittent crashes, add `--retry N` — it auto-retries `retryable: true` errors.

## Recovery after `browser_launch_failed`

1. `GET /diagnostics` — `stderr_lines` shows why the subprocess failed.
2. Common cause: no browser binary found. Check `afhttp capabilities --endpoint-url <ep>` → `backend.family`.
3. Specify the binary explicitly: `afhttp host --browser chromium --browser-bin /usr/bin/chromium`.

## Checking what the last fetch sent

If the host was started with `--recent-requests-cap N`, `GET /recent-requests`
returns a ring of the last N network events the browser observed — useful for
confirming the right cookies/headers went out.
