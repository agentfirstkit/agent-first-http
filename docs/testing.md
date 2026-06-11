# Testing Strategy

This project optimizes for functional correctness and protocol stability, not stress/performance testing.

## Default Gates

Run on every PR.

1. Static checks
   - `bash tests/test.sh static` (runs `cargo fmt`, `cargo build`, and `cargo clippy` inside Docker)

2. Unit + component tests
   - `bash tests/test.sh unit` (runs lib/bin tests plus `tests/check_regressions.sh` inside Docker)
   - Focus: argument parsing, endpoint URL handling, fetch builder shape, CDP message framing, artifact path resolution, observation/network schema serialization, health/capabilities clients, SDK error mapping.
   - No browser process required.

3. Regression list
   - Covered by `bash tests/test.sh unit`; it runs `tests/check_regressions.sh` inside Docker.
   - Every production bug fix adds or updates a regression entry.

4. Coverage gate
   - `bash tests/test.sh coverage` (runs cargo-llvm-cov inside Docker)
   - Both the unit suite and the browser integration suite contribute, so the coverage job installs a chromium binary and exports `AFHTTP_TEST_BROWSER_BIN`. The regions threshold (65%) is the v0.5.0 floor and should ratchet up as artifact extractors deepen.

## Browser Integration Suite

Runs in CI when a browser binary is available, and locally on demand. Separated from the default gates because it requires Chromium installed and is slower.

### What it covers

- `afhttp host` startup, profile directory lifecycle, listener binding, graceful shutdown.
- `afhttp fetch --render=auto` against a local fixture HTTP server (gates the HTTP fast path).
- `afhttp fetch --render=always` against a JavaScript-rendered fixture page (gates the browser escalation path).
- All seven default artifacts plus opt-in `storage` are produced and readable in `--out` when supported, including `observation.json`.
- Deep `network.json` entries for document, script, XHR/fetch, redirect, failed resource, and optional body capture under `network-bodies/`.
- `afhttp health` and `afhttp capabilities` round-trip against a running host, including token-required, percent-encoded query tokens, minimal-public-health, degraded backend summaries, real tab counts, and implemented capability feature flags.
- `afhttp cdp` round-trip for a known CDP method (`Browser.getVersion`).
- Multi-client attach: two SDK clients to the same endpoint, both observe the same `Page.frameNavigated`.
- Profile isolation: a `--profile` host's cookies are not visible to a separate `--profile -` host.
- Profile lifecycle tooling: list/info/lock-status/downloads/delete/prune, refusing locked profile deletion.
- Error code coverage: `navigation_timeout`, `profile_locked`, `host_unreachable`, `tab_crashed`, `cdp_unavailable`, `profile_not_found`, `profile_delete_locked`.

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

The network tests assert structure and linkage rather than exact event order when CDP does not guarantee ordering across resource types.

### Health, capabilities, and profile tests

Unit tests cover JSON shapes without a browser. Integration tests cover:

- `/health` shallow readiness before and after browser startup, plus degraded status when the browser process exits
- `/capabilities` matching the selected backend and reporting unsupported artifacts as unsupported rather than absent
- `afhttp profile` behavior on real temp profile roots, including metadata creation, missing metadata fallback, lock detection, captured-download listing, delete confirmation, and prune dry-runs

### Browser discovery

The suite respects, in order:

1. `AFHTTP_TEST_BROWSER_BIN` environment variable (explicit path).
2. `which chromium`, `which chrome`, `which google-chrome-stable`.
3. Standard install paths per platform (`/Applications/Google Chrome.app/...`, `/usr/bin/chromium`, `C:\Program Files\Google\Chrome\...`).

When none is found, the suite is **skipped, not failed**, with a clear log line so the CI matrix can either provide a browser or accept the skip.

### Running locally

```bash
# Default gates only (Docker)
bash tests/test.sh

# Full integration suite (Docker)
bash tests/test.sh integration

# The integration mode runs real test files including:
# tests/browser_fetch.rs, tests/fetch_http_only.rs, tests/health_capabilities.rs,
# tests/cdp_proxy.rs, tests/cookie_jar_isolation.rs, tests/env_isolation.rs,
# tests/display_takeover.rs, tests/network_artifact.rs, and tests/tabs_management.rs
```

### CI (`.github/workflows/ci.yml`)

Linux is covered by the **`integration-docker`** job, which runs the full
integration suite through the Docker harness (real chromium + every backend +
KasmVNC, `AFHTTP_NO_SANDBOX=1` so the in-container sandbox is off). There is no
ubuntu *native* leg: the ubuntu runner's chromium is a confined snap that can't
complete download-to-disk tests.

The **native** `integration` matrix validates the binaries actually shipped, and
runs with Chromium's sandbox **on** (no `AFHTTP_NO_SANDBOX`) since these are
normal desktops:

| OS | Browser | Source |
| --- | --- | --- |
| macos-latest | Chrome | preinstalled by GitHub-hosted runner (Homebrew binary) |
| windows-latest | Chrome | preinstalled by GitHub-hosted runner (Scoop binary) |

The matrix sets `RUST_MIN_STACK=16 MiB` so the deep fetch/host future chain
doesn't overflow Windows' small default thread stack.

The flaky-by-design **display takeover** suite runs in its own non-blocking
nightly workflow (`.github/workflows/takeover-panel.yml`), not in the gating
`ci.yml`. The Lightpanda backend is exercised inside `integration-docker` when
its binary is present.

## Display Takeover Tests

Real-display takeover is exercised through the `display_takeover.rs` integration
suite: it boots an `afhttp host --takeover-provider kasmvnc`, asserts the host brings up
the KasmVNC display provider, serves `/takeover/panel` through the
authenticated listener, and reports `display_takeover: true` in `/capabilities`.
These run in the nightly takeover workflow because the provider startup is
timing-sensitive and version-dependent.

## What is not tested

- **Anti-detection effectiveness**. Whether a specific site classifies a
  takeover-driven session as bot or human is non-deterministic and
  version-dependent. The architecture's risk-control statements
  (`architecture.md §9`) are deliberately framed as honest assessments, not test
  contracts.
- **Performance / throughput**. The project does not promise latency or request-per-second targets.
- **Network conditions**. Tests assume the loopback / fixture server is reachable; no chaos/network-impairment testing.
