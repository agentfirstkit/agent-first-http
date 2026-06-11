<!-- Generated. Do not edit by hand. -->

# afhttp CLI Reference

> Regenerate with `afhttp --help --recursive --output markdown`.
> See [reference.md](reference.md) for field-level response details.

# afhttp - A URL acquisition tool for AI agents.

A URL acquisition tool for AI agents.

Give afhttp a URL and it returns the page plus the artifacts an agent needs to
decide what to do next: rendered HTML, a DOM observation, a screenshot, and
network and console logs. It covers the whole acquisition range behind one
structured contract — a plain HTTP fetch when that works, a browser-backed
fetch when it does not, deep network capture, a raw CDP escape hatch, and a
takeover panel for human takeover (login, captcha, 2FA).

Two roles. `afhttp host` is the long-lived browser-host: it holds Chromium and
one on-disk profile, and exposes a CDP endpoint plus the takeover panel. The other
commands are short-lived drivers that connect to a host, do work, and write
artifacts locally. Run the host where the browser needs to be and the driver
wherever the agent runs.

Every output is one line of structured JSON; every failure carries a stable
error_code. The tool never decides what a page means — the agent does.

```text
A URL acquisition tool for AI agents.

Give afhttp a URL and it returns the page plus the artifacts an agent needs to
decide what to do next: rendered HTML, a DOM observation, a screenshot, and
network and console logs. It covers the whole acquisition range behind one
structured contract — a plain HTTP fetch when that works, a browser-backed
fetch when it does not, deep network capture, a raw CDP escape hatch, and a
takeover panel for human takeover (login, captcha, 2FA).

Two roles. `afhttp host` is the long-lived browser-host: it holds Chromium and
one on-disk profile, and exposes a CDP endpoint plus the takeover panel. The other
commands are short-lived drivers that connect to a host, do work, and write
artifacts locally. Run the host where the browser needs to be and the driver
wherever the agent runs.

Every output is one line of structured JSON; every failure carries a stable
error_code. The tool never decides what a page means — the agent does.

Usage: afhttp <COMMAND>

Commands:
  fetch         Fetch a URL
  host          Run the browser host
  upload        Upload a local file to a browser tab via DOM.setFileInputFiles
  cdp           Send a raw CDP method
  panel         Print a short-lived takeover URL
  health        Query /health
  capabilities  Query /capabilities
  profile       Local profile lifecycle commands
  tabs          List and close CDP targets attached to the host
  skill         Install, remove, or check the embedded Agent Skill (Codex, Claude Code, opencode)
  container     Build and run the host container (Docker or Apple) from the embedded recipe
  help          Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help. Add --recursive to expand every nested subcommand; add --output json|yaml|markdown to render this help in another format.

  -V, --version
          Print version
```

## afhttp fetch - Fetch a URL

```text
Fetch a URL

Usage: fetch [OPTIONS] <URL>

Arguments:
  <URL>
          URL to fetch

Options:
  -h, --help
          Print help (see a summary with '-h')

Connection:
      --endpoint-url <ENDPOINT>
          CDP endpoint of a running host. Omit to spawn an inline ephemeral host for ordinary browser fetches; with --takeover, omission discovers the standard local `afhttp-host`. Falls back to `AFHTTP_ENDPOINT_URL`

          [env: AFHTTP_ENDPOINT_URL=]

      --token-secret <TOKEN>
          Bearer token, if the host was started with `--token-secret`. Falls back to `AFHTTP_TOKEN_SECRET`

          [env: AFHTTP_TOKEN_SECRET=]

Inline host:
      --browser <BROWSER>
          Browser backend for the inline host. Ignored when --endpoint-url is set (the host owns its browser)

          [default: auto]
          [possible values: auto, chromium, chrome, chrome-headless-shell, fingerprint-chromium, edge, brave, lightpanda, camoufox]

      --browser-bin <PATH>
          Browser binary path for the inline host, for when auto-discovery can't find one. Ignored when --endpoint-url is set

Rendering:
      --render <RENDER>
          Render strategy: none (HTTP fast path, no browser), auto (HTTP first, escalate to the browser on failure), or always (browser only)

          Possible values:
          - none:   HTTP fast path, no browser
          - auto:   HTTP first, escalate to the browser on failure
          - always: Browser only

          [default: auto]

      --wait <WAIT>
          Readiness signal before capture on the browser path: auto | load | idle | selector:<css> | selector-visible:<css> | ms:<n>

          [default: auto]

      --evaluate-after-wait <JS>
          JavaScript to evaluate after the wait condition resolves (repeatable). Runs in page context before artifacts are captured

      --want <WANT>
          Artifacts to capture, comma-separated. Default: body, rendered_html, text, content, content_json, screenshot, network, console, observation. `content` is the agent-oriented composed page view (content.md); `content_json` its structured form with link/action candidates. `storage` is opt-in (sensitive: localStorage/IndexedDB)

Session:
      --tab <new|<id>>
          Tab target: "new" allocates a temporary target and closes it after fetch; a CDP target id reuses that target and leaves it open (the same id `afhttp cdp`/`upload`/`tabs` accept)

          [default: new]

      --takeover
          Escalate to human takeover when a wall (captcha/login/2FA) is hit: keep a persistent tab open and return its short-lived takeover URL in `next_action`, plus a re-fetch command for the same tab once the human clears the wall. Uses `--endpoint-url` / `AFHTTP_ENDPOINT_URL` when set; otherwise discovers the standard local `afhttp-host` container (build one with `afhttp container install`)

      --profile <PROFILE>
          Host profile to use for this fetch. Switches the host's active profile if it differs (per-domain isolation), relaunching its browser. With `--takeover` and no `--profile`, the profile defaults to the URL's registrable domain (eTLD+1). Requires a host via `--endpoint-url`, or the standard local takeover host discovered by `--takeover`

Request:
      --header <NAME:VALUE>
          Add a request header (repeatable). Format: `Name:value` (a space after the colon is allowed)

      --cookie <NAME=VALUE>
          Add a request cookie (repeatable). Format: `name=value`

      --user-agent <USER_AGENT>
          Override the User-Agent header for this fetch

      --method <METHOD>
          HTTP method. Common values: POST, PUT, PATCH, DELETE

          [default: GET]

      --data <STRING|@FILE>
          Request body as a string. Prefix with `@` to read from a file path (e.g. `--data @payload.json`). Mutually exclusive with `--form`

      --form <NAME=VALUE>
          Add a form field (repeatable). Sends body as `application/x-www-form-urlencoded`. Mutually exclusive with `--data`. Format: `name=value`

Network capture:
      --network-bodies <NETWORK_BODIES>
          Capture response bodies for network requests: off, xhr (XHR/fetch only), or all

          [default: off]
          [possible values: off, xhr, all]

      --network-body-max-bytes <NETWORK_BODY_MAX_BYTES>
          Per-body cap for each captured network sub-request body, in bytes (see `--max-response-bytes` for the main HTTP-path response body)

          [default: 10485760]

      --no-network-redact
          Disable redaction of sensitive values in network.json (redacted by default). Writes raw Authorization/Cookie headers and token-bearing query params to the artifact — only for trusted local debugging

      --capture-ws
          Capture WebSocket frame payloads to network-bodies/<id>.frames.jsonl. Frames may carry bearer tokens, session IDs, and message content — treat the artifact as sensitive

      --capture-sse
          Capture SSE event payloads to network-bodies/<id>.frames.jsonl. Events may carry PII; treat the artifact as sensitive

Readiness tuning:
      --readiness-idle-ms <READINESS_IDLE_MS>
          Network quiet window used by --wait auto, in milliseconds

          [default: 800]

      --readiness-stable-ms <READINESS_STABLE_MS>
          DOM/text unchanged window used by --wait auto, in milliseconds

          [default: 500]

      --readiness-min-text-bytes <READINESS_MIN_TEXT_BYTES>
          Low visible-text byte threshold for --wait auto quality warnings only

          [default: 32]

      --observe-main-wait-ms <OBSERVE_MAIN_WAIT_MS>
          Upper bound on the browser-path wait for the main document network event, in milliseconds. Raise for slow networks or low-end machines

          [default: 500]

Output:
      --out <OUT>
          Directory to write artifacts into. Defaults to `afhttp-out` under the system temporary directory. Files persist there for inspection

Cookies:
      --cookie-jar <COOKIE_JAR>
          Override the cookie-jar path. The default — derived from the host's `GET /profile` — places the jar at `<profile-dir>/cookies.jar.json`. This override is rejected with `invalid_argument` if it does not match the host's profile path; the flag exists for tests and forensic tooling, not production sessions. Honors `AFHTTP_COOKIE_JAR` when omitted (same validation applies)

      --no-cookie-jar
          Opt out of cookie-jar persistence for this fetch. No cookies are replayed from the jar and no `Set-Cookie` responses are merged back

HTTP transport:
      --max-response-bytes <MAX_RESPONSE_BYTES>
          Upper bound on the main HTTP-path response body, in bytes (see `--network-body-max-bytes` for captured network sub-request bodies). Default 1 GiB (`1073741824`). `0` disables the cap entirely. When the cap is hit, the fetch returns successfully with a `network_body_truncated` warning and the prefix bytes that were collected

          [default: 1073741824]

      --proxy-url <PROXY>
          Per-fetch upstream HTTP/HTTPS proxy for the HTTP fast path. The SDK never honors `HTTP_PROXY` from the environment; this flag is the only way to route an HTTP-path fetch through one. Format: `http://user:pass@host:port` or `socks5://host:port`

      --ca-cert <CA_CERT>
          Path to a PEM file containing extra root CAs to trust for this fetch's HTTP path. Useful for self-signed staging or corporate MITM CAs

      --tls-insecure
          Disable TLS certificate verification for this fetch's HTTP path. Dangerous; leaves the connection open to MITM. Use only against known-self-signed environments

      --timeout-ms <TIMEOUT_MS>
          Overall fetch timeout, in milliseconds. Applies to both the HTTP fast path and the browser path

          [default: 30000]

Retry:
      --retry <RETRY>
          Number of additional attempts after the first. Retries fire only when the error has `retryable: true` (e.g. `host_unreachable`, `cdp_timeout`); non-retryable failures (`tls_error`, `wait_selector_unmatched`, etc.) short-circuit. Default 0 = single attempt

          [default: 0]

      --backoff-ms <BACKOFF_MS>
          Fixed delay between retries, in milliseconds

          [default: 250]
```

## afhttp host - Run the browser host

```text
Run the browser host

Usage: host [OPTIONS] --listen <LISTEN>

Options:
  -h, --help
          Print help

Listener:
      --listen <LISTEN>
          Listener address: `tcp:host:port` or `unix:/path/to.sock`

      --token-secret <TOKEN>
          Bearer token required for clients on TCP listeners

Profile:
      --profile <PROFILE>
          Initial logical profile name, or `-` for an ephemeral profile. Persistent profiles are stored under $XDG_DATA_HOME/afhttp/profiles/<backend>/<name>. A host serves one active profile at a time but can switch at runtime when a client passes `?profile=` on the `/cdp` connection (the browser is relaunched)

          [default: -]

Display & takeover:
      --display <DISPLAY>
          Display mode. Omit when `--takeover-provider` should imply headful

          [possible values: headless, headful]

      --takeover-provider <TAKEOVER_PROVIDER>
          Real-display takeover provider: `off` serves no takeover surface; a provider name (currently `kasmvnc`) serves a real-display takeover at /takeover/panel for hard sites (captcha, IME, flaky CDP input — implies headful)

          [default: off]
          [possible values: off, kasmvnc]

      --takeover-quality-percent <TAKEOVER_QUALITY_PERCENT>
          Takeover-provider image quality hint, 0-100 percent (default 100 = crispest). The KasmVNC provider maps this to 0-9 quality tiers; lower trades clarity for bandwidth. Adjustable live in the display panel too

          [default: 100]

Browser:
      --browser <BROWSER>
          Browser backend

          [default: auto]
          [possible values: auto, chromium, chrome, chrome-headless-shell, fingerprint-chromium, edge, brave, lightpanda, camoufox]

      --browser-bin <BROWSER_BIN>
          Override browser binary path

      --engine-env <NAME=VALUE>
          Propagate an environment variable into the browser subprocess. Repeatable. The host scrubs all other ambient env (`HTTP_PROXY`, `XDG_*`, `BROWSER`, locale, etc.) so a browsing environment can never silently honor configuration the agent did not request. Use the form `NAME=VALUE`

      --browser-arg <FLAG>
          Append a raw flag to the backend subprocess command line. Repeatable. Use for backend-specific surfaces the host doesn't model first-class — for example `--browser-arg --fingerprint-brand=Chrome` to override fingerprint-chromium's brand string. Chromium honors last-wins for duplicate flags, so an explicit entry overrides any default the host applied

      --proxy-url <PROXY>
          Explicit upstream proxy URL. The host never inherits `HTTP_PROXY`/`HTTPS_PROXY` from the environment — this is the only way to route browser traffic. Example: `http://user:pass@proxy.local:8080` or `socks5://10.0.0.5:1080`

Diagnostics:
      --no-health
          Disable serving /health and /capabilities (served by default)

      --health-public <HEALTH_PUBLIC>
          Make /health public with minimal payload

          [default: off]
          [possible values: off, minimal]

      --recent-requests-cap <RECENT_REQUESTS_CAP>
          Enable /recent-requests with a bounded ring of N entries. 0 = off

          [default: 0]
```

## afhttp upload - Upload a local file to a browser tab via DOM.setFileInputFiles

```text
Upload a local file to a browser tab via DOM.setFileInputFiles

Usage: upload [OPTIONS] --endpoint-url <ENDPOINT> --tab <TAB> --selector <SELECTOR> --file <FILE>

Options:
      --endpoint-url <ENDPOINT>
          CDP endpoint of the running host (e.g. `ws://127.0.0.1:9222`). Falls back to `AFHTTP_ENDPOINT_URL`

          [env: AFHTTP_ENDPOINT_URL=]

      --token-secret <TOKEN>
          Bearer token, if the host was started with `--token-secret`. Falls back to `AFHTTP_TOKEN_SECRET`

          [env: AFHTTP_TOKEN_SECRET=]

      --tab <TAB>
          CDP target id (tab) to operate in

      --selector <SELECTOR>
          CSS selector for the `<input type=file>` element

      --file <FILE>
          Local file path to upload

  -h, --help
          Print help
```

## afhttp cdp - Send a raw CDP method

```text
Send a raw CDP method

Usage: cdp [OPTIONS] --endpoint-url <ENDPOINT> --tab <TAB> <METHOD>

Arguments:
  <METHOD>
          CDP method name (e.g. Runtime.evaluate)

Options:
      --endpoint-url <ENDPOINT>
          CDP endpoint of the running host (e.g. `ws://127.0.0.1:9222`). Falls back to `AFHTTP_ENDPOINT_URL`

          [env: AFHTTP_ENDPOINT_URL=]

      --token-secret <TOKEN>
          Bearer token, if the host was started with `--token-secret`. Falls back to `AFHTTP_TOKEN_SECRET`

          [env: AFHTTP_TOKEN_SECRET=]

      --tab <TAB>
          CDP target id (tab) to drive

      --params <JSON|@->
          JSON literal, or `@-` to read from stdin

      --wait-event <WAIT>
          "<event>:<timeout>" — wait for a CDP event before exiting

  -h, --help
          Print help
```

## afhttp panel - Print a short-lived takeover URL

```text
Print a short-lived takeover URL

Usage: panel [OPTIONS] --endpoint-url <ENDPOINT>

Options:
      --endpoint-url <ENDPOINT>
          CDP endpoint of the running host (e.g. ws://127.0.0.1:9222). Falls back to `AFHTTP_ENDPOINT_URL`

          [env: AFHTTP_ENDPOINT_URL=]

      --token-secret <TOKEN>
          Bearer token, if the host requires one. Falls back to `AFHTTP_TOKEN_SECRET`

          [env: AFHTTP_TOKEN_SECRET=]

  -h, --help
          Print help
```

## afhttp health - Query /health

```text
Query /health

Usage: health [OPTIONS] --endpoint-url <ENDPOINT>

Options:
      --endpoint-url <ENDPOINT>
          CDP endpoint of the running host (e.g. `ws://127.0.0.1:9222`). Falls back to `AFHTTP_ENDPOINT_URL`

          [env: AFHTTP_ENDPOINT_URL=]

      --token-secret <TOKEN>
          Bearer token, if the host was started with `--token-secret`. Falls back to `AFHTTP_TOKEN_SECRET`

          [env: AFHTTP_TOKEN_SECRET=]

  -h, --help
          Print help
```

## afhttp capabilities - Query /capabilities

```text
Query /capabilities

Usage: capabilities [OPTIONS] --endpoint-url <ENDPOINT>

Options:
      --endpoint-url <ENDPOINT>
          CDP endpoint of the running host (e.g. `ws://127.0.0.1:9222`). Falls back to `AFHTTP_ENDPOINT_URL`

          [env: AFHTTP_ENDPOINT_URL=]

      --token-secret <TOKEN>
          Bearer token, if the host was started with `--token-secret`. Falls back to `AFHTTP_TOKEN_SECRET`

          [env: AFHTTP_TOKEN_SECRET=]

  -h, --help
          Print help
```

## afhttp profile - Local profile lifecycle commands

```text
Local profile lifecycle commands

Usage: profile <COMMAND>

Commands:
  list         List on-disk profiles under the profiles root
  info         Show metadata for one profile (size, last use, lock state)
  lock-status  Report whether a profile is currently locked by a running host
  downloads    List files captured in the profile's browser download directory
  delete       Delete a profile and all of its on-disk state
  prune        Delete profiles whose last use is older than a cutoff
  cookies      Show the non-expired cookies in a profile's jar (values redacted)
  help         Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

### afhttp profile list - List on-disk profiles under the profiles root

```text
List on-disk profiles under the profiles root

Usage: list [OPTIONS]

Options:
      --profile-root <PROFILE_ROOT>
          Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles`

  -h, --help
          Print help
```

### afhttp profile info - Show metadata for one profile (size, last use, lock state)

```text
Show metadata for one profile (size, last use, lock state)

Usage: info [OPTIONS] <NAME>

Arguments:
  <NAME>
          Profile name

Options:
      --backend <BACKEND>
          Profile backend scope (for example chromium, brave, camoufox). Required when the same profile name exists under multiple backends

      --profile-root <PROFILE_ROOT>
          Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles`

  -h, --help
          Print help
```

### afhttp profile lock-status - Report whether a profile is currently locked by a running host

```text
Report whether a profile is currently locked by a running host

Usage: lock-status [OPTIONS] <NAME>

Arguments:
  <NAME>
          Profile name

Options:
      --backend <BACKEND>
          Profile backend scope (for example chromium, brave, camoufox). Required when the same profile name exists under multiple backends

      --profile-root <PROFILE_ROOT>
          Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles`

  -h, --help
          Print help
```

### afhttp profile downloads - List files captured in the profile's browser download directory

```text
List files captured in the profile's browser download directory

Usage: downloads [OPTIONS] <NAME>

Arguments:
  <NAME>
          Profile name

Options:
      --backend <BACKEND>
          Profile backend scope (for example chromium, brave, camoufox). Required when the same profile name exists under multiple backends

      --profile-root <PROFILE_ROOT>
          Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles`

  -h, --help
          Print help
```

### afhttp profile delete - Delete a profile and all of its on-disk state

```text
Delete a profile and all of its on-disk state

Usage: delete [OPTIONS] --confirm <CONFIRM> <NAME>

Arguments:
  <NAME>
          Profile name to delete

Options:
      --backend <BACKEND>
          Profile backend scope (for example chromium, brave, camoufox). Required when the same profile name exists under multiple backends

      --confirm <CONFIRM>
          Confirmation guard: must equal the profile name for the delete to proceed

      --profile-root <PROFILE_ROOT>
          Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles`

  -h, --help
          Print help
```

### afhttp profile prune - Delete profiles whose last use is older than a cutoff

```text
Delete profiles whose last use is older than a cutoff

Usage: prune [OPTIONS] --older-than <OLDER_THAN>

Options:
      --older-than <OLDER_THAN>
          Age cutoff (e.g. `30d`, `12h`); profiles last used before this are removed

      --dry-run
          Report what would be deleted without deleting anything

      --profile-root <PROFILE_ROOT>
          Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles`

  -h, --help
          Print help
```

### afhttp profile cookies - Show the non-expired cookies in a profile's jar (values redacted)

```text
Show the non-expired cookies in a profile's jar (values redacted)

Usage: cookies [OPTIONS] <NAME>

Arguments:
  <NAME>
          Profile name

Options:
      --backend <BACKEND>
          Profile backend scope (for example chromium, brave, camoufox). Required when the same profile name exists under multiple backends

      --profile-root <PROFILE_ROOT>
          Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles`

  -h, --help
          Print help
```

## afhttp tabs - List and close CDP targets attached to the host

```text
List and close CDP targets attached to the host

Usage: tabs <COMMAND>

Commands:
  list   List currently-attached CDP targets
  close  Close a target by its CDP target id
  help   Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

### afhttp tabs list - List currently-attached CDP targets

```text
List currently-attached CDP targets

Usage: list [OPTIONS] --endpoint-url <ENDPOINT>

Options:
      --endpoint-url <ENDPOINT>
          CDP endpoint URL (e.g. `ws://127.0.0.1:9222`). Falls back to `AFHTTP_ENDPOINT_URL`

          [env: AFHTTP_ENDPOINT_URL=]

      --token-secret <TOKEN>
          Bearer token, if the host was started with `--token-secret`. Falls back to `AFHTTP_TOKEN_SECRET`

          [env: AFHTTP_TOKEN_SECRET=]

  -h, --help
          Print help
```

### afhttp tabs close - Close a target by its CDP target id

```text
Close a target by its CDP target id

Usage: close [OPTIONS] --tab <TAB> --endpoint-url <ENDPOINT>

Options:
      --tab <TAB>
          CDP target id (tab) to close (e.g. `41A0F1E0FD…`)

      --endpoint-url <ENDPOINT>
          CDP endpoint URL (e.g. `ws://127.0.0.1:9222`). Falls back to `AFHTTP_ENDPOINT_URL`

          [env: AFHTTP_ENDPOINT_URL=]

      --token-secret <TOKEN>
          Bearer token, if the host was started with `--token-secret`. Falls back to `AFHTTP_TOKEN_SECRET`

          [env: AFHTTP_TOKEN_SECRET=]

  -h, --help
          Print help
```

## afhttp skill - Install, remove, or check the embedded Agent Skill (Codex, Claude Code, opencode)

```text
Install, remove, or check the embedded Agent Skill (Codex, Claude Code, opencode)

Usage: skill <COMMAND>

Commands:
  status     Show whether the skill is installed, valid, and up to date
  install    Install or refresh the skill
  uninstall  Remove a managed skill
  help       Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

### afhttp skill status - Show whether the skill is installed, valid, and up to date

```text
Show whether the skill is installed, valid, and up to date

Usage: status [OPTIONS]

Options:
      --agent <AGENT>
          Agent to manage

          [default: all]
          [possible values: all, codex, claude-code, opencode]

      --scope <SCOPE>
          Skill scope (project is Claude Code / opencode only)

          [default: personal]
          [possible values: personal, project]

      --skills-dir <SKILLS_DIR>
          Skills directory; requires a single concrete --agent

  -h, --help
          Print help
```

### afhttp skill install - Install or refresh the skill

```text
Install or refresh the skill

Usage: install [OPTIONS]

Options:
      --agent <AGENT>
          Agent to manage

          [default: all]
          [possible values: all, codex, claude-code, opencode]

      --scope <SCOPE>
          Skill scope (project is Claude Code / opencode only)

          [default: personal]
          [possible values: personal, project]

      --skills-dir <SKILLS_DIR>
          Skills directory; requires a single concrete --agent

      --force
          Overwrite or remove a skill this tool did not manage

  -h, --help
          Print help
```

### afhttp skill uninstall - Remove a managed skill

```text
Remove a managed skill

Usage: uninstall [OPTIONS]

Options:
      --agent <AGENT>
          Agent to manage

          [default: all]
          [possible values: all, codex, claude-code, opencode]

      --scope <SCOPE>
          Skill scope (project is Claude Code / opencode only)

          [default: personal]
          [possible values: personal, project]

      --skills-dir <SKILLS_DIR>
          Skills directory; requires a single concrete --agent

      --force
          Overwrite or remove a skill this tool did not manage

  -h, --help
          Print help
```

## afhttp container - Build and run the host container (Docker or Apple) from the embedded recipe

```text
Build and run the host container (Docker or Apple) from the embedded recipe

Usage: container <COMMAND>

Commands:
  install    Build the host image if missing and run the container; print the client command
  uninstall  Stop and remove the container (--purge also removes the image and cache)
  status     Report whether the host is running, with its endpoint and client command
  logs       Capture or explicitly stream the container logs
  help       Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help
```

### afhttp container install - Build the host image if missing and run the container; print the client command

```text
Build the host image if missing and run the container; print the client command

Usage: install [OPTIONS] [HOST_ARGS]...

Arguments:
  [HOST_ARGS]...
          Extra args passed through to `afhttp host` inside the container

Options:
      --runtime <RUNTIME>
          Container runtime: docker, podman, or apple (auto-detected if omitted)

          Possible values:
          - docker
          - podman
          - apple:  Apple's `container` CLI. Accepts `apple` or `container` on the command line; its binary is `container` (see [`Runtime::bin`])

      --name <NAME>
          Container name

          [default: afhttp-host]

      --port <PORT>
          Host CDP port, published on 127.0.0.1

          [default: 9222]

      --profile <PROFILE>
          Initial logical profile name inside the container. Defaults to `-` (ephemeral); persistent profiles are scoped by backend

      --shm-size <SHM_SIZE>
          Chromium /dev/shm size. Defaults to `1g`, or `2g` when takeover is on

      --takeover-provider <TAKEOVER_PROVIDER>
          Real-display takeover provider for the built host. A provider name (default `kasmvnc`) builds a Brave + KasmVNC takeover-ready host with an ephemeral initial profile and 2g /dev/shm; `off` builds a lean headless host

          [default: kasmvnc]
          [possible values: off, kasmvnc]

      --with <COMPONENT>
          Extra component to build into the image (repeatable). Browser backends: chrome-headless-shell, lightpanda, fingerprint-chromium, camoufox, brave. Plus the takeover provider: kasmvnc

      --rebuild
          Rebuild the image even if it already exists

      --from-source
          Build the full image from a source checkout (container/docker/Dockerfile) instead of downloading the prebuilt release. Needs the source tree

      --context <DIR>
          Source checkout to build from with --from-source (default: current dir, then the checkout this afhttp binary was built from)

      --reveal-token-secret
          Explicitly include the long-lived host token in stdout

  -h, --help
          Print help (see a summary with '-h')
```

### afhttp container uninstall - Stop and remove the container (--purge also removes the image and cache)

```text
Stop and remove the container (--purge also removes the image and cache)

Usage: uninstall [OPTIONS]

Options:
      --runtime <RUNTIME>
          Container runtime: docker, podman, or apple (auto-detected if omitted)

          Possible values:
          - docker
          - podman
          - apple:  Apple's `container` CLI. Accepts `apple` or `container` on the command line; its binary is `container` (see [`Runtime::bin`])

      --name <NAME>
          Container name

          [default: afhttp-host]

      --purge
          Also remove the built image and the cached build context

  -h, --help
          Print help (see a summary with '-h')
```

### afhttp container status - Report whether the host is running, with its endpoint and client command

```text
Report whether the host is running, with its endpoint and client command

Usage: status [OPTIONS]

Options:
      --runtime <RUNTIME>
          Container runtime: docker, podman, or apple (auto-detected if omitted)

          Possible values:
          - docker
          - podman
          - apple:  Apple's `container` CLI. Accepts `apple` or `container` on the command line; its binary is `container` (see [`Runtime::bin`])

      --name <NAME>
          Container name

          [default: afhttp-host]

      --port <PORT>
          Published host port, used to format the endpoint and client command

          [default: 9222]

      --reveal-token-secret
          Explicitly include the long-lived host token in stdout

  -h, --help
          Print help (see a summary with '-h')
```

### afhttp container logs - Capture or explicitly stream the container logs

```text
Capture or explicitly stream the container logs

Usage: logs [OPTIONS]

Options:
      --runtime <RUNTIME>
          Container runtime: docker, podman, or apple (auto-detected if omitted)

          Possible values:
          - docker
          - podman
          - apple:  Apple's `container` CLI. Accepts `apple` or `container` on the command line; its binary is `container` (see [`Runtime::bin`])

      --name <NAME>
          Container name

          [default: afhttp-host]

      --follow
          Follow the log output

      --raw
          Stream raw runtime logs instead of returning a JSON summary

  -h, --help
          Print help (see a summary with '-h')
```
