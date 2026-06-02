# Agent-First HTTP

A URL acquisition tool for AI agents — give it a URL, get back the rendered page and the artifacts (HTML, screenshot, network and console logs, DOM observation) an agent needs to decide what to do next.

## The problem: agents often cannot reach the page

The hard part for an agent is not fetching bytes. It is that many useful URLs do not turn into a usable page from a simple shell request. Modern pages depend on JavaScript rendering, cookies, session state, captchas, and sometimes a browser fingerprint the target site recognizes. When acquisition fails, a human can open a browser and inspect; an agent needs the same facts as data it can branch on.

## What `afhttp` does

`afhttp` covers the whole acquisition range behind one structured contract:

- **Plain HTTP fetch** when the page works without a browser.
- **Browser-backed fetch** when it does not, producing rendered HTML, an observation snapshot, screenshot, and network/console logs as artifacts.
- **Deep network capture** when the useful data arrives through XHR/fetch/GraphQL instead of the initial document.
- **Raw CDP escape hatch** when the agent needs to drive the browser directly (DOM inspection, form submission, custom waits) without a "click/type" abstraction layer.
- **Ops panel** when a human needs to step in (manual login, captcha, 2FA) on the same browser the agent is using — the default panel needs no VNC/X server, and an optional KasmVNC display-takeover mode is available for hard sites.
- **Health/capabilities endpoints and profile tools** for host readiness, backend planning, captured-download listing, and local persistent-profile lifecycle management.

Every output is structured JSON. Every failure carries a stable `error_code`. The tool never decides what a page means or what to do next — the agent does.

## Two roles

| Role | Command | What it does |
| --- | --- | --- |
| **browser-host** | `afhttp host` | Long-running process. Holds Chromium + an on-disk profile. Exposes a CDP endpoint and the ops panel. |
| **agent-driver** | `afhttp fetch`, `afhttp upload`, `afhttp cdp`, `afhttp ui`, `afhttp health`, `afhttp capabilities`, `afhttp profile`, `afhttp tabs`, or the Rust SDK | Short-lived client. Connects to a host's endpoint when needed, does work, writes artifacts locally. |

Hosts and drivers are independently locatable: run the host where the browser needs to be (residential IP, GUI machine, datacenter); run the driver wherever the agent runs. Connectivity is your mesh's problem.


The CLI has 9 commands: `host`, `fetch`, `upload`, `cdp`, `ui`, `health`, `capabilities`, `profile`, and `tabs`.

Fetch success output keeps artifact paths flat at the top level:

```json
{
  "code": "fetch",
  "status": 200,
  "final_url": "https://example.com/",
  "body_file": "/work/afhttp-out/req/body.html",
  "rendered_html_file": "/work/afhttp-out/req/rendered.html",
  "network_file": "/work/afhttp-out/req/network.json",
  "trace": {"render_decision": "browser", "render_used": true, "duration_ms": 820}
}
```

## Running the host

The **driver** commands are a thin client — install the binary and run them
wherever the agent is. The **host** runs a real browser (launched `--no-sandbox`,
so the container is the isolation boundary), holds a profile, and serves a
full-control CDP endpoint — so **run the host in a container**. This spore ships
one at [`container/docker/`](container/docker/) (chromium by default, other
backends opt-in via build args, token-by-default):

```bash
docker build -t afhttp-host -f container/docker/Dockerfile .
docker run --rm -p 9222:9222 --shm-size=1g -v afhttp-profile:/data afhttp-host
```

See [docs/deployment.md](docs/deployment.md) for backends, security, and human takeover.

## Install

**macOS / Linux — Homebrew**

```bash
brew install agentfirstkit/tap/afhttp
```

**Windows — Scoop**

```powershell
scoop bucket add agentfirstkit https://github.com/agentfirstkit/scoop-bucket
scoop install afhttp
```

**Any platform — Cargo**

```bash
cargo install agent-first-http
```

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
