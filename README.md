# Agent-First HTTP

Give an AI agent its own isolated browser to actually open a URL — running JavaScript when the page needs it and returning the page as files the agent can read — so it works from the real page instead of a search guess, an empty app shell, or a login wall, all without touching the browser you use every day.

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
{"code":"fetch","request_url":"https://example.com","status":200,"final_url":"https://example.com/","body_file":"/tmp/afhttp-out/<id>/body.html","text_file":"/tmp/afhttp-out/<id>/text.txt"}
```

That is the whole job: **hand it a URL, get the page back as files an agent can read** — never a terminal blob to scrape, and every failure a stable `error_code` rather than a guess.

By default afhttp sends a **plain HTTP request** and only starts a **real browser** when the page actually needs one (`--render none` forces the fast path, `--render always` forces the browser, `--render auto` decides). A browser-backed fetch captures more of what a human would look at — an agent-oriented composed page view (`content.md`, the one to read first), rendered HTML, a screenshot, a DOM observation, the network and console logs — each a flat `*_file` field on the same JSON, never nested:

```json
{
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
}
```

## Browser backends: meet each site with the engine it demands

afhttp is not "headless Chromium." How hard a site fights back decides which engine actually reaches it, so afhttp drives a whole spectrum behind one CDP contract — pick one with `--browser` (or point `--browser-bin` at a binary):

- **chromium / chrome** — the default: full rendering, screenshots, downloads.
- **chrome-headless-shell** — a lean headless build for fast, low-overhead fetches.
- **fingerprint-chromium** — Chromium that randomizes its fingerprint per profile, for bot-walled sites.
- **camoufox** — a Firefox stealth fork (via foxbridge) for sites that fingerprint Chromium.
- **lightpanda** — an ultralight engine covering a rendering subset without a full browser.
- **edge** — Microsoft Edge, when a target expects that specific engine.
- **brave** — Brave, with built-in ad/tracker blocking; also the browser a human drives during takeover.

## Human takeover: a person drives the same browser when a step needs it

When a fetch hits a login, captcha, or 2FA wall, `afhttp fetch <url> --takeover` keeps a persistent tab open on a takeover-ready host and hands back a complete short-lived `takeover_url` a human opens to drive the *same* browser the agent is using, via real-display takeover backed by KasmVNC. Once the human is past the wall, the agent re-fetches the same tab to continue.

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

## Adopt it: hand afhttp to your agent

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

# or build from source after reviewing the repo
git clone https://github.com/agentfirstkit/agent-first-http
cargo install --path agent-first-http
```

Then install the embedded [Agent Skill](skills/agent-first-http.md) so the agent
follows afhttp's behavior rules — when to escalate `--render`, when to reach for
human takeover, how to read the artifacts. `skill install` targets Codex, Claude
Code, and opencode; `skill status` reports whether each install is present,
valid, and current:

```bash
afhttp skill install
afhttp skill status
```

To remove it, run `afhttp skill uninstall`.

## Docs

- [Overview](docs/overview.md) — narrative introduction with worked examples and Rust SDK usage
- [Architecture](docs/architecture.md) — the canonical contract: roles, CLI surface, profile model, artifacts, health/capabilities endpoints, human takeover, backends, error codes, SDK
- [Deployment](docs/deployment.md) — running the host in a container: backends, security, human takeover
- [Design Principles](docs/design.md) — codebase-wide conventions
- [CLI Reference](docs/cli.md) — flag-by-flag reference for the `afhttp` binary
- [Protocol Reference](docs/reference.md) — output schemas for fetch, cdp, health, capabilities, and profile results
- [Testing](docs/testing.md) — test strategy and coverage gates

## License

MIT
