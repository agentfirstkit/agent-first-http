# Deployment

`afhttp` has two roles, and they deploy differently:

- The **driver** (`afhttp fetch` / `cdp` / `upload` / `ui` / `health` /
  `capabilities` / `tabs`) is a thin client. Install it wherever the agent runs —
  no container needed.
- The **host** (`afhttp host`) runs a real browser, holds a persistent profile,
  and exposes a CDP endpoint. **Run the host in a container.** That is the
  supported deployment, and this spore ships one at
  [`container/docker/`](../container/docker/).

## Why the host belongs in a container

`afhttp host` launches Chromium with `--no-sandbox --disable-setuid-sandbox`, so
Chromium's own sandbox is off — **the container is the isolation boundary.** The
host also loads untrusted, often adversarial web content and holds live
cookies/sessions, so it should run isolated and disposable:

- non-root (the image already runs as an unprivileged `afhttp` user),
- the default seccomp profile and dropped capabilities,
- `--shm-size=1g` (or a `/dev/shm` mount) for Chromium,
- on a private network — never host networking with the port exposed publicly.

## Quick start

```bash
cd spores/agent-first-http

# Build the host image (chromium only; build context is the spore root):
docker build -t afhttp-host -f container/docker/Dockerfile .

# Run it. The entrypoint generates a bearer token on first start and prints it
# along with a ready-to-run driver command. The profile persists in the volume.
docker run --rm -p 9222:9222 --shm-size=1g -v afhttp-profile:/data afhttp-host
```

or with compose (toggles backends via `WITH_*` env, see below):

```bash
docker compose -f container/docker/compose.yaml up --build
```

Then, from wherever the agent runs (the driver needs no container):

```bash
afhttp fetch https://example.com \
  --endpoint-url ws://<host>:9222 --token-secret "<token-from-host-logs>"
```

## Security

The CDP endpoint is **full control of the browser and its profile** (cookies,
live sessions, downloads), so the container is **token-by-default**: if you don't
pass `AFHTTP_TOKEN`, the entrypoint generates one and persists it to the profile
volume (`/data/afhttp/host-token`). Set `AFHTTP_TOKEN` yourself to pin it.

afhttp does **not** terminate TLS. For cross-host use, keep the endpoint on a
private network and reach it as `wss://` through a mesh/proxy that provides TLS.
Never expose a tokenless endpoint on a public interface.

## Optional backends (build args)

Chromium is always present (backends `auto` / `chromium`). The rest are opt-in at
build time and arch-guarded (several upstreams ship x86_64-only Linux builds):

| Build arg | Adds backend(s) | Arch |
| --- | --- | --- |
| `WITH_CHROME_HEADLESS_SHELL=1` | `chrome_shell` | x86_64 |
| `WITH_LIGHTPANDA=1` | `lightpanda` | x86_64 + arm64 |
| `WITH_FINGERPRINT_CHROMIUM=1` | `fingerprint_chromium` | x86_64 |
| `WITH_CAMOUFOX=1` | `camoufox` (+ foxbridge) | x86_64 + arm64 |
| `WITH_KASMVNC=1` | real-display takeover (`--takeover kasmvnc`) | x86_64 + arm64 |

```bash
docker build -t afhttp-host:stealth \
  --build-arg WITH_CAMOUFOX=1 --build-arg WITH_FINGERPRINT_CHROMIUM=1 \
  -f container/docker/Dockerfile .
```

The pinned versions live in one place — `container/docker/install-backends.sh` —
shared with the test image (`tests/Dockerfile.test`) so they cannot drift.

### Proprietary browsers (Chrome / Edge / Brave)

These can't be redistributed, so they're not bundled. Mount the vendor binary into
the container and point at it:

```bash
docker run --rm -p 9222:9222 --shm-size=1g \
  -v /opt/google/chrome:/opt/google/chrome:ro \
  afhttp-host --browser chrome --browser-bin /opt/google/chrome/chrome
```

(`--browser chrome` also works when the binary is already on `PATH`.)

## Human takeover

The default ops panel (CDP screencast) needs no X or VNC and works in the slim
image — `afhttp ui --endpoint-url … --token-secret …` prints its URL. Real-display takeover
for hard captcha/IME sites needs `--build-arg WITH_KASMVNC=1`, then start the host
with `--takeover kasmvnc --display headful`. See
[architecture.md §9](architecture.md).

## Lifecycle

`afhttp host` is a single long-running foreground process; it manages its own
browser (and, with KasmVNC, its own `Xvnc`) — so there is no supervisor inside
the image. Process lifecycle (restart policy, scaling, scheduling) stays the
operator's responsibility, exactly as the rest of the contract assumes.
