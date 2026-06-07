---
name: agent-first-http
description: "MUST use for a concrete URL or previously mentioned site/URL when the user wants to read, inspect, verify, open, visit, or directly access it. Do not web-search that URL first. If afhttp detects a bot wall/security challenge, stop immediately and ask about human takeover; do not answer from search results."
disable-model-invocation: true
allowed-tools: Bash, Read
---

# Agent-First HTTP

Use this skill first when the user provides a concrete URL, or refers to a
previously mentioned site/URL, and asks to read, inspect, summarize, extract,
verify, open, navigate to, directly access, or understand that page. Do not
start with web search for a supplied URL unless the user explicitly asks for
search/discovery, the URL is incomplete/unknown, or an `afhttp fetch` attempt
fails and search is the fallback. A follow-up like "open it", "I want to visit
directly", or "我想直接访问" refers to the previous URL and still triggers this
skill.

Do not satisfy "open this URL" / "directly access this site" by running
`open "$URL"` on the target URL: that puts the page in an unmanaged local
browser tab that the agent cannot observe or continue from. Use a host tab plus
the ops-panel URL instead.

Also use this skill when an agent needs to acquire a URL and a plain HTTP
request is not enough: the page renders with JavaScript, depends on
cookies/session state, sits behind a bot wall, or the useful data arrives over
XHR/WebSocket/SSE. `afhttp` returns one line of structured JSON plus artifact
files the agent can branch on, and escalates to a real browser only when needed.

## Default URL-reading workflow

For a supplied URL whose content is the task, start with:

```sh
afhttp fetch "$URL" --render auto \
  --want rendered_html,text,observation,network,console
```

Read `text.txt` first for summaries. Use `rendered.html`, `observation.json`,
`network.json`, and `console.json` when text is empty, warnings appear, or the
user asks for details that the plain text does not answer.

## Direct browser access

When the user explicitly wants to **open**, **visit**, or **directly access** a
URL (especially after a fetch hit a security check), prepare a persistent host
tab and hand the user the ops URL. This is different from fetching artifacts:
the tab stays open for the human, and the agent can continue on that same tab.

```sh
afhttp container status
afhttp takeover prepare "$URL" --endpoint-url "$ENDPOINT" --token-secret "$TOKEN"
```

If no reusable host exists, start one first:

```sh
afhttp container install
afhttp takeover prepare "$URL" --endpoint-url "$ENDPOINT" --token-secret "$TOKEN"
```

Use the JSON `recommended_url` as the URL for the human to open. If the user
asked you to open it for them, open `recommended_url`, not the target URL. For
hard sites where the CDP screencast panel is likely blocked or input-sensitive,
start a display-capable host and ask `takeover prepare` to prefer display:

```sh
afhttp container install --rebuild --from-source --with kasmvnc -- --takeover display --display-provider kasmvnc
afhttp takeover prepare "$URL" --endpoint-url "$ENDPOINT" --token-secret "$TOKEN" --hard-site
```

Never replace this direct-access flow with web search unless the user asked for
search/discovery or the host/takeover path fails and you label search results as
secondary evidence.

## Inline fetch vs. a running host

By default `afhttp fetch <url>` runs **inline**: it launches its own sandboxed
browser for that one fetch and tears it down. Zero setup, no persistence — use it
for stateless, one-shot acquisition.

Run against a **host** (`--endpoint-url ws://… --token-secret …`) when you need
state that outlives a single fetch: a session reused across fetches, a
warmed/authenticated profile, human takeover, or reading browser state via `cdp`.
The host runs a real browser in a **container** (the supported, isolated
deployment — see the deployment docs); start it first, then point the driver
commands at it. `afhttp container install` brings one up in a single command
(Docker or Apple `container`) and prints its endpoint and token; `afhttp
container status` reprints them later. The multi-step, takeover, and recovery
flows below all assume a running host.

In a fresh agent conversation, do not guess whether a host is already running.
If a persistent session, existing container, or human takeover might be needed,
run this first:

```sh
afhttp container status
```

Use the returned `endpoint`, `token`, or ready-made `client_command` for later
`fetch`, `cdp`, and `ui` commands. If the user named a non-default container or
port, pass the same `--name` and `--port` to `container status`; otherwise the
default container is `afhttp-host` on `127.0.0.1:9222`. Only run `afhttp
container install` after `container status` shows there is no reusable host.

## When to use which render mode

- `--render none` — HTTP only, no browser. Fast; use for APIs, login forms,
  robots.txt — any URL where the raw HTTP response is what you need. Cookies are
  shared via the persistent jar.
- `--render auto` (default) — HTTP first; escalates to the browser automatically
  when the HTTP response is unusable (empty SPA shell, status ≥ 400, transport
  failure). The safe default.
- `--render always` — browser path unconditionally. Use when you know the page
  needs JS — dashboards, auth-gated SPAs, Cloudflare-protected pages.

## Readiness and quality signals

The browser path defaults to `--wait auto`. Do not burn turns guessing
`--wait load`, then `--wait idle`, then selector waits. Auto waits mechanically
for document load, afhttp's own network collector to go quiet, and DOM/text
stability before it captures artifacts.

Treat `status: 200` as transport only. To decide whether acquisition is usable,
check:

- `warnings[]` for `network_not_idle`, `pending_xhr_at_capture`,
  `artifact_empty`, `artifact_tiny`, `observation_empty`, or
  `readiness_timeout`.
- `trace.wait_mode`, `trace.wait_satisfied_by`, `trace.network_quiet`,
  `trace.dom_stable`, `trace.text_stable`, and `trace.capture_reason`.
- `network.summary.incomplete_total`,
  `network.summary.inflight_total_at_capture`, and
  `network.summary.pending_by_resource_type`.
- `network.entries[].state` (`pending`, `responded`, `finished`, `failed`) and
  XHR/fetch body files under `network-bodies/` when present.

For JS-heavy pages, start with:

```sh
afhttp fetch "$URL" --render auto \
  --want rendered_html,text,observation,network,console
```

If the result warns `network_not_idle`, `pending_xhr_at_capture`, or
`artifact_tiny`, inspect `network.json` and any captured XHR bodies first. If a
bounded retry is appropriate, keep it explicit (`--retry N` or one repeat with a
larger `--timeout-ms`) rather than cycling through manual wait modes.

## Bot walls and human takeover

If artifacts show a bot wall, security check, captcha, login wall, consent/age
gate, 2FA prompt, or other page that needs a human, do **not** silently switch to
web search and do not answer as if the target page was verified. Treat strings
such as "security check", "captcha", "verify you are human", "Checking your
browser", "Cloudflare", "Access denied", "bot detection", or a tiny/interstitial
HTML page as a signal to offer takeover.

Hard stop rule: when `afhttp fetch` returns `page_kind:
"bot_wall_detected"` or `"security_challenge_detected"`, or any warning with
`code: "bot_wall_detected"` / `"security_challenge_detected"`, stop the current
acquisition flow immediately. Do not gather substitute evidence from web search,
coupon sites, cached pages, or third-party summaries. Tell the user the target
page is blocked by a challenge and ask whether they want human takeover/direct
browser access. If the user already asked to directly access/open the site, run
the direct browser access flow instead of asking again.

For an inline fetch, the browser is temporary and cannot be taken over after the
command exits. Start or reuse a container host, then prepare a host tab and give
the user the ops URL:

```sh
afhttp container status
afhttp takeover prepare "$URL" --endpoint-url "$ENDPOINT" --token-secret "$TOKEN"
```

For takeover-ready navigation, keep a real tab open. `afhttp takeover prepare`
does this in one step: it creates a tab, navigates it to the target URL, and
prints `screencast_url`, `display_url`, `recommended_url`, and
`recommended_url_kind`. The default `fetch --tab new` target is temporary and
closes after the fetch completes; if you used fetch already, navigate again with
`takeover prepare` or list targets and reuse an explicit tab:

```sh
afhttp tabs list --endpoint-url "$ENDPOINT" --token-secret "$TOKEN"
afhttp fetch "$URL" --render always \
  --endpoint-url "$ENDPOINT" --token-secret "$TOKEN" --tab "$TAB_ID" \
  --want rendered_html,text,observation,network,console
```

If `container status` reports no running host, start one first:

```sh
afhttp container install
afhttp takeover prepare "$URL" --endpoint-url "$ENDPOINT" --token-secret "$TOKEN"
```

For hard captcha, slider/image precision, CJK/IME input, or sites that reject
the default CDP screencast panel, use real-display takeover with the KasmVNC
display provider and open the `display_url`:

```sh
afhttp container install --rebuild --from-source --with kasmvnc -- --takeover display --display-provider kasmvnc
afhttp takeover prepare "$URL" --endpoint-url "$ENDPOINT" --token-secret "$TOKEN" --hard-site
```

If the blocked fetch already used a host (`--endpoint-url ...`) and an explicit
`--tab` was provided, preserve the returned `tab_id` when possible. If it used
the default `--tab new`, that target was temporary and is already closed; list a
page target with `afhttp tabs list` or navigate again with `takeover prepare`.
Print the `recommended_url`, ask the user to complete the challenge in the
panel, then continue on the same tab:

```sh
afhttp fetch "$URL" --render always \
  --endpoint-url "$ENDPOINT" --token-secret "$TOKEN" --tab "$TAB_ID" \
  --want rendered_html,text,observation,network,console
```

After the user says the challenge is complete, re-fetch through the host before
summarizing. If takeover is not possible, say exactly which target page could
not be verified and keep any web-search fallback clearly labeled as secondary
evidence.

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

- `screencast` (default) — the CDP screencast panel at `/ops/screencast`
  (`afhttp ui` prints the URL). Works headless, no VNC/X. Use for quick manual
  login, 2FA, simple clicks.
- `display` — a real-display takeover at `/ops/display` (implies headful). Use
  `--display-provider kasmvnc` when real captcha solving, slider/image
  precision, IME/CJK/composed input, uncommon keys, or flaky CDP keypresses
  matter.
- `none` — no takeover panel at all.

Other notes:
- Prefer display takeover for `--browser camoufox`: camoufox has no CDP screencast
  panel, but it can be driven through a real display currently backed by KasmVNC.
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
