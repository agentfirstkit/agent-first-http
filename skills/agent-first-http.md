---
name: agent-first-http
description: "MUST use for a concrete URL or previously mentioned site/URL when the user wants to read, inspect, verify, open, visit, or directly access it. Do not web-search that URL first. If afhttp detects a bot wall/security challenge, switch to `afhttp fetch --takeover`; do not answer from search results."
disable-model-invocation: true
allowed-tools: Bash, Read
---

# Agent-First HTTP

Use `afhttp` first when the user gives a concrete URL, or refers to a previous
URL/site, and asks to read, inspect, summarize, extract, verify, open, visit, or
directly access it. Do not start with web search for a supplied URL unless the
user explicitly asks for search/discovery, the URL is incomplete, or the afhttp
path fails and you clearly label search as fallback evidence.

For command details, prefer discovery over memorization:

```sh
afhttp --help
afhttp fetch --help
afhttp container --help
```

## Fetch A Page

Start with a structured fetch:

```sh
afhttp fetch "$URL" --render auto \
  --want content,content_json,observation,network,console
```

Read `content.md` first; it is the agent-oriented composed page view and should
include visible text from open shadow DOM, same-origin frames, cards, tables, and
links. Use `content.json` when you need to choose a follow-up link/action.
Inspect `observation.json`, `network.json`, and `console.json` only when needed.
Use `rendered_html` as a debug artifact, not as the authoritative rendered page.

Before answering, sanity-check `content.md` against stdout warnings. If the main
facts look incomplete, placeholder-like, or contradicted by readiness/network
warnings, do not treat the capture as final. Prefer a more specific same-site
link from `content.json.links`, one longer-wait retry, or clearly state that the
page did not fully settle.

Treat `status: 200` as transport only. If the page is a login, consent gate,
security check, captcha, or bot wall, do not answer as if the target page was
verified.

## Stay On Task

Keep the crawl bounded by the user's actual question. For follow-ups like
"that provider too" or "compare them", use the same product class and the
previously gathered facts; do not broaden into unrelated product lines unless
the user asks.

Stop as soon as official artifacts contain enough target content to answer
confidently. A single `--render always` retry is fine when the HTTP fast path or
the first render omits useful `content.md`, but do not keep drilling into app
bundles, JavaScript source, or extra APIs after prices/specs/products are
already visible in `content.md`, `content.json`, `observation.json`, or captured
`network-bodies`.

First-party network endpoints are acceptable only when they were observed in
that page's `network.json`, are public (`2xx` without authentication), and
directly explain the same visible page. Do not call authenticated/private APIs,
admin endpoints, or token-required APIs to answer a page-reading question; if
one returns `unauthorized`, `token required`, `forbidden`, or similar, abandon
that endpoint and answer from the official page artifacts you already have.

Fetch raw JS/CSS bundles only for debugging an afhttp/site rendering problem or
when the user explicitly asks for implementation details. Do not reverse
engineer site bundles just to answer normal pricing/product questions.

When the task needs a deeper same-site page, choose from
`content.json.links` first. Prefer visible, same-site links whose `kind` matches
the task (`product_detail`, `pricing`, or `docs`). Do not infer follow-up URLs
from JS bundles when `content.json.links` already contains relevant candidates.

## Direct Browser Access

When the user asks to open/visit/directly access a URL, use a managed host tab
so the agent can observe and continue. Do not open the target URL directly in an
unmanaged local browser.

Check for a reusable host:

```sh
afhttp container status
```

If no host is running, start one:

```sh
afhttp container install
```

Open a persistent tab and give/open the returned `next_action.takeover_url`.
The URL is a short-lived handoff; tell the user its expiry if
`next_action.takeover_url_expires_at_rfc3339` is present. The standard local
`afhttp-host` endpoint and token are discovered automatically:

```sh
afhttp fetch "$URL" --takeover
```

After the human finishes, run the returned `next_action.recommended_command` to
read the same tab before summarizing.

## Bot Walls And Human Takeover

If afhttp reports a `next_action.kind: "human_takeover"` (or only `page_kind:
"bot_wall_detected"` / `"security_challenge_detected"`), or artifacts clearly
show captcha/security verification (Cloudflare, "verify you are human",
"checking your browser", "access denied", etc.), enter a hard stop: do not keep
fetching the target, do not web-search for substitute answers, and do not use
third-party mirrors/proxies/readability services. The only allowed next step is
re-running the fetch with human takeover against a takeover-ready host:

```sh
afhttp fetch "$URL" --takeover
```

`afhttp container install` builds a takeover-ready host (Brave + KasmVNC real
display) by default. When `--endpoint-url` is omitted, `fetch --takeover`
discovers the standard local `afhttp-host` and reads its token; it does not
start containers for you. It opens a persistent tab and fetches once. By default
each site gets its own isolated profile (the URL's registrable domain /
eTLD+1), so logins and cookies don't leak across sites; pass
`--profile work` only to intentionally share browser state across sites. If the
warmed profile already reaches the target it returns the content directly (no
`next_action`); use that instead of bothering the user. If it returns
`next_action.kind: "human_takeover"`, give/open the complete
`next_action.takeover_url` (it normally expires after about 15 minutes), ask the
user to complete or confirm the visible browser state, then stop and wait. Only
after the user explicitly confirms should you run
`next_action.recommended_command` (a re-fetch of the same `--tab`).

If takeover still fails, report that the target page could not be verified and
name likely external causes such as IP/network reputation, account state, or
site policy.

## Host State

Inline `afhttp fetch` is one-shot and non-persistent. Use a container host when
you need session reuse, a warmed/authenticated profile, human takeover, or CDP
state inspection. A host serves one active profile at a time but switches at
runtime when a fetch passes `--profile` (the browser is relaunched under it);
persistent profile storage is scoped by backend, so `work` under Brave and
`work` under Chromium are separate identities.

Use `afhttp health`, `afhttp capabilities`, `afhttp tabs`, or `afhttp cdp` only
when the task needs that detail; check the relevant `--help` before doing so.
