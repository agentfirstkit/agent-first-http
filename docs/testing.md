# Testing Strategy

This project optimizes for functional correctness and protocol stability, not stress/performance testing.

`tests/test.sh` is the fixed local/container entrypoint used by the monorepo
driver and release gate. Native CI legs run the corresponding Cargo checks
directly.
It is written against the [`agent-first-data`](https://github.com/agentfirstkit/agent-first-data)
Bash kit, so `afdata` has to be on `PATH` (or named by `AFDATA_BIN`) to drive
the harness — the container it drives needs none of that. All Cargo work driven
by this entrypoint runs inside the container.

## Default Gates

Run on every PR.

1. Static checks
   - `bash tests/test.sh static` (runs ShellCheck at the pinned version on the host, then `cargo fmt`, `cargo build`, and `cargo clippy` inside the selected test container)

2. Unit + component tests
   - `bash tests/test.sh unit` (runs lib/bin tests plus `tests/check_regressions.sh` inside the selected test container)
   - Focus: argument parsing, endpoint URL handling, fetch builder shape, CDP message framing, artifact path resolution, observation/network schema serialization, health/capabilities clients, SDK error mapping.
   - No browser process required.
   - Deterministic broadcast fixtures verify recovery after event loss, capture
     limits, idle-based network quiet after loss, and collector cancellation on drop.
     A local CDP responder counts body requests to verify known oversized responses
     are skipped before reading or decoding their body.

3. Regression list
   - Covered by `bash tests/test.sh unit`; it runs `tests/check_regressions.sh` inside the selected test container.
   - Every production bug fix adds or updates a regression entry.

## Browser Integration Suite

Runs in CI when a browser binary is available, and locally on demand. Separated from the default gates because it requires Chromium installed and is slower.

### What it covers

- `afhttp host` startup, profile directory lifecycle, listener binding, graceful shutdown.
- `afhttp fetch --render=auto` against a local fixture HTTP server (gates the HTTP fast path).
- `afhttp fetch --render=always` against a JavaScript-rendered fixture page (gates the browser escalation path).
- Browser default artifacts plus opt-in `storage` are produced and readable in `--out` when supported, including `observation.json`.
- Deep `network.json` entries for document, script, XHR/fetch, redirect, failed resource, and optional body capture under `network-bodies/`.
- `afhttp health` and `afhttp capabilities` round-trip against a running host, including token-required, percent-encoded query tokens, minimal-public-health, degraded backend summaries, real tab counts, and implemented capability feature flags.
- `afhttp cdp` round-trip for a known CDP method (`Browser.getVersion`).
- Multi-client attach: two SDK clients to the same endpoint, both observe the same `Page.frameNavigated`.
- Profile isolation: a `--profile` host's cookies are not visible to a separate `--profile -` host.
- Profile lifecycle tooling: list/info/lock-status/downloads/delete/prune, refusing locked profile deletion.
- Error code coverage: `navigation_timeout`, `profile_locked`, `host_unreachable`, `cdp_unavailable`, `profile_not_found`, `profile_delete_locked`.

### Observation artifact tests

Fixture pages cover:

- buttons, links, inputs, checkboxes, selects, disabled controls, labels, ARIA names, iframes, and hidden/offscreen nodes
- stable per-snapshot refs, frame ids, bounding boxes, visible/enabled/focused/checked state, and redacted input value metadata
- explicit non-goals: no generated "login", "captcha", "important", or "best action" labels in `observation.json`

The tests compare normalized JSON snapshots. Browser-version-specific geometry tolerance is allowed only for pixel-level bounding-box drift.

### Network artifact tests

Fixture pages cover:

- top-level document load, redirects, script/style/image resources, XHR/fetch JSON, GraphQL-shaped JSON, failed requests, cached requests, and service-worker responses when supported
- default redaction for `Cookie`, `Authorization`, `Proxy-Authorization`, `Set-Cookie`, and token/secret-like headers
- `--network-bodies off|xhr|all`, per-body byte limits, UTF-8 text bodies, binary bodies, and body-capture warning paths
- real CLI error canaries for URL userinfo and credential query parameters,
  including unreachable and malformed targets; normal output goldens remain stable

The network tests assert structure and linkage rather than exact event order when CDP does not guarantee ordering across resource types.

### Health, capabilities, and profile tests

Unit tests cover JSON shapes without a browser. Integration tests cover:

- `/health` shallow readiness before and after browser startup, plus degraded status when the browser process exits
- takeover retains explicit tabs for unmarked 403 challenges, unknown responses and incomplete target readiness; successful completion recycles the final work tab with a browser-window anchor
- metadata fault injection covers ENOSPC after a partial write, WriteZero, interrupted writes and abandoned temporary files without overwriting old metadata
- live display plus a terminated isolated browser degrades health and rejects new handoffs, panel access and lease renewal; existing persistent profiles are never killed by these tests
- `/capabilities` matching the selected backend and reporting unsupported artifacts as unsupported rather than absent
- `afhttp profile` behavior on real temp profile roots, including metadata creation, missing metadata fallback, lock detection, captured-download listing, delete confirmation, and prune dry-runs

### Browser discovery

The suite resolves Chromium in this order:

1. `AFHTTP_TEST_BROWSER_BIN` (explicit path). An explicit path is authoritative: if it does not name a file, the browser counts as missing and discovery does **not** fall back to the standard paths.
2. Without that variable, the standard install paths (`/usr/bin/chromium`, `/usr/bin/chromium-browser`, `/usr/bin/google-chrome`, `/usr/bin/google-chrome-stable`, `/Applications/Google Chrome.app/...`).

The other backends follow the same rule through their own variables (`AFHTTP_TEST_LIGHTPANDA_BIN`, `AFHTTP_TEST_FINGERPRINT_CHROMIUM_BIN`, `AFHTTP_TEST_FOXBRIDGE_BIN`, `AFHTTP_TEST_CAMOUFOX_BIN`).

What a missing backend means is decided by `AFHTTP_TEST_REQUIRE_BACKENDS`, a comma-separated list of backend names (`chromium`, `lightpanda`, `fingerprint-chromium`, `camoufox`):

- A backend **named in the list** must be present. A test that cannot find it **fails** with `required backend <name> is missing` instead of returning early.
- A backend **not in the list** is optional: its tests print a `(skipping: ...)` line and pass, so a green run does not show that backend was exercised.

The test container sets the list to every backend its image installs (`tests/container-entrypoint.sh`): `chromium` and `lightpanda` always, `fingerprint-chromium` on x86_64, `camoufox` when the image was built with `AFHTTP_TEST_INSTALL_CAMOUFOX=1`. The native CI legs require `chromium`. `tests/check_backend_requirements.sh`, part of the `unit` mode, proves both halves against real test binaries: a required backend pointed at a nonexistent path fails, an optional one skips.

### Running locally

Native arm64 test images do not need Rosetta. If Apple `container` reports that
Rosetta is unavailable while creating its builder, set the user configuration
at `~/.config/container/config.toml` before running the fixed gate:

```toml
[build]
rosetta = false
```

```bash
# Default gates (Apple container on local macOS)
bash tests/test.sh

# Refresh the base image, browser packages, and moving backend downloads.
# Cargo cache volumes and running hosts are retained.
bash tests/test.sh refresh

# Read Rust, Chromium, and Lightpanda versions from the resulting image.
bash tests/test.sh versions

# Full integration suite (Apple container on local macOS)
bash tests/test.sh integration

# Only the browser download-to-disk tests (browser_navigation_download* in
# tests/browser_fetch.rs), for iterating on download handling
bash tests/test.sh download

# The integration mode runs real test files including:
# tests/browser_fetch.rs, tests/fetch_http_only.rs, tests/health_capabilities.rs,
# tests/cdp_proxy.rs, tests/cookie_jar_isolation.rs, tests/env_isolation.rs,
# tests/display_takeover.rs, tests/network_artifact.rs, and tests/tabs_management.rs
```

### CI (`.github/workflows/ci.yml`)

Linux is covered by the **`integration-docker`** job, which runs the full
integration suite through the Docker harness (real Chromium, available optional backends, and
KasmVNC, `AFHTTP_NO_SANDBOX=1` so the in-container sandbox is off). There is no
ubuntu *native* leg: the ubuntu runner's chromium is a confined snap that can't
complete download-to-disk tests.

Camoufox/foxbridge installation is opt-in with `AFHTTP_TEST_INSTALL_CAMOUFOX=1`.
Fingerprint Chromium is unavailable on arm64. These are the only backends
left out of `AFHTTP_TEST_REQUIRE_BACKENDS` in the container, so they are the
only ones whose tests may return early; a passing gate does not demonstrate
that they were exercised unless the image carries them.

The **native** `integration` matrix validates the binaries actually shipped, and
runs with Chromium's sandbox **on** (no `AFHTTP_NO_SANDBOX`) since these are
normal desktops:

| OS | Browser | Source |
| --- | --- | --- |
| macos-latest | Chrome | preinstalled by GitHub-hosted runner (Homebrew binary) |
| windows-latest | Chrome | preinstalled by GitHub-hosted runner (Scoop binary) |

The matrix sets `RUST_MIN_STACK=16 MiB` so the deep fetch/host future chain
doesn't overflow Windows' small default thread stack.

The fixed harness selects Apple `container` on macOS, then Podman, then Docker.
`AFHTTP_TEST_CONTAINER_RUNTIME=apple|podman|docker` selects an explicit backend;
`AF_CONTAINER_RUNTIME` supplies a shared override. CI can explicitly select Docker.
A selected backend failure stops the run instead of trying another backend.

The flaky-by-design **display takeover** suite runs in its own non-blocking
nightly workflow (`.github/workflows/takeover-panel.yml`), not in the gating
`ci.yml`. The Lightpanda backend is exercised inside `integration-docker` when
its binary is present.

## Display Takeover Tests

Real-display takeover is exercised through the `display_takeover.rs` integration
suite: it boots an `afhttp host --takeover-provider kasmvnc`, asserts the host brings up
the KasmVNC display provider, serves `/takeover/panel` through the
authenticated listener, and reports `takeover.supported: true` in `/capabilities`.
These run in the nightly takeover workflow because the provider startup is
timing-sensitive and version-dependent.

`tests/takeover-in-workbench.sh` is the other half, and is deliberately not a
gate. It takes a real takeover — Xvnc, a headful browser on its display, a real
`afhttp ui takeover --mode session` — and puts it behind
`agent-first-ui`'s `afui session serve`, then drives the result with a real
Chromium over CDP. It reports what it observed (HTTP statuses, the first RFB
frame off the WebSocket, what each browser document says about its own origin
and storage) rather than only whether it passed, and it writes screenshots to
`target/`. It needs a checkout of agent-first-ui beside this repository and
refuses to run without one, which is why it stays out of `tests/test.sh`.

## What is not tested

- **Anti-detection effectiveness**. Whether a specific site classifies a
  takeover-driven session as bot or human is non-deterministic and
  version-dependent. The architecture's risk-control statements
  (`architecture.md §9`) are deliberately framed as honest assessments, not test
  contracts.
- **Performance / throughput**. The project does not promise latency or request-per-second targets.
- **Network conditions**. Tests assume the loopback / fixture server is reachable; no chaos/network-impairment testing.

## Readiness regressions and a warmed host

`tests/test.sh unit`, `tests/test.sh integration`, and `tests/test.sh static`
cover wait parsing, compatibility, redaction and diagnostic limits, plus browser
fixtures for delayed prices, continuous changes, pending ancillary frames,
required same-origin/cross-origin/streaming frames, shadow-host frame ownership,
and real challenge documents. Use the configured test container runtime; do
not mount the same Apple test volumes in concurrent gate invocations.

For a real-site comparison without replacing a running host or system binary:

```sh
DOCKER_CONTEXT=colima python3 tests/live-readiness.py \
  --build --profile vps --url https://provider.example/ \
  --selector '#prices' --out /tmp/readiness-comparison
```

The fixed entrypoint builds through `tests/build.sh --native`, checks active
fetch/takeover processes and the host's current profile, then runs strict and
scoped captures sequentially. It refuses to switch profiles or start/rebuild a
host and stops when review is requested, including a retained incomplete
capture. It verifies retained tabs with `Page.bringToFront`. `--wait-mode target`
selects only the scoped capture; `--wait-mode load` exercises load-based
handoff, and `--tab ID` reuses a caller-specified tab with one wait mode. Review
a challenge before authorizing a re-fetch. Inspect
`summary.json`, the saved finite results, and composed content; readiness alone
cannot verify the required business fields. See [readiness](readiness.md).
