# Takeover retention and runtime health

This page covers takeover tab ownership and takeover availability; wait
strategies and `readiness` are described in [readiness](readiness.md).

## When a takeover tab is reclaimed

`--takeover` reclaims its tab only after affirmative mechanical capture
evidence: a 2xx main response, a nonempty rendered document and visible text,
no recognized challenge, no failed/empty requested capture, and, when scoped
readiness was measured, a satisfied wait with ready target content. This is
not validation of business fields. Use `selector-stable:<css>` to identify
required content and inspect those fields in the returned artifacts.

HTTP 403 (including an unrecognized denial), missing/unknown content, failed
captures and readiness timeouts retain the tab. A short “Hang tight! We are
checking your browser” document is recognized without requiring a provider
marker. Unknown content gets `takeover_content_unverified` and a
`human_takeover` action without inventing a `page_kind`. A successfully loaded
HTTP error page is not successful takeover completion.

The result preserves status, artifacts, readiness signals, warnings and
`tab_id`. If minting a panel URL fails, the result keeps its review action,
tab and artifacts, adds the actual handoff error as a warning, and leaves
`takeover_url_secret` absent. Do not assume that every review action contains
a usable panel URL. Fetch errors report `trace.retained_tab_id` when a target
was retained; those failures do not become successful fetches.

A plain fetch with `--tab` retains the caller's tab. A successful takeover
re-fetch may recycle that tab. Driver cleanup and the idle reaper ensure a
dedicated inert `about:blank#afhttp-window-anchor` exists before closing a
page, so concurrent cleanup cannot close the host's final browser window.
If that guard fails, cleanup retains the original tab and reports the failure.
Raw CDP `Target.closeTarget` is an explicit low-level command and has no such
automatic protection.

## Configuration support and actual availability

`/capabilities.takeover` still describes configured backend/provider support.
Authenticated `/health` adds `build` and `takeover`:

| Field | Meaning |
| --- | --- |
| `configured` | Takeover is enabled with a display provider. |
| `browser_running` | Supervised browser process has not exited; `null` if not supervised. |
| `cdp_connected` | A fresh CDP connection answered `Target.getTargets`. |
| `display_connected` | Provider process and X/web connections are usable; test adapters check their web connection. |
| `ready` | Configured, browser has not exited, CDP and display probes passed. |
| `reason` | Why takeover is unavailable; omitted when ready. |

`container status.running` describes the container process. It can be true
while takeover is unavailable. Status adds `host_build`, `backend_connected`,
`takeover_configured` and `takeover_health`; `takeover_ready` reflects live
checks, not configured capability. A degraded health response keeps the
profile identity and provides the backend error.

The host checks before issuing or exchanging a handoff, renewing a UI lease,
loading the panel document, and accepting its WebSocket connection. Panel
HTML polls an authenticated status endpoint and shows a connection-loss
banner after repeated failures. These checks do not restart a browser, switch
profiles, or remove cookies. Diagnose the host before an operator restarts it.
Health is a bounded observation at a point in time, not a guarantee against a
process exiting immediately afterwards.

## Metadata durability and recovery

While holding the profile lock, the host writes metadata to a uniquely named
temporary file in the same directory, retries short/interrupted system writes,
syncs the completed file, atomically replaces `afhttp-profile.json`, then syncs
the parent directory on Unix. ENOSPC or another failure before replacement
leaves the previous file byte-identical. An interruption before replacement
can leave an unused temporary file; it cannot truncate the old metadata. A
directory-sync failure after replacement leaves complete new metadata but
reports that crash durability could not be confirmed.

An EOF or schema/name/backend error concerns `afhttp-profile.json`, not proof
of corrupt browser data. Back up the profile directory, recover or repair its
metadata using its actual name/backend, and preserve the browser files. Do not
delete/recreate an entire profile to repair a metadata parse error. Free disk
space separately before retrying.

## Older hosts

Against a host that predates these checks, the driver uses its
`backend.connected` health field before minting a handoff or UI lease (or
probes CDP directly if `/health` is disabled). It cannot attest to that host's
display liveness: status omits `takeover_ready` and emits a warning. Metadata
durability, host-side checks, the panel banner, and reaper protection require
a rebuilt host; browser fetch cleanup protection runs in the driver. A handoff
URL issued earlier by such a host is still exchanged by that host, so check its
authenticated health or mint a fresh handoff before opening it.
