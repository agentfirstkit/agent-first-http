# Agent-First HTTP - Protocol Reference

> Reflects the v0.5.0 implementation. [architecture.md](architecture.md) is the canonical contract; this file documents the on-wire JSON shapes the CLI and SDK actually emit. Coverage of the eight artifact tokens and 9 CLI commands matches `src/shared/`, `src/sdk/`, and `src/host/`.

All command outputs are single JSON objects on stdout unless otherwise stated. Artifact files are referenced by absolute `*_file` paths in those JSON envelopes.

Every failure envelope carries the standard fields below. `afhttp fetch`
execution failures also include `trace`; parse/configuration failures from
other commands may omit it.

| Field | Description |
| --- | --- |
| `error_code` | Stable machine-readable enum. Agents match on this field, not `error`. |
| `error` | Human-readable detail for logs. |
| `retryable` | Whether retrying the same operation may help. |
| `trace` | Best-effort timings and phase details on fetch execution failures. |

## Fetch Result

`afhttp fetch` returns one object when a URL acquisition attempt reaches an HTTP response or browser-rendered page state.

| Field | Present | Description |
| --- | --- | --- |
| `request_id` | always on success | Per-fetch id used in the default artifact directory. |
| `status` | if HTTP response exists | Final HTTP status code. 4xx/5xx are successful transport responses, not `error` envelopes. |
| `final_url` | always on success | URL after redirects/navigation. |
| `tab_id` | when a browser tab was used | CDP target/tab id for follow-up `afhttp cdp` or `afhttp fetch --tab`. |
| `body_file` | when `body` requested and body exists | Raw HTTP response body path. |
| `rendered_html_file` | when produced | Serialized post-JS DOM path. |
| `text_file` | when produced | `document.body.innerText` path. |
| `screenshot_file` | when produced | Full-page PNG path. |
| `network_file` | when produced | Deep network log path. |
| `console_file` | when produced | Console-event log path. |
| `observation_file` | when produced | Agent-readable accessibility/DOM snapshot path. |
| `storage_file` | when `storage` requested and produced | localStorage/sessionStorage/IndexedDB-name snapshot path. |
| `download_file` | when navigation becomes a download | Captured browser download path inside the active profile. |
| `download_bytes` | with `download_file` | Captured file size in bytes. |
| `download_filename` | with `download_file` | Browser-selected filename. |
| `download_url` | with `download_file` | URL that triggered the download. |
| `download_state` | with `download_file` | Mechanical state, currently `"completed"`. |
| `warnings` | if non-empty | Per-artifact or per-entry non-fatal failures. |
| `trace` | always | Render decision, readiness, phase timings, bytes, and escalation signals. |

Example:

```json
{
  "code": "fetch",
  "request_id": "req",
  "status": 200,
  "final_url": "https://example.com/",
  "tab_id": "page-1",
  "body_file": "/work/afhttp-out/req/body.html",
  "rendered_html_file": "/work/afhttp-out/req/rendered.html",
  "text_file": "/work/afhttp-out/req/text.txt",
  "screenshot_file": "/work/afhttp-out/req/page.png",
  "network_file": "/work/afhttp-out/req/network.json",
  "console_file": "/work/afhttp-out/req/console.json",
  "observation_file": "/work/afhttp-out/req/observation.json",
  "storage_file": "/work/afhttp-out/req/storage.json",
  "trace": {
    "render_decision": "browser",
    "render_mode": "auto",
    "render_used": true,
    "main_request_observed": true,
    "current_stage": "complete",
    "duration_ms": 820,
    "timeout_ms": 30000,
    "navigation_duration_ms": 540,
    "wait_mode": "auto",
    "wait_satisfied_by": "network_quiet_dom_text_stable",
    "network_quiet": true,
    "dom_stable": true,
    "text_stable": true,
    "capture_reason": "wait_satisfied",
    "stages": [
      {"name": "navigate", "status": "ok", "duration_ms": 340},
      {"name": "wait_readiness", "status": "ok", "duration_ms": 180},
      {"name": "capture_rendered_html", "status": "ok", "duration_ms": 25}
    ]
  }
}
```

### Warnings

Warnings do not fail the whole fetch.

| Field | Description |
| --- | --- |
| `artifact` | Artifact token, for example `screenshot`, `network`, or `observation`. |
| `code` | Stable warning/error code such as `backend_unsupported` or `artifact_capture_failed`. |
| `detail` | Human-readable detail. |
| `request_id` | Optional network request id when the warning applies to one network entry. |

## Trace

`duration_ms`, `timeout_ms`, `current_stage`, and `stages` are always present once fetch execution begins. Successful and failed fetch envelopes use the same trace shape.

| Field | Description |
| --- | --- |
| `duration_ms` | Total wall-clock time. |
| `timeout_ms` | Overall fetch budget from `--timeout-ms`. |
| `current_stage` | Stage active at snapshot time; `complete` on successful completion. |
| `render_decision` | `http_only` when the HTTP fast path was used, `browser` when a CDP-driven render was used. |
| `render_mode` | Requested render mode (`none`, `auto`, or `always`). |
| `render_used` | Convenience boolean mirroring whether the browser path actually ran. |
| `escalation_reason` | Stable token describing why the browser path was taken. Values: `"empty_html_shell"` (HTTP returned a JS-bootstrap with no visible text), `"http_status_NNN"` (HTTP returned status NNN), `"http_failed_<code>"` (transport error, `<code>` is the `error_code`). |
| `main_request_observed` | Whether the main document request was observed by the active fetch path. HTTP-only successes set this true; browser-internal URLs like `about:blank` or cancelled navigations may set it false. |
| `navigation_duration_ms` | Browser-path only: wall-clock from `Page.navigate` to the wait condition resolving. |
| `wait_mode` | Browser-path wait mode (`auto`, `load`, `idle`, `selector`, `selector_visible`, or `ms`). |
| `wait_satisfied_by` | Mechanical readiness condition that allowed capture, such as `network_quiet_dom_text_stable`, `load`, `selector`, or `network_idle_event`. |
| `network_quiet` / `dom_stable` / `text_stable` | `--wait auto` booleans showing whether afhttp's own network collector and DOM/text stabilization checks were satisfied at capture time. |
| `capture_reason` | Why artifacts were captured, for example `wait_satisfied`, `readiness_timeout`, or `download`. |
| `cookie_jar_file` | Absolute cookie jar path used for this fetch, when a jar was resolved. |
| `cookie_jar_warning` | Structured note when `/profile` was unavailable and implicit cookie-jar persistence was disabled. |
| `sensitive_capture` | Non-empty when `--network-redact off`, `--capture-ws`, or `--capture-sse` may write tokens/PII into artifacts. |
| `stages[]` | Ordered stage timings. Each stage has `name`, `status`, and `duration_ms`; `status` is one of `ok`, `error`, `timeout`, or `started`. |

Failure envelopes for `afhttp fetch` also include this trace:

```json
{
  "code": "error",
  "error_code": "navigation_timeout",
  "error": "fetch timed out after 90000ms during capture_text",
  "retryable": true,
  "trace": {
    "render_decision": "browser",
    "render_mode": "always",
    "render_used": true,
    "current_stage": "capture_text",
    "duration_ms": 90001,
    "timeout_ms": 90000,
    "main_request_observed": true,
    "stages": [
      {"name": "navigate", "status": "ok", "duration_ms": 929},
      {"name": "wait_readiness", "status": "ok", "duration_ms": 5000},
      {"name": "capture_text", "status": "timeout", "duration_ms": 84072}
    ]
  }
}
```

## Artifact Schemas

### `body_file`

Raw main-resource response body. The file is not redacted or transformed except decompression when requested by the fetch path.

### `rendered_html_file`

UTF-8 HTML serialization of the post-JS DOM. It is a browser artifact, not a readability or markdown conversion.

### `text_file`

UTF-8 text from `document.body.innerText`. This is mechanical visible text extraction and does not include summarization.

### `screenshot_file`

Full-page PNG. Missing screenshots produce a warning, usually `backend_unsupported`.

### `console_file`

JSON array of console/runtime events:

```json
[
  {
    "timestamp_ms": 123,
    "level": "warning",
    "type": "log",
    "text": "deprecated API",
    "url": "https://example.com/app.js",
    "line": 10,
    "column": 5
  }
]
```

### `observation_file`

Agent-readable page snapshot. It is intentionally smaller and more action-oriented than `rendered.html`, but it remains a mechanical projection.

Nodes include native interactive elements, explicit interaction markers
(`role`, `tabindex`, `contenteditable=true`), iframes, and a bounded set of
non-semantic elements whose computed `cursor` is `pointer`.
Observation traverses open shadow roots and same-origin iframes. Cross-origin
iframes are represented only by their iframe node plus `frame_ref`/`frames[]`
metadata because their contents are not readable from the embedding page.

Top-level fields:

| Field | Description |
| --- | --- |
| `schema_version` | Observation schema version. |
| `url` | Page URL at capture time. |
| `title` | Document title. |
| `viewport` | Width, height, device scale factor. |
| `frames` | Frame list with ids and urls. |
| `nodes` | Interactive and meaningful visible accessibility/DOM nodes. |
| `forms` | Mechanical form ownership and control refs. |
| `focused_ref` | `ref` of focused node, if any. |
| `truncated` | Present when the global node or scan cap stopped traversal. |

Node fields:

| Field | Description |
| --- | --- |
| `ref` | Snapshot-scoped opaque id. Not durable across observations. |
| `frame_id` | Owning frame id. |
| `role` | Accessibility role or mechanical DOM role. |
| `name` | Accessible name when available. |
| `text` | Visible text snippet when available. |
| `visible` | Whether the node is visible. |
| `enabled` | Whether interaction is enabled. |
| `bbox` | CSS-pixel bounding box. |
| `actions` | Mechanical possible actions such as `click`, `fill`, `select`, `check`, `focus`. |
| `href` / `src` | URL-bearing attributes when present. |
| `frame_ref` | On iframe nodes, the matching `frames[].frame_id` for the child frame entry. |
| `value_redacted` | True when an input has a value that was intentionally not emitted. |
| `selector_hint` | Optional best-effort selector hint for CDP resolution in the node's context; iframe nodes use `frame_ref`, iframe children use frame-relative selectors, and shadow nodes use `host >> shadow >> inner` chains. |
| `selector_hint_unique` | Present when `selector_hint` is present; true when it matches exactly one element in that node's actual document/shadow context. |

Traversal caps are global across the main document, open shadow roots, and
same-origin iframe documents. When `truncated` is present it reports the
mechanical reason and the node/scan limits; no truncation is silent.

Forbidden fields: intent labels, importance scores, page-type guesses, recommended actions, or captcha/paywall/login classification.

### `network_file`

Deep network artifact. Top-level shape:

```json
{
  "schema_version": 1,
  "main_request_id": "req-1",
  "entries": [],
  "summary": {
    "requests_total": 12,
    "responses_total": 11,
    "finished_total": 10,
    "failed_total": 1,
    "incomplete_total": 1,
    "inflight_total_at_capture": 1,
    "pending_by_resource_type": {"XHR": 1},
    "captured_body_files": 2,
    "redacted": true
  }
}
```

Each `entries[]` item may include:

| Field | Description |
| --- | --- |
| `request_id` | Stable request id from the browser backend. |
| `state` | Mechanical lifecycle state: `pending`, `responded`, `finished`, or `failed`. |
| `redirect_from_request_id` | Prior request id for redirect chains. |
| `frame_id` / `loader_id` | CDP frame/loader ids when known. |
| `resource_type` | `Document`, `XHR`, `Fetch`, `Script`, `Stylesheet`, `Image`, etc. |
| `initiator` | CDP initiator type and stack when available. |
| `request` | Method, URL, headers, post-data metadata, and timing. |
| `response` | Status, status text, URL, headers, mime type, protocol, remote address, sizes, and timing. |
| `cache` | Cache/service-worker flags. |
| `failure` | Failure text and cancellation status when the resource failed. |
| `body_file` | Optional captured response body path under `network-bodies/`. |
| `body_base64_file` | Optional captured binary body path when bytes are not UTF-8. |
| `payload_hints` | Mechanical hints such as `json_valid`, `json_top_level_type`, `graphql_operation_name`, `graphql_operation_type`. |

Sensitive request/response headers are redacted by default in `network.json`: `cookie`, `authorization`, `proxy-authorization`, `set-cookie`, and token/secret-like header names. `--network-redact off` disables this for trusted local debugging and may write raw tokens, cookies, and PII into `network.json`; `trace.sensitive_capture` records that opt-in.

Network body capture modes:

| Mode | Behavior |
| --- | --- |
| `off` | Metadata only. |
| `xhr` | Capture text/JSON XHR/fetch bodies up to the configured per-body limit. |
| `all` | Attempt every exposed response body up to the configured per-body limit. |

Body capture failures become warnings, not fetch failures. `--capture-ws` and `--capture-sse` write WebSocket/SSE payloads to frame/event files and may expose bearer tokens, session identifiers, chat content, or other PII.

When `--wait auto` is used, XHR/fetch/EventSource body capture is enabled by
default so the network artifact can explain pages whose visible DOM is only a
shell. Pending requests are not hidden: `network.summary.incomplete_total`,
`network.summary.inflight_total_at_capture`, and
`network.summary.pending_by_resource_type` stay non-zero, and entries remain in
`state: "pending"` or `state: "responded"`.

## CDP Result

`afhttp cdp` returns the CDP method result without adding semantic wrappers:

```json
{"result":{"type":"number","value":42}}
```

CDP method errors return the standard error envelope with `error_code: "cdp_error"` or `error_code: "cdp_timeout"`.

## Health Result

Authenticated `/health` and `afhttp health` return:

| Field | Description |
| --- | --- |
| `code` | Always `health`. |
| `status` | `ok`, `starting`, or `degraded`. |
| `version` | afhttp version. |
| `uptime_s` | Host uptime in seconds. |
| `backend` | Browser family/version/connected status. |
| `backend_error` | Structured backend/CDP error summary when `status` is `degraded`. |
| `profile` | Current profile kind/name/lock summary. |
| `tabs_active` | Current browser page target count from `Target.getTargets`. |
| `capabilities_url` | Relative URL for capabilities. |

Unauthenticated public health, when enabled, returns only `status`.

## Capabilities Result

`/capabilities` and `afhttp capabilities` return:

| Field | Description |
| --- | --- |
| `code` | Always `capabilities`. |
| `backend` | Browser family/version. |
| `artifacts` | Per-artifact `supported` booleans and notes. |
| `wait_modes` | Supported wait modes. |
| `display_takeover` | Whether the backend can expose real-display takeover when the host is launched with `--takeover display --display-provider kasmvnc` (`true` for Chromium-family and camoufox, `false` for lightpanda). |
| `ops_panel` | Screencast/input support plus provider-neutral display fields (`display_url`, `display_provider`) when display takeover is enabled. |
| `profile` | Persistent/ephemeral support. |
| `features` | Implemented feature support such as `selector_visible`, `network_body_capture`, `capture_ws`, `capture_sse`, `display_takeover`, `ops_panel`, `recent_requests`, and `profile_persistence`; risky captures include a `risk` string. |
| `limits` | Defaults and hard limits relevant to fetch planning. |

Capabilities describe support; they do not guarantee a later page-specific artifact capture will succeed.

## External Runtime Dependencies

The distributed afhttp binary does not bundle browser engines or KasmVNC. It locates external tools on `PATH` (or explicit flags where available) and spawns them as separate processes:

| Dependency | Used by | Notes |
| --- | --- | --- |
| Chromium/Chrome/Edge/Brave/fingerprint-chromium | Browser-backed fetch, screenshots, default ops panel | Set `--browser-bin` to override discovery for the primary browser binary. |
| lightpanda | `--browser lightpanda` | Rendering subset; no display takeover. |
| foxbridge + camoufox | `--browser camoufox` | `--browser-bin` may point at foxbridge; camoufox is discovered separately on `PATH`. |
| KasmVNC `Xvnc` | `--takeover display --display-provider kasmvnc` | GPLv2 external process only. Install it in the container and ensure `Xvnc` plus the KasmVNC web root are present; optional env overrides are `AFHTTP_KASMVNC_BIN` and `AFHTTP_KASMVNC_WEB_ROOT`. |
| matchbox-window-manager (or openbox) | KasmVNC display provider | Optional. Keeps the headful browser maximized so the client can resize the framebuffer to the operator's window (`resize=remote`); absent, the panel falls back to scaled rendering. Discovered on `PATH`. |

## Profile Results

Profile lifecycle commands are local-only.

### `profile list`

```json
{
  "code": "profile_list",
  "profile_root": "/Users/me/.local/share/afhttp/profiles",
  "profiles": [
    {
      "name": "work",
      "path": "/Users/me/.local/share/afhttp/profiles/work",
      "locked": true,
      "metadata_present": true,
      "last_used_at_rfc3339": "2026-05-27T01:23:45Z",
      "size_bytes": 123456789
    }
  ]
}
```

### `profile info`

Returns one profile object with metadata, lock owner when known, approximate size, path, and browser-family hints.

### `profile lock-status`

Returns `locked`, and when available `owner_pid`, `owner_started_at_rfc3339`, and `owner_command`.

### `profile downloads`

Read-only listing of files the browser captured in the profile download directory. Completed files report `state:"completed"`; Chromium partial files ending in `.crdownload` report `state:"in_progress"`.

```json
{
  "code": "profile_downloads",
  "name": "work",
  "download_dir": "/Users/me/.local/share/afhttp/profiles/work/downloads",
  "downloads": [
    {
      "filename": "report.csv",
      "path": "/Users/me/.local/share/afhttp/profiles/work/downloads/report.csv",
      "size_bytes": 12345,
      "state": "completed"
    }
  ]
}
```

### `profile delete` / `profile prune`

`profile delete` returns the deleted profile name. `profile prune` returns the resolved `profile_root`, `dry_run`, and the matching profile entries. Locked profiles are skipped; missing profiles return `profile_not_found`.

## Error Codes

Agents should branch on `error_code`, not the human-readable `error`.
Examples below are representative `error` / Chromium `errorText` strings.

| Code | Example detail | Agent should |
| --- | --- | --- |
| `navigation_timeout` | `Wait::Load: readyState never became complete` | Retry with a longer timeout or weaker wait condition; preserve artifacts already written. |
| `wait_selector_unmatched` | `selector "#ready" did not appear before --timeout-ms` | Distinguish from `navigation_timeout`: page itself loaded fine, only the CSS selector never matched. Verify the selector against the captured `observation.json` rather than blind-retrying. |
| `render_unavailable` | `browser fetch requires --endpoint-url pointing at an afhttp host` | Start/connect a browser host or use `--render none` if HTTP-only is enough. |
| `host_unreachable` | `CDP connect ws://127.0.0.1:9222/cdp: connection refused` | Check the afhttp host endpoint/token and retry after the host is reachable. |
| `dns_resolution_failed` | `net::ERR_NAME_NOT_RESOLVED` | Check spelling/DNS/network; retry later only if DNS may recover. |
| `target_unreachable` | `net::ERR_CONNECTION_REFUSED` | Check target service/firewall/port; retry when the target is reachable. |
| `tls_error` | `net::ERR_CERT_AUTHORITY_INVALID` | Do not blind-retry; fix trust/certificate settings or choose an HTTP-safe route. |
| `tab_crashed` | `Target.detachedFromTarget: target crashed` | Reopen the tab and retry the operation. |
| `profile_locked` | `profile "work" is already locked by pid 1234` | Reuse that host/profile or wait for the lock holder to exit. |
| `browser_launch_failed` | `chromium exited before DevTools endpoint appeared` | Inspect browser path/dependencies/display and retry after fixing launch. |
| `cdp_unavailable` | `wait_event: events channel closed` | Verify the endpoint speaks afhttp CDP and reconnect. |
| `cdp_error` | `CDP error -32000: No target with given id found` | Fix the method/params/tab id; retry only if the target may reappear. |
| `cdp_timeout` | `wait_event: timed out` | Retry with a longer timeout or different event/wait strategy. |
| `backend_unsupported` | `Page.captureScreenshot not supported by backend` | Drop that artifact/action or switch to a backend that supports it. |
| `artifact_capture_failed` | `DOM.getOuterHTML: missing outerHTML` | Use other artifacts if sufficient; retry if the page/backend state changed. |
| `artifact_capture_timeout` | `fetch timed out after 30000ms during capture_screenshot` | Use the partial result or retry with a larger `--timeout-ms` / smaller artifact set. |
| `artifact_empty` | `text artifact was empty after trimming whitespace` | Check `trace` readiness fields and `network.json`; the page may still be a shell. |
| `artifact_tiny` | `text artifact was 12 bytes; threshold is 32` | Inspect network bodies or retry with a larger timeout if the page is still rendering. |
| `network_not_idle` | `1 request(s) were still pending/responded at capture` | Inspect pending entries and captured XHR bodies before deciding whether to retry. |
| `pending_xhr_at_capture` | `pending XHR/fetch/EventSource at capture` | Treat `status: 200` as incomplete acquisition until network payloads are checked. |
| `observation_empty` | `observation contained zero projected nodes` | Fall back to rendered/text/network artifacts or retry after readiness improves. |
| `readiness_timeout` | `--wait auto captured before all readiness signals settled` | Partial capture succeeded; inspect warnings/trace/network before bounded retry. |
| `network_body_truncated` | `body for 1234.1 truncated to 10485760 bytes` | Increase `--network-body-max-bytes` if the omitted suffix matters. |
| `profile_not_found` | `profile "work" does not exist` | Create/select an existing profile name. |
| `profile_delete_locked` | `profile "work" is locked; refusing delete` | Stop the owning host before delete/prune. |
| `profile_invalid_name` | `profile name "../work" is invalid` | Use a simple non-hidden profile name without path separators. |
| `profile_root_unavailable` | `profile root cannot be created: permission denied` | Fix filesystem permissions/path or choose another profile root. |
| `invalid_argument` | `--render: unknown mode "sometimes"` | Correct the CLI/SDK argument before retrying. |
| `invalid_endpoint` | `endpoint must start with ws://, http://, or unix:` | Correct endpoint syntax. |
| `io_error` | `write /out/body.html: permission denied` | Fix local filesystem permissions/space/path and retry. |
| `internal_error` | `serialize observation: ...` | Treat as a bug; capture logs and file an issue if reproducible. |
