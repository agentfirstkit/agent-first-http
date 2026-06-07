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

afhttp keeps Chromium's OS sandbox **on by default** — running unsandboxed
against untrusted, often adversarial pages is the weakest posture, so it must be
opt-in, not the default. The shipped image sets `AFHTTP_NO_SANDBOX=1`, which
makes the host launch Chromium with `--no-sandbox --disable-setuid-sandbox`
(Chromium's own sandbox can't initialize as root without user namespaces anyway)
— **the container then is the isolation boundary.** Run `afhttp host` natively
only on a trusted host; there the sandbox stays on (set `AFHTTP_NO_SANDBOX=1`
yourself only in an environment where Chromium's sandbox can't start). The host
loads untrusted web content and holds live cookies/sessions, so it should run
isolated and disposable:

- non-root (the image already runs as an unprivileged `afhttp` user),
- the default seccomp profile and dropped capabilities,
- `--shm-size=1g` (or a `/dev/shm` mount) for Chromium,
- on a private network — never host networking with the port exposed publicly.

## Quick start

The driver embeds the host image recipe, so a brew-only install (no source tree)
can stand up a host in one command:

```bash
# Build the image if needed and run the host. Auto-detects Docker or Apple
# `container`; add optional backends with repeated --with (see below).
afhttp container install

# Show the running host, its endpoint, and a ready-to-run driver command:
afhttp container status

# Tail logs / tear it down (--purge also drops the image and build cache):
afhttp container logs -f
afhttp container uninstall --purge
```

`install` does **not** compile afhttp: it builds the canonical
`container/docker/Dockerfile` with `--build-arg AFHTTP_BIN_FROM=downloader`,
selecting a stage that **downloads the prebuilt release binary** matching the
driver's own version and the image architecture (`x86_64-unknown-linux-gnu` for
Docker on Intel, `aarch64-unknown-linux-gnu` for Apple `container` and arm64
Docker). BuildKit skips the unused `builder` (Rust) stage, so the image stays a
slim `debian:bookworm-slim` with no toolchain and needs no source tree. The
version is hard-pinned: if no release asset exists for this version/arch (e.g. a
dev build of an unreleased version), the build fails with a pointer to the
from-source path below — it never installs a different version.

**Runtime selection** is `--runtime docker|podman|apple` (auto-detected in that
order: `docker`, then `podman`, then Apple `container`), or the
`AFHTTP_CONTAINER_RUNTIME` env var. Podman behaves like Docker (rootless, no
daemon); Apple `container` builds `linux/arm64` and is started for you
(`container system start`) and has no compose, so on macOS the `afhttp container`
path — not compose — is how you run a host.

**From a source checkout** (development, or to run an unreleased version that has
no published release asset), pass `--from-source`: instead of downloading the
prebuilt binary, it builds the full `container/docker/Dockerfile` from the current
directory, or from `--context <dir>`. If you run the command from an agent scratch
directory, afhttp also falls back to the source checkout that built the current
binary when that checkout is still available. This works under any runtime — the
Dockerfile is runtime-agnostic — giving a 2×2 of {prebuilt, from-source} ×
{docker/podman, apple}:

```bash
afhttp container install                              # prebuilt, auto runtime
afhttp container install --from-source                # source build, auto runtime
afhttp container install --runtime apple --from-source  # source build under Apple
```

Caveat for `--from-source` **under Apple `container`**: its builder runs in a
separate persistent VM that defaults to **2 GiB**, and compiling afhttp pulls
`chromiumoxide` (its CDP-bindings crate needs several GB for one `rustc`), so the
build OOM-kills at the default size. The `-m` flag on `container build` does **not**
resize that VM — you must resize the builder itself once (**8 GiB is enough**; the
2 GiB default is not):

```bash
container builder stop && container builder delete
container builder start --cpus 4 --memory 8g
```

After that, from-source builds and runs fine under Apple `container`. (The prebuilt
download path never compiles, so it is unaffected and needs no resizing.)

Or drive the runtime CLI directly (Apple's `container` is docker-shaped, so the
same Dockerfile builds under it with `--platform linux/arm64`):

```bash
cd spores/agent-first-http

# Build the host image (chromium only; build context is the spore root):
docker build -t afhttp-host -f container/docker/Dockerfile .

# Run it. The entrypoint generates a bearer token on first start and prints it
# along with a ready-to-run driver command. The profile persists in the volume.
docker run --rm -p 9222:9222 --shm-size=1g -v afhttp-profile:/data afhttp-host

# …or with compose (toggles backends via WITH_* env, see below):
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
| `WITH_KASMVNC=1` | KasmVNC display provider for `--takeover display` | x86_64 + arm64 |

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
with `--takeover display --display-provider kasmvnc --display headful`. See
[architecture.md §9](architecture.md).

## Lifecycle

`afhttp host` is a single long-running foreground process; it manages its own
browser (and, with KasmVNC, its own `Xvnc`) — so there is no supervisor inside
the image. Process lifecycle (restart policy, scaling, scheduling) stays the
operator's responsibility, exactly as the rest of the contract assumes.
