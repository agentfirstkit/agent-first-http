# Deployment

`afhttp` has two roles, and they deploy differently:

- The **driver** (`afhttp fetch` / `cdp` / `upload` / `panel` / `health` /
  `capabilities` / `tabs`) is a thin client. Install it wherever the agent runs —
  no container needed.
- The **host** (`afhttp host`) runs a real browser, holds browser profile state,
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
- `--shm-size=2g` (or a `/dev/shm` mount) for Chromium on the takeover-ready
  host; `--takeover-provider off` builds lean down to `--shm-size=1g`,
- on a private network — never host networking with the port exposed publicly.

## Quick start

The driver embeds the host image recipe, so a brew-only install (no source tree)
can stand up a host in one command:

```bash
# Build the image if needed and run the host. Auto-detects Docker or Apple
# `container`; add optional backends with repeated --with (see below). The host
# is takeover-ready by default (Brave + KasmVNC + ephemeral initial profile + 2g shm).
afhttp container install

# Lean headless host instead (no takeover, 1g shm):
afhttp container install --takeover-provider off

# Show the running host, its endpoint, and a ready-to-run driver command:
afhttp container status

# Capture a structured log summary, or explicitly stream raw logs:
afhttp container logs
afhttp container logs --raw --follow

# Tear it down (--purge also drops the image and build cache):
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
afhttp container install                              # prebuilt, takeover-ready
afhttp container install --takeover-provider off               # lean headless host
afhttp container install --from-source                # source build, auto runtime
afhttp container install --runtime apple --from-source  # source build under Apple
```

### Version upgrades and profile preservation

Managed images are tagged with the driver version (`afhttp-host:<version>`), and
the prebuilt path downloads that exact `AFHTTP_VERSION` into the image. After
upgrading the local driver, run `afhttp container install` again: the new driver
builds/uses the matching image, stops and removes the old `afhttp-host`
container, and starts a replacement with the same named data volume
(`afhttp-host-data` by default). The host token and persistent profiles live in
that volume under `/data/afhttp`, so they survive the recreate; only ephemeral
profile state (`--profile -`) is intentionally disposable.

`afhttp container status` reports both the driver version and the running host
version. `fetch --takeover` auto-discovery refuses a standard local host whose
version differs from the driver and tells you to rerun `afhttp container
install`, rather than silently driving a stale protocol surface.

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

# Run it. The entrypoint generates a bearer token secret on first start and
# prints it along with a ready-to-run driver command. Persistent profiles and the
# host token live in the volume.
docker run --rm -p 9222:9222 --shm-size=1g -v afhttp-profile:/data afhttp-host

# …or with compose (toggles backends via WITH_* env, see below):
docker compose -f container/docker/compose.yaml up --build
```

Then, from wherever the agent runs (the driver needs no container):

```bash
afhttp fetch https://example.com \
  --endpoint-url ws://<host>:9222 --token-secret "<host-token>"
```

## Security

The CDP endpoint is **full control of the browser and its profile** (cookies,
live sessions, downloads), so the container is **token-by-default**: if you don't
pass `AFHTTP_TOKEN_SECRET`, the entrypoint generates a 32-byte base64url token
secret and persists it to the profile volume (`/data/afhttp/host-token`). Set
`AFHTTP_TOKEN_SECRET` yourself to pin it. The managed `afhttp container`
commands do not print this long-lived token unless you pass
`--reveal-token-secret`; takeover links use short-lived handoff URLs instead.

afhttp does **not** terminate TLS. For cross-host use, keep the endpoint on a
private network and reach it as `wss://` through a mesh/proxy that provides TLS.
Never expose a tokenless endpoint on a public interface.

## Optional backends (build args)

Chromium is always present (backends `auto` / `chromium`). The rest are opt-in at
build time and arch-guarded (several upstreams ship x86_64-only Linux builds):

| Build arg | Adds backend(s) | Arch |
| --- | --- | --- |
| `WITH_CHROME_HEADLESS_SHELL=1` | `chrome-headless-shell` | x86_64 |
| `WITH_LIGHTPANDA=1` | `lightpanda` | x86_64 + arm64 |
| `WITH_FINGERPRINT_CHROMIUM=1` | `fingerprint-chromium` | x86_64 |
| `WITH_CAMOUFOX=1` | `camoufox` (+ foxbridge) | x86_64 + arm64 |
| `WITH_BRAVE=1` | `brave` | x86_64 + arm64 |
| `WITH_KASMVNC=1` | KasmVNC display provider for `--takeover-provider kasmvnc` | x86_64 + arm64 |

```bash
docker build -t afhttp-host:stealth \
  --build-arg WITH_BRAVE=1 --build-arg WITH_KASMVNC=1 \
  -f container/docker/Dockerfile .
```

The pinned/download recipes live in one place —
`container/docker/install-backends.sh` — shared with the test image
(`tests/Dockerfile.test`) so they cannot drift. Brave is installed from Brave's
stable apt repository because it publishes architecture-native Debian packages.

### Proprietary browsers (Chrome / Edge)

These can't be redistributed, so they're not bundled. Mount the vendor binary
into the container and point at it:

```bash
docker run --rm -p 9222:9222 --shm-size=1g \
  -v /opt/google/chrome:/opt/google/chrome:ro \
  afhttp-host --browser chrome --browser-bin /opt/google/chrome/chrome
```

(`--browser chrome` also works when the binary is already on `PATH`.)

## Human takeover

Human takeover is real-display takeover backed by KasmVNC. The default
`afhttp container install` host is takeover-ready (Brave + KasmVNC + ephemeral
initial profile + 2g `/dev/shm`); a raw `afhttp host` opts in with `--takeover-provider kasmvnc`.
The driver-side entry point is:

```bash
afhttp fetch "$URL" --takeover
```

`fetch --takeover` auto-discovers the standard local `afhttp-host` when no
endpoint is supplied, switches the host to the URL-derived persistent site
profile (e.g. `contabo.com`), opens or reuses a persistent tab, and navigates it.
If the warmed profile already reaches the target, it returns the content.
Otherwise it returns a `next_action` with `kind: "human_takeover"`, a complete
short-lived `takeover_url` a human opens to drive the real display, expiry
metadata, and a `recommended_command` that re-fetches the same `--tab` once the
wall is cleared. For remote/custom hosts, pass
`--endpoint-url ws://<host>:9222 --token-secret "<token-secret>"`; the token is
used to mint the handoff URL and is not embedded in it. `afhttp panel
--endpoint-url … --token-secret …` prints a short-lived display URL directly.

If the takeover host still fails, the likely remaining causes are IP/network
reputation, account state, or site policy rather than the takeover surface. See
[architecture.md §9](architecture.md).

## Lifecycle

`afhttp host` is a single long-running foreground process; it manages its own
browser (and, with KasmVNC, its own `Xvnc`) — so there is no supervisor inside
the image. Process lifecycle (restart policy, scaling, scheduling) stays the
operator's responsibility, exactly as the rest of the contract assumes.
