<!-- Generated. Do not edit by hand. -->

# afhttp CLI Reference

> Regenerate with `afhttp --help-markdown`.
> See [reference.md](reference.md) for field-level response details.

# Command-Line Help for `afhttp`

This document contains the help content for the `afhttp` command-line program.

**Command Overview:**

* [`afhttp`↴](#afhttp)
* [`afhttp host`↴](#afhttp-host)
* [`afhttp fetch`↴](#afhttp-fetch)
* [`afhttp upload`↴](#afhttp-upload)
* [`afhttp cdp`↴](#afhttp-cdp)
* [`afhttp ui`↴](#afhttp-ui)
* [`afhttp health`↴](#afhttp-health)
* [`afhttp capabilities`↴](#afhttp-capabilities)
* [`afhttp profile`↴](#afhttp-profile)
* [`afhttp profile list`↴](#afhttp-profile-list)
* [`afhttp profile info`↴](#afhttp-profile-info)
* [`afhttp profile lock-status`↴](#afhttp-profile-lock-status)
* [`afhttp profile downloads`↴](#afhttp-profile-downloads)
* [`afhttp profile delete`↴](#afhttp-profile-delete)
* [`afhttp profile prune`↴](#afhttp-profile-prune)
* [`afhttp profile cookies`↴](#afhttp-profile-cookies)
* [`afhttp tabs`↴](#afhttp-tabs)
* [`afhttp tabs list`↴](#afhttp-tabs-list)
* [`afhttp tabs close`↴](#afhttp-tabs-close)

## `afhttp`

A URL acquisition tool for AI agents.

Give afhttp a URL and it returns the page plus the artifacts an agent needs to
decide what to do next: rendered HTML, a DOM observation, a screenshot, and
network and console logs. It covers the whole acquisition range behind one
structured contract — a plain HTTP fetch when that works, a browser-backed
fetch when it does not, deep network capture, a raw CDP escape hatch, and an
ops panel for human takeover (login, captcha, 2FA).

Two roles. `afhttp host` is the long-lived browser-host: it holds Chromium and
one on-disk profile, and exposes a CDP endpoint plus the ops panel. The other
commands are short-lived drivers that connect to a host, do work, and write
artifacts locally. Run the host where the browser needs to be and the driver
wherever the agent runs.

Every output is one line of structured JSON; every failure carries a stable
error_code. The tool never decides what a page means — the agent does.

**Usage:** `afhttp <COMMAND>`

###### **Subcommands:**

* `host` — Run the browser host
* `fetch` — Fetch a URL
* `upload` — Upload a local file to a browser tab via DOM.setFileInputFiles
* `cdp` — Send a raw CDP method
* `ui` — Print or open the ops panel URL
* `health` — Query /health
* `capabilities` — Query /capabilities
* `profile` — Local profile lifecycle commands
* `tabs` — List and close CDP targets attached to the host



## `afhttp host`

Run the browser host

**Usage:** `afhttp host [OPTIONS] --listen <LISTEN>`

###### **Options:**

* `--listen <LISTEN>` — Listener address: `tcp:host:port` or `unix:/path/to.sock`
* `--profile <PROFILE>` — Profile name under $XDG_DATA_HOME/afhttp/profiles, or `-` for an ephemeral profile. One host binds exactly one profile

  Default value: `-`
* `--display <DISPLAY>` — headless or headful. Omit when --takeover=kasmvnc should imply headful
* `--takeover <TAKEOVER>` — Human takeover mode (like --render, pick one): none serves no takeover panel; screencast serves the CDP screencast panel at /ops (works headless, no VNC/X needed); kasmvnc serves a real KasmVNC display at /ops/display for hard sites (captcha, IME, flaky CDP input — implies headful)

  Default value: `screencast`
* `--display-quality-percent <DISPLAY_QUALITY>` — Display-takeover image quality, 0-100 (default 100 = crispest). Maps to KasmVNC's 0-9 quality tiers; lower trades clarity for bandwidth. Only applies with `--takeover kasmvnc`. Adjustable live in the panel too

  Default value: `100`
* `--browser <BROWSER>` — auto | chromium | chrome | chrome_shell | fingerprint_chromium | edge | brave | lightpanda | camoufox

  Default value: `auto`
* `--browser-bin <BROWSER_BIN>` — Override browser binary path
* `--token-secret <TOKEN>` — Bearer token required for clients on TCP listeners
* `--health <HEALTH>` — Serve /health and /capabilities

  Default value: `on`
* `--health-public <HEALTH_PUBLIC>` — Make /health public with minimal payload

  Default value: `off`
* `--engine-env <K=V>` — Propagate an environment variable into the browser subprocess. Repeatable. The host scrubs all other ambient env (`HTTP_PROXY`, `XDG_*`, `BROWSER`, locale, etc.) so a browsing environment can never silently honor configuration the agent did not request. Use the form `K=V`
* `--browser-arg <FLAG>` — Append a raw flag to the backend subprocess command line. Repeatable. Use for backend-specific surfaces the host doesn't model first-class — for example `--browser-arg --fingerprint-brand=Chrome` to override fingerprint-chromium's brand string. Chromium honors last-wins for duplicate flags, so an explicit entry overrides any default the host applied
* `--proxy-url <PROXY>` — Explicit upstream proxy URL. The host never inherits `HTTP_PROXY`/`HTTPS_PROXY` from the environment — this is the only way to route browser traffic. Example: `http://user:pass@proxy.local:8080` or `socks5://10.0.0.5:1080`
* `--recent-requests-cap <RECENT_REQUESTS_CAP>` — Enable /recent-requests with a bounded ring of N entries. 0 = off

  Default value: `0`



## `afhttp fetch`

Fetch a URL

**Usage:** `afhttp fetch [OPTIONS] <URL>`

###### **Arguments:**

* `<URL>` — URL to fetch

###### **Options:**

* `--endpoint-url <ENDPOINT>` — CDP endpoint of a running host. Omit to spawn an inline ephemeral host for this one fetch
* `--token-secret <TOKEN>` — Bearer token, if the host was started with `--token-secret`
* `--render <RENDER>` — Render strategy: none (HTTP fast path, no browser), auto (HTTP first, escalate to the browser on failure), or always (browser only)

  Default value: `auto`
* `--tab <new|<id>>` — Tab target to use. "new" allocates a temporary target and closes it after fetch; an id reuses that target and leaves it open

  Default value: `new`
* `--wait <WAIT>` — Readiness signal before capture on the browser path: load | idle | selector:<css> | selector-visible:<css> | ms:<n>

  Default value: `load`
* `--header <K:V>` — Add a request header (repeatable). Format: `Name: value`
* `--cookie <name=value>` — Add a request cookie (repeatable). Format: `name=value`
* `--user-agent <USER_AGENT>` — Override the User-Agent header for this fetch
* `--evaluate-after-wait <js>` — JavaScript to evaluate after the wait condition resolves (repeatable). Runs in page context before artifacts are captured
* `--want <WANT>` — Artifacts to capture, comma-separated. Omit for all of: body, rendered_html, text, screenshot, network, console, observation (storage is opt-in only)
* `--method <METHOD>` — HTTP method. Common values: POST, PUT, PATCH, DELETE

  Default value: `GET`
* `--data <DATA>` — Request body as a string. Prefix with `@` to read from a file path (e.g. `--data @payload.json`). Mutually exclusive with `--form`
* `--data-file <DATA_FILE>` — Request body from a file path. Mutually exclusive with `--form`
* `--form <key=value>` — Add a form field (repeatable). Sends body as `application/x-www-form-urlencoded`. Mutually exclusive with `--data`. Format: `key=value`
* `--network-bodies <NETWORK_BODIES>` — Capture response bodies for network requests: off, xhr (XHR/fetch only), or all

  Default value: `off`

  Possible values: `off`, `xhr`, `all`

* `--network-body-max-bytes <NETWORK_BODY_MAX_BYTES>` — Per-body cap for captured network bodies, in bytes

  Default value: `1048576`
* `--network-redact <NETWORK_REDACT>` — Redact sensitive values in network.json: on or off. On by default; off writes raw Authorization/Cookie headers and token-bearing query params to the artifact — only disable for trusted local debugging

  Default value: `on`

  Possible values: `on`, `off`

* `--out <OUT>` — Directory to write artifacts into. Defaults to an `afhttp-out` subdirectory of the working directory
* `--cookie-jar <COOKIE_JAR>` — Override the cookie-jar path. The default — derived from the host's `GET /profile` — places the jar at `<profile-dir>/cookies.jar.json`. This override is rejected with `invalid_argument` if it does not match the host's profile path; the flag exists for tests and forensic tooling, not production sessions. Honors `AFHTTP_COOKIE_JAR` when omitted (same validation applies)
* `--no-cookie-jar` — Opt out of cookie-jar persistence for this fetch. No cookies are replayed from the jar and no `Set-Cookie` responses are merged back
* `--observe-main-wait-ms <OBSERVE_MAIN_WAIT_MS>` — Upper bound on the browser-path wait for the main document network event, in milliseconds. Raise for slow networks or low-end machines

  Default value: `500`
* `--max-response-bytes <MAX_RESPONSE_BYTES>` — Upper bound on the HTTP-path response body, in bytes. Default 1 GiB (`1073741824`). `0` disables the cap entirely. When the cap is hit, the fetch returns successfully with a `network_body_truncated` warning and the prefix bytes that were collected

  Default value: `1073741824`
* `--retry <RETRY>` — Number of additional attempts after the first. Retries fire only when the error has `retryable: true` (e.g. `host_unreachable`, `cdp_timeout`); non-retryable failures (`tls_error`, `wait_selector_unmatched`, etc.) short-circuit. Default 0 = single attempt

  Default value: `0`
* `--backoff-ms <BACKOFF_MS>` — Fixed delay between retries, in milliseconds

  Default value: `250`
* `--proxy-url <PROXY>` — Per-fetch upstream HTTP/HTTPS proxy for the HTTP fast path. The SDK never honors `HTTP_PROXY` from the environment; this flag is the only way to route an HTTP-path fetch through one. Format: `http://user:pass@host:port` or `socks5://host:port`
* `--ca-cert <CA_CERT>` — Path to a PEM file containing extra root CAs to trust for this fetch's HTTP path. Useful for self-signed staging or corporate MITM CAs
* `--tls-insecure` — Disable TLS certificate verification for this fetch's HTTP path. Dangerous; leaves the connection open to MITM. Use only against known-self-signed environments
* `--timeout <TIMEOUT>` — Overall fetch timeout (e.g. `30s`, `1500ms`)

  Default value: `30s`
* `--capture-ws` — Capture WebSocket frame payloads to network-bodies/<id>.frames.jsonl. Frames may carry bearer tokens, session IDs, and message content — treat the artifact as sensitive
* `--capture-sse` — Capture SSE event payloads to network-bodies/<id>.frames.jsonl. Events may carry PII; treat the artifact as sensitive



## `afhttp upload`

Upload a local file to a browser tab via DOM.setFileInputFiles

**Usage:** `afhttp upload [OPTIONS] --endpoint-url <ENDPOINT> --tab <TAB> --selector <SELECTOR> --file <FILE>`

###### **Options:**

* `--endpoint-url <ENDPOINT>` — CDP endpoint of the running host
* `--token-secret <TOKEN>` — Bearer token, if the host was started with `--token-secret`
* `--tab <TAB>` — Tab ID to operate in
* `--selector <SELECTOR>` — CSS selector for the `<input type=file>` element
* `--file <FILE>` — Local file path to upload



## `afhttp cdp`

Send a raw CDP method

**Usage:** `afhttp cdp [OPTIONS] --endpoint-url <ENDPOINT> --tab <TAB> <METHOD>`

###### **Arguments:**

* `<METHOD>` — CDP method name (e.g. Runtime.evaluate)

###### **Options:**

* `--endpoint-url <ENDPOINT>` — CDP endpoint of the running host
* `--token-secret <TOKEN>` — Bearer token, if the host was started with `--token-secret`
* `--tab <TAB>` — CDP target id to drive
* `--params <PARAMS>` — JSON literal, or `@-` to read from stdin
* `--wait <WAIT>` — "<event>:<timeout>" — wait for a CDP event before exiting



## `afhttp ui`

Print or open the ops panel URL

**Usage:** `afhttp ui [OPTIONS] --endpoint-url <ENDPOINT>`

###### **Options:**

* `--endpoint-url <ENDPOINT>` — CDP endpoint of the running host
* `--token-secret <TOKEN>` — Bearer token, if the host requires one (appended to the panel URLs)



## `afhttp health`

Query /health

**Usage:** `afhttp health [OPTIONS] --endpoint-url <ENDPOINT>`

###### **Options:**

* `--endpoint-url <ENDPOINT>` — CDP endpoint of the running host
* `--token-secret <TOKEN>` — Bearer token, if the host was started with `--token-secret`



## `afhttp capabilities`

Query /capabilities

**Usage:** `afhttp capabilities [OPTIONS] --endpoint-url <ENDPOINT>`

###### **Options:**

* `--endpoint-url <ENDPOINT>` — CDP endpoint of the running host
* `--token-secret <TOKEN>` — Bearer token, if the host was started with `--token-secret`



## `afhttp profile`

Local profile lifecycle commands

**Usage:** `afhttp profile <COMMAND>`

###### **Subcommands:**

* `list` — List on-disk profiles under the profiles root
* `info` — Show metadata for one profile (size, last use, lock state)
* `lock-status` — Report whether a profile is currently locked by a running host
* `downloads` — List files captured in the profile's browser download directory
* `delete` — Delete a profile and all of its on-disk state
* `prune` — Delete profiles whose last use is older than a cutoff
* `cookies` — Show the non-expired cookies in a profile's jar (values redacted)



## `afhttp profile list`

List on-disk profiles under the profiles root

**Usage:** `afhttp profile list [OPTIONS]`

###### **Options:**

* `--profile-root <PROFILE_ROOT>` — Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles`



## `afhttp profile info`

Show metadata for one profile (size, last use, lock state)

**Usage:** `afhttp profile info [OPTIONS] <NAME>`

###### **Arguments:**

* `<NAME>` — Profile name

###### **Options:**

* `--profile-root <PROFILE_ROOT>` — Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles`



## `afhttp profile lock-status`

Report whether a profile is currently locked by a running host

**Usage:** `afhttp profile lock-status [OPTIONS] <NAME>`

###### **Arguments:**

* `<NAME>` — Profile name

###### **Options:**

* `--profile-root <PROFILE_ROOT>` — Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles`



## `afhttp profile downloads`

List files captured in the profile's browser download directory

**Usage:** `afhttp profile downloads [OPTIONS] <NAME>`

###### **Arguments:**

* `<NAME>` — Profile name

###### **Options:**

* `--profile-root <PROFILE_ROOT>` — Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles`



## `afhttp profile delete`

Delete a profile and all of its on-disk state

**Usage:** `afhttp profile delete [OPTIONS] --confirm <CONFIRM> <NAME>`

###### **Arguments:**

* `<NAME>` — Profile name to delete

###### **Options:**

* `--confirm <CONFIRM>` — Confirmation guard: must equal the profile name for the delete to proceed
* `--profile-root <PROFILE_ROOT>` — Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles`



## `afhttp profile prune`

Delete profiles whose last use is older than a cutoff

**Usage:** `afhttp profile prune [OPTIONS] --older-than <OLDER_THAN>`

###### **Options:**

* `--older-than <OLDER_THAN>` — Age cutoff (e.g. `30d`, `12h`); profiles last used before this are removed
* `--dry-run` — Report what would be deleted without deleting anything

  Default value: `false`
* `--profile-root <PROFILE_ROOT>` — Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles`



## `afhttp profile cookies`

Show the non-expired cookies in a profile's jar (values redacted)

**Usage:** `afhttp profile cookies [OPTIONS] <NAME>`

###### **Arguments:**

* `<NAME>` — Profile name

###### **Options:**

* `--profile-root <PROFILE_ROOT>` — Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles`



## `afhttp tabs`

List and close CDP targets attached to the host

**Usage:** `afhttp tabs <COMMAND>`

###### **Subcommands:**

* `list` — List currently-attached CDP targets
* `close` — Close a target by its CDP target id



## `afhttp tabs list`

List currently-attached CDP targets

**Usage:** `afhttp tabs list [OPTIONS] --endpoint-url <ENDPOINT>`

###### **Options:**

* `--endpoint-url <ENDPOINT>` — CDP endpoint URL (e.g. `ws://127.0.0.1:9222`)
* `--token-secret <TOKEN>` — Bearer token, if the host was started with `--token-secret`



## `afhttp tabs close`

Close a target by its CDP target id

**Usage:** `afhttp tabs close [OPTIONS] --endpoint-url <ENDPOINT> <ID>`

###### **Arguments:**

* `<ID>` — CDP target id to close (e.g. `41A0F1E0FD…`)

###### **Options:**

* `--endpoint-url <ENDPOINT>` — CDP endpoint URL (e.g. `ws://127.0.0.1:9222`)
* `--token-secret <TOKEN>` — Bearer token, if the host was started with `--token-secret`
