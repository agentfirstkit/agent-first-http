<!-- Generated. Do not edit by hand. Regenerate: afhttp --help --recursive --output markdown -->

# afhttp CLI Reference

# afhttp - Give your AI agent its own private browser — so it reads the real page, past logins and bot walls, without ever touching yours.

```text
afhttp [OPTIONS] <COMMAND>
```

| Argument | Description |
| --- | --- |
| `--output-to <OUTPUT_TO>` | Where protocol events go: split (default), stdout, or stderr *(global, default: `split`)* |
| `--stdout-file <PATH>` | Redirect stdout bytes to this file *(global)* |
| `--stderr-file <PATH>` | Redirect stderr bytes to this file *(global)* |
| `--help` | Print help. Add --recursive to expand every nested subcommand; add --output plain\|json\|yaml\|markdown to choose the format. |
| `--version` | Print version |

| Command | Summary |
| --- | --- |
| `afhttp fetch` | Fetch a URL |
| `afhttp host` | Run the browser host |
| `afhttp upload` | Upload a local file to a browser tab via DOM.setFileInputFiles |
| `afhttp cdp` | Send a raw CDP method |
| `afhttp panel` | Print a short-lived takeover URL |
| `afhttp health` | Query /health |
| `afhttp capabilities` | Query /capabilities |
| `afhttp profile` | Local profile lifecycle commands |
| `afhttp tabs` | List and close CDP targets attached to the host |
| `afhttp skill` | Install, remove, or check the embedded Agent Skill (Codex, Claude Code, opencode, Hermes) |
| `afhttp container` | Build and run the host container (Docker or Apple) from the embedded recipe |

## afhttp fetch - Fetch a URL

Use `--takeover` to keep a persistent tab open when a captcha, login, or 2FA wall needs a human. The result returns the short-lived takeover URL in `next_action.takeover_url_secret` together with a command that re-fetches the same tab.

```text
afhttp fetch [OPTIONS] <URL>
```

| Argument | Description |
| --- | --- |
| `URL` | URL to fetch *(required)* |
| `--endpoint-url <ENDPOINT>` | CDP endpoint of a running host |
| `--token-secret <TOKEN>` | Bearer token for a protected host |
| `--browser <BROWSER>` | Browser backend for the inline host *(default: `auto`)* |
| `--browser-bin <PATH>` | Browser binary path for the inline host |
| `--render <RENDER>` | Render strategy: none, auto, or always *(default: `auto`)* |
| `--tab <new\|<id>>` | Browser tab target: new or an existing CDP target ID *(default: `new`)* |
| `--takeover` | Escalate captcha, login, or 2FA walls to human takeover |
| `--profile <PROFILE>` | Host profile to use for this fetch |
| `--wait <WAIT>` | Readiness signal before browser capture *(default: `auto`)* |
| `--header <NAME:VALUE>...` | Add a request header (repeatable) |
| `--cookie <NAME=VALUE>...` | Add a request cookie (repeatable). Format: `name=value` |
| `--user-agent <USER_AGENT>` | Override the User-Agent header for this fetch |
| `--evaluate-after-wait <JS>...` | Evaluate JavaScript after readiness (repeatable) |
| `--want <WANT>...` | Artifacts to capture, comma-separated |
| `--method <METHOD>` | HTTP method. Common values: POST, PUT, PATCH, DELETE *(default: `GET`)* |
| `--data <STRING\|@FILE>` | Request body as a string or `@FILE` |
| `--form <NAME=VALUE>...` | Add a URL-encoded form field (repeatable) |
| `--network-bodies <NETWORK_BODIES>` | Capture network response bodies: off, xhr, or all *(default: `off`)* |
| `--network-body-max-bytes <NETWORK_BODY_MAX_BYTES>` | Per-body byte cap for captured network sub-requests *(default: `10485760`)* |
| `--readiness-idle-ms <READINESS_IDLE_MS>` | Network quiet window used by --wait auto, in milliseconds *(default: `800`)* |
| `--readiness-stable-ms <READINESS_STABLE_MS>` | DOM/text unchanged window used by --wait auto, in milliseconds *(default: `500`)* |
| `--readiness-min-text-bytes <READINESS_MIN_TEXT_BYTES>` | Low visible-text byte threshold for --wait auto quality warnings only *(default: `32`)* |
| `--no-network-redact` | Disable sensitive-value redaction in network.json |
| `--out <OUT>` | Directory in which to write artifacts |
| `--cookie-jar <COOKIE_JAR>` | Override the profile cookie-jar path |
| `--no-cookie-jar` | Disable cookie-jar replay and persistence for this fetch |
| `--observe-main-wait-ms <OBSERVE_MAIN_WAIT_MS>` | Maximum wait for the browser's main-document event, in milliseconds *(default: `500`)* |
| `--max-response-bytes <MAX_RESPONSE_BYTES>` | Maximum main HTTP-path response body size in bytes *(default: `1073741824`)* |
| `--retry <RETRY>` | Number of retry attempts after the first *(default: `0`)* |
| `--backoff-ms <BACKOFF_MS>` | Fixed delay between retries, in milliseconds *(default: `250`)* |
| `--proxy-url <PROXY>` | Upstream proxy for this fetch's HTTP fast path |
| `--ca-cert <CA_CERT>` | PEM file with extra root CAs for the HTTP path |
| `--tls-insecure` | Disable TLS certificate verification on the HTTP path |
| `--timeout-ms <TIMEOUT_MS>` | Overall fetch timeout, in milliseconds. Applies to both the HTTP fast path and the browser path *(default: `30000`)* |
| `--capture-ws` | Capture WebSocket frame payloads |
| `--capture-sse` | Capture SSE event payloads |

## afhttp host - Run the browser host

```text
afhttp host [OPTIONS] --listen <LISTEN>
```

| Argument | Description |
| --- | --- |
| `--listen <LISTEN>` | Listener address: `tcp:host:port` or `unix:/path/to.sock` *(required)* |
| `--profile <PROFILE>` | Initial profile name, or `-` for an ephemeral profile *(default: `-`)* |
| `--display <DISPLAY>` | Display mode. Omit when `--takeover-provider` should imply headful |
| `--takeover-provider <TAKEOVER_PROVIDER>` | Real-display takeover provider: off or kasmvnc *(default: `off`)* |
| `--takeover-quality-percent <TAKEOVER_QUALITY_PERCENT>` | Takeover image quality hint, from 0 to 100 percent *(default: `100`)* |
| `--browser <BROWSER>` | Browser backend *(default: `auto`)* |
| `--browser-bin <BROWSER_BIN>` | Override browser binary path |
| `--token-secret <TOKEN>` | Bearer token required for clients on TCP listeners |
| `--no-health` | Disable serving /health and /capabilities (served by default) |
| `--health-public <HEALTH_PUBLIC>` | Make /health public with minimal payload *(default: `off`)* |
| `--engine-env <NAME=VALUE>...` | Set a browser subprocess environment variable (repeatable) |
| `--browser-arg <FLAG>...` | Append a raw browser backend flag (repeatable) |
| `--proxy-url <PROXY>` | Explicit upstream browser proxy URL |
| `--recent-requests-cap <RECENT_REQUESTS_CAP>` | Enable /recent-requests with a bounded ring of N entries. 0 = off *(default: `0`)* |

## afhttp upload - Upload a local file to a browser tab via DOM.setFileInputFiles

```text
afhttp upload [OPTIONS] --endpoint-url <ENDPOINT> --tab <TAB> --selector <SELECTOR> --file <FILE>
```

| Argument | Description |
| --- | --- |
| `--endpoint-url <ENDPOINT>` | CDP endpoint of the running host (e.g. `ws://127.0.0.1:9222`). Falls back to `AFHTTP_ENDPOINT_URL` *(required)* |
| `--token-secret <TOKEN>` | Bearer token, if the host was started with `--token-secret`. Falls back to `AFHTTP_TOKEN_SECRET` |
| `--tab <TAB>` | CDP target id (tab) to operate in *(required)* |
| `--selector <SELECTOR>` | CSS selector for the `<input type=file>` element *(required)* |
| `--file <FILE>` | Local file path to upload *(required)* |

## afhttp cdp - Send a raw CDP method

```text
afhttp cdp [OPTIONS] --endpoint-url <ENDPOINT> --tab <TAB> <METHOD>
```

| Argument | Description |
| --- | --- |
| `METHOD` | CDP method name (e.g. Runtime.evaluate) *(required)* |
| `--endpoint-url <ENDPOINT>` | CDP endpoint of the running host (e.g. `ws://127.0.0.1:9222`). Falls back to `AFHTTP_ENDPOINT_URL` *(required)* |
| `--token-secret <TOKEN>` | Bearer token, if the host was started with `--token-secret`. Falls back to `AFHTTP_TOKEN_SECRET` |
| `--tab <TAB>` | CDP target id (tab) to drive *(required)* |
| `--params <JSON\|@->` | JSON literal, or `@-` to read from stdin |
| `--wait-event <WAIT>` | "<event>:<timeout>" — wait for a CDP event before exiting |

## afhttp panel - Print a short-lived takeover URL

```text
afhttp panel [OPTIONS] --endpoint-url <ENDPOINT>
```

| Argument | Description |
| --- | --- |
| `--endpoint-url <ENDPOINT>` | CDP endpoint of the running host (e.g. ws://127.0.0.1:9222). Falls back to `AFHTTP_ENDPOINT_URL` *(required)* |
| `--token-secret <TOKEN>` | Bearer token, if the host requires one. Falls back to `AFHTTP_TOKEN_SECRET` |

## afhttp health - Query /health

```text
afhttp health [OPTIONS] --endpoint-url <ENDPOINT>
```

| Argument | Description |
| --- | --- |
| `--endpoint-url <ENDPOINT>` | CDP endpoint of the running host (e.g. `ws://127.0.0.1:9222`). Falls back to `AFHTTP_ENDPOINT_URL` *(required)* |
| `--token-secret <TOKEN>` | Bearer token, if the host was started with `--token-secret`. Falls back to `AFHTTP_TOKEN_SECRET` |

## afhttp capabilities - Query /capabilities

```text
afhttp capabilities [OPTIONS] --endpoint-url <ENDPOINT>
```

| Argument | Description |
| --- | --- |
| `--endpoint-url <ENDPOINT>` | CDP endpoint of the running host (e.g. `ws://127.0.0.1:9222`). Falls back to `AFHTTP_ENDPOINT_URL` *(required)* |
| `--token-secret <TOKEN>` | Bearer token, if the host was started with `--token-secret`. Falls back to `AFHTTP_TOKEN_SECRET` |

## afhttp profile - Local profile lifecycle commands

```text
afhttp profile <COMMAND>
```

| Command | Summary |
| --- | --- |
| `afhttp profile list` | List on-disk profiles under the profiles root |
| `afhttp profile info` | Show metadata for one profile (size, last use, lock state) |
| `afhttp profile lock-status` | Report whether a profile is currently locked by a running host |
| `afhttp profile downloads` | List files captured in the profile's browser download directory |
| `afhttp profile delete` | Delete a profile and all of its on-disk state |
| `afhttp profile prune` | Delete profiles whose last use is older than a cutoff |
| `afhttp profile cookies` | Show the non-expired cookies in a profile's jar (values redacted) |

### afhttp profile list - List on-disk profiles under the profiles root

```text
afhttp profile list [OPTIONS]
```

| Argument | Description |
| --- | --- |
| `--profile-root <PROFILE_ROOT>` | Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles` |

### afhttp profile info - Show metadata for one profile (size, last use, lock state)

```text
afhttp profile info [OPTIONS] <NAME>
```

| Argument | Description |
| --- | --- |
| `NAME` | Profile name *(required)* |
| `--backend <BACKEND>` | Filter by browser backend |
| `--profile-root <PROFILE_ROOT>` | Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles` |

### afhttp profile lock-status - Report whether a profile is currently locked by a running host

```text
afhttp profile lock-status [OPTIONS] <NAME>
```

| Argument | Description |
| --- | --- |
| `NAME` | Profile name *(required)* |
| `--backend <BACKEND>` | Filter by browser backend |
| `--profile-root <PROFILE_ROOT>` | Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles` |

### afhttp profile downloads - List files captured in the profile's browser download directory

```text
afhttp profile downloads [OPTIONS] <NAME>
```

| Argument | Description |
| --- | --- |
| `NAME` | Profile name *(required)* |
| `--backend <BACKEND>` | Filter by browser backend |
| `--profile-root <PROFILE_ROOT>` | Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles` |

### afhttp profile delete - Delete a profile and all of its on-disk state

```text
afhttp profile delete [OPTIONS] --confirm <CONFIRM> <NAME>
```

| Argument | Description |
| --- | --- |
| `NAME` | Profile name to delete *(required)* |
| `--backend <BACKEND>` | Filter by browser backend |
| `--confirm <CONFIRM>` | Confirmation guard: must equal the profile name for the delete to proceed *(required)* |
| `--profile-root <PROFILE_ROOT>` | Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles` |

### afhttp profile prune - Delete profiles whose last use is older than a cutoff

```text
afhttp profile prune [OPTIONS] --older-than <OLDER_THAN>
```

| Argument | Description |
| --- | --- |
| `--older-than <OLDER_THAN>` | Age cutoff (e.g. `30d`, `12h`); profiles last used before this are removed *(required)* |
| `--dry-run` | Report what would be deleted without deleting anything *(default: `false`)* |
| `--profile-root <PROFILE_ROOT>` | Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles` |

### afhttp profile cookies - Show the non-expired cookies in a profile's jar (values redacted)

```text
afhttp profile cookies [OPTIONS] <NAME>
```

| Argument | Description |
| --- | --- |
| `NAME` | Profile name *(required)* |
| `--backend <BACKEND>` | Filter by browser backend |
| `--profile-root <PROFILE_ROOT>` | Profiles root directory. Defaults to `$XDG_DATA_HOME/afhttp/profiles` |

## afhttp tabs - List and close CDP targets attached to the host

```text
afhttp tabs <COMMAND>
```

| Command | Summary |
| --- | --- |
| `afhttp tabs list` | List currently-attached CDP targets |
| `afhttp tabs close` | Close a target by its CDP target id |

### afhttp tabs list - List currently-attached CDP targets

```text
afhttp tabs list [OPTIONS] --endpoint-url <ENDPOINT>
```

| Argument | Description |
| --- | --- |
| `--endpoint-url <ENDPOINT>` | CDP endpoint URL (e.g. `ws://127.0.0.1:9222`). Falls back to `AFHTTP_ENDPOINT_URL` *(required)* |
| `--token-secret <TOKEN>` | Bearer token, if the host was started with `--token-secret`. Falls back to `AFHTTP_TOKEN_SECRET` |

### afhttp tabs close - Close a target by its CDP target id

```text
afhttp tabs close [OPTIONS] --tab <TAB> --endpoint-url <ENDPOINT>
```

| Argument | Description |
| --- | --- |
| `--tab <TAB>` | CDP target id (tab) to close (e.g. `41A0F1E0FD…`) *(required)* |
| `--endpoint-url <ENDPOINT>` | CDP endpoint URL (e.g. `ws://127.0.0.1:9222`). Falls back to `AFHTTP_ENDPOINT_URL` *(required)* |
| `--token-secret <TOKEN>` | Bearer token, if the host was started with `--token-secret`. Falls back to `AFHTTP_TOKEN_SECRET` |

## afhttp skill - Install, remove, or check the embedded Agent Skill (Codex, Claude Code, opencode, Hermes)

```text
afhttp skill <COMMAND>
```

| Command | Summary |
| --- | --- |
| `afhttp skill status` | Show whether the skill is installed, valid, and up to date |
| `afhttp skill install` | Install or refresh the skill |
| `afhttp skill uninstall` | Remove a managed skill |

### afhttp skill status - Show whether the skill is installed, valid, and up to date

```text
afhttp skill status [OPTIONS]
```

| Argument | Description |
| --- | --- |
| `--agent <AGENT>` | Agent to manage *(default: `all`)* |
| `--scope <SCOPE>` | Skill scope *(default: `personal`)* |
| `--skills-dir <SKILLS_DIR>` | Skills directory; requires a single concrete --agent |

### afhttp skill install - Install or refresh the skill

```text
afhttp skill install [OPTIONS]
```

| Argument | Description |
| --- | --- |
| `--agent <AGENT>` | Agent to manage *(default: `all`)* |
| `--scope <SCOPE>` | Skill scope *(default: `personal`)* |
| `--skills-dir <SKILLS_DIR>` | Skills directory; requires a single concrete --agent |
| `--force` | Overwrite or remove a skill this tool did not manage |

### afhttp skill uninstall - Remove a managed skill

```text
afhttp skill uninstall [OPTIONS]
```

| Argument | Description |
| --- | --- |
| `--agent <AGENT>` | Agent to manage *(default: `all`)* |
| `--scope <SCOPE>` | Skill scope *(default: `personal`)* |
| `--skills-dir <SKILLS_DIR>` | Skills directory; requires a single concrete --agent |
| `--force` | Overwrite or remove a skill this tool did not manage |

## afhttp container - Build and run the host container (Docker or Apple) from the embedded recipe

```text
afhttp container <COMMAND>
```

| Command | Summary |
| --- | --- |
| `afhttp container install` | Build the host image if missing and run the container; print the client command |
| `afhttp container uninstall` | Stop and remove the container (--purge also removes the image and cache) |
| `afhttp container status` | Report whether the host is running, with its endpoint and client command |
| `afhttp container logs` | Capture or explicitly stream the container logs |

### afhttp container install - Build the host image if missing and run the container; print the client command

```text
afhttp container install [OPTIONS] [HOST_ARGS]...
```

| Argument | Description |
| --- | --- |
| `--runtime <RUNTIME>` | Container runtime: docker, podman, or apple (auto-detected if omitted) |
| `--name <NAME>` | Container name *(default: `afhttp-host`)* |
| `--port <PORT>` | Host CDP port, published on 127.0.0.1 *(default: `9222`)* |
| `--profile <PROFILE>` | Initial profile name inside the container |
| `--shm-size <SHM_SIZE>` | Chromium /dev/shm size. Defaults to `1g`, or `2g` when takeover is on |
| `--takeover-provider <TAKEOVER_PROVIDER>` | Real-display takeover provider for the built host *(default: `kasmvnc`)* |
| `--with <COMPONENT>...` | Extra image component to build (repeatable) |
| `--rebuild` | Rebuild the image even if it already exists |
| `--from-source` | Build the image from a source checkout |
| `--context <DIR>` | Source checkout used by --from-source |
| `HOST_ARGS...` | Extra args passed through to `afhttp host` inside the container |
| `--reveal-token-secret` | Explicitly include the long-lived host token in stdout |

### afhttp container uninstall - Stop and remove the container (--purge also removes the image and cache)

```text
afhttp container uninstall [OPTIONS]
```

| Argument | Description |
| --- | --- |
| `--runtime <RUNTIME>` | Container runtime: docker, podman, or apple (auto-detected if omitted) |
| `--name <NAME>` | Container name *(default: `afhttp-host`)* |
| `--purge` | Also remove the built image and the cached build context |

### afhttp container status - Report whether the host is running, with its endpoint and client command

```text
afhttp container status [OPTIONS]
```

| Argument | Description |
| --- | --- |
| `--runtime <RUNTIME>` | Container runtime: docker, podman, or apple (auto-detected if omitted) |
| `--name <NAME>` | Container name *(default: `afhttp-host`)* |
| `--port <PORT>` | Published host port, used to format the endpoint and client command *(default: `9222`)* |
| `--reveal-token-secret` | Explicitly include the long-lived host token in stdout |

### afhttp container logs - Capture or explicitly stream the container logs

```text
afhttp container logs [OPTIONS]
```

| Argument | Description |
| --- | --- |
| `--runtime <RUNTIME>` | Container runtime: docker, podman, or apple (auto-detected if omitted) |
| `--name <NAME>` | Container name *(default: `afhttp-host`)* |
| `--follow` | Follow the log output |
| `--raw` | Stream raw runtime logs instead of returning a JSON summary |
