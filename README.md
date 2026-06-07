# Agent-First HTTP

Give an AI agent any URL and get back a usable page — fetched directly, or rendered in a real browser when the page needs one — with a human able to take over the same browser for a login, captcha, or 2FA.

## The basics: hand it a URL, get the page back as data

Give afhttp a URL; it writes the page to disk and prints one line of JSON saying what it got:

```bash
$ afhttp fetch https://example.com
{"code":"fetch","status":200,"final_url":"https://example.com/","body_file":"afhttp-out/<id>/body.html","text_file":"afhttp-out/<id>/text.txt"}
```

That is the whole job: **hand it a URL, get the page back as files an agent can read** — never a terminal blob to scrape, and every failure a stable `error_code` rather than a guess.

By default afhttp sends a **plain HTTP request** and only starts a **real browser** when the page actually needs one (`--render none` forces the fast path, `--render always` forces the browser, `--render auto` decides). A browser-backed fetch captures more of what a human would look at — rendered HTML, a screenshot, a DOM observation, the network and console logs — each a flat `*_file` field on the same JSON, never nested:

```json
{
  "code": "fetch",
  "status": 200,
  "final_url": "https://example.com/",
  "body_file": "afhttp-out/<id>/body.html",
  "rendered_html_file": "afhttp-out/<id>/rendered.html",
  "text_file": "afhttp-out/<id>/text.txt",
  "screenshot_file": "afhttp-out/<id>/page.png",
  "network_file": "afhttp-out/<id>/network.json",
  "console_file": "afhttp-out/<id>/console.json",
  "observation_file": "afhttp-out/<id>/observation.json"
}
```

## Browser backends: meet each site with the engine it demands

afhttp is not "headless Chromium." How hard a site fights back decides which engine actually reaches it, so afhttp drives a whole spectrum behind one CDP contract — pick one with `--browser` (or point `--browser-bin` at a binary):

- **chromium / chrome** — the default: full rendering, screenshots, downloads.
- **chrome-headless-shell** — a lean headless build for fast, low-overhead fetches.
- **fingerprint-chromium** — Chromium that randomizes its fingerprint per profile, for bot-walled sites.
- **camoufox** — a Firefox stealth fork (via foxbridge) for sites that fingerprint Chromium.
- **lightpanda** — an ultralight engine covering a rendering subset without a full browser.
- **edge / brave** — when a target expects that specific engine.

## Human takeover: a person drives the same browser when a step needs it

For a manual login, captcha, or 2FA, a person drives the *same* browser the agent is using — through an ops panel that needs no VNC/X server by default, or optional real-display takeover for hard sites currently backed by KasmVNC — then hands it back with session state intact.

## Running it: inline for a one-shot, a host for sessions

`afhttp fetch <url>` with no `--endpoint-url` runs **inline** — it spins up a sandboxed browser for that one fetch and tears it down. Zero setup; use it for stateless, one-shot acquisition.

For state that outlives a single fetch — a reused login, a warmed profile, human takeover — run a long-lived **host** in a container (the isolation boundary) and point the driver at it with `--endpoint-url`. One command builds the image from a recipe embedded in the binary and runs it (Docker, Podman, or Apple `container`, auto-detected — override with `--runtime`); it prints a bearer token and a ready-to-run driver command:

```bash
afhttp container install
```

Building from a source checkout instead? `docker compose -f container/docker/compose.yaml up --build` is the from-source path. See [docs/deployment.md](docs/deployment.md) for the full container setup and security posture.

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
- [Architecture](docs/architecture.md) — the canonical contract: roles, CLI surface, profile model, artifacts, health/capabilities endpoints, ops panel, backends, error codes, SDK
- [Deployment](docs/deployment.md) — running the host in a container: backends, security, human takeover
- [Design Principles](docs/design.md) — codebase-wide conventions
- [CLI Reference](docs/cli.md) — flag-by-flag reference for the `afhttp` binary
- [Protocol Reference](docs/reference.md) — output schemas for fetch, cdp, health, capabilities, and profile results
- [Testing](docs/testing.md) — test strategy and coverage gates

## License

MIT
