#!/usr/bin/env python3
"""Drive a real KasmVNC takeover through `afui session serve` and report what happened.

Run it with tests/takeover-in-workbench.sh, which supplies the container this
needs: KasmVNC, an X server, a Chromium, and a checkout of agent-first-ui next
to this one.

Nothing here is a unit test. It starts the three real processes a person's
takeover would involve — `afhttp host --takeover-provider kasmvnc`, `afhttp ui
takeover --mode session`, `afui session serve` — walks the panel exactly
as a browser would, and then puts a real browser in front of it. Every check
prints PASS/FAIL with what it actually saw, because the point is evidence
rather than a green tick.
"""

import base64
import hashlib
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

WORKSPACE = "/workspace"
AFUI_SOURCE = "/agent-first-ui"
HOST_PORT = 9222
SHELL_PORT = 8787
HOST_TOKEN = "workbench-e2e-token"

results = []


def check(name, ok, detail=""):
    results.append((name, ok, detail))
    print(f"{'PASS' if ok else 'FAIL'}  {name}")
    if detail:
        for line in str(detail).splitlines():
            print(f"        {line}")
    sys.stdout.flush()
    return ok


def note(text):
    print(f"      · {text}")
    sys.stdout.flush()


def run(cmd, **kwargs):
    print(f"$ {' '.join(cmd)}")
    sys.stdout.flush()
    return subprocess.run(cmd, check=True, **kwargs)


def wait_for_port(port, timeout=90.0, host="127.0.0.1"):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with socket.create_connection((host, port), 0.5):
                return True
        except OSError:
            time.sleep(0.25)
    return False


# ── HTTP, by hand, so nothing a client library does is mistaken for the proxy ──


def joined(message):
    """Headers as one string per name, a repeated one joined the way HTTP reads
    it: AFUI sends its frame sandbox as a policy of its own, beside the panel's."""
    headers = {}
    for name, value in message.items():
        headers[name] = f"{headers[name]}, {value}" if name in headers else value
    return headers


def http(url, headers=None, method="GET"):
    request = urllib.request.Request(url, method=method)
    for name, value in (headers or {}).items():
        request.add_header(name, value)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, joined(response.headers), response.read()
    except urllib.error.HTTPError as error:
        return error.code, joined(error.headers), error.read()


# ── WebSocket, by hand, because what has to be observed is the handshake ──

GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


def ws_open(host, port, path, extra_headers=None, timeout=20.0, host_header=None):
    """Open one WebSocket and return (status, headers, accept_ok, socket, buffered).

    `host_header` separates the name the request claims from the address it goes
    to, which is what reaching a session that has an origin of its own needs:
    the name is one a browser resolves and this script never has to.
    """
    key = base64.b64encode(os.urandom(16)).decode()
    lines = [
        f"GET {path} HTTP/1.1",
        f"Host: {host_header or f'{host}:{port}'}",
        "Upgrade: websocket",
        "Connection: Upgrade",
        f"Sec-WebSocket-Key: {key}",
        "Sec-WebSocket-Version: 13",
    ]
    for name, value in (extra_headers or {}).items():
        lines.append(f"{name}: {value}")
    request = ("\r\n".join(lines) + "\r\n\r\n").encode()

    connection = socket.create_connection((host, port), timeout)
    connection.settimeout(timeout)
    connection.sendall(request)
    buffered = b""
    while b"\r\n\r\n" not in buffered:
        chunk = connection.recv(4096)
        if not chunk:
            break
        buffered += chunk
    head, _, rest = buffered.partition(b"\r\n\r\n")
    text = head.decode("latin-1")
    status = int(text.split(" ")[1]) if " " in text else 0
    headers = {}
    for line in text.split("\r\n")[1:]:
        if ":" in line:
            name, value = line.split(":", 1)
            headers[name.strip().lower()] = value.strip()
    expected = base64.b64encode(hashlib.sha1((key + GUID).encode()).digest()).decode()
    accept_ok = headers.get("sec-websocket-accept") == expected
    return status, headers, accept_ok, connection, rest


def ws_read_frame(connection, buffered, timeout=20.0):
    """Read one unmasked server frame. Returns (opcode, payload, leftover)."""
    deadline = time.time() + timeout

    def need(count):
        nonlocal buffered
        while len(buffered) < count:
            if time.time() > deadline:
                raise TimeoutError("no frame within the deadline")
            chunk = connection.recv(65536)
            if not chunk:
                raise ConnectionError("upstream closed before a frame arrived")
            buffered += chunk

    need(2)
    opcode = buffered[0] & 0x0F
    masked = bool(buffered[1] & 0x80)
    length = buffered[1] & 0x7F
    offset = 2
    if length == 126:
        need(4)
        length = int.from_bytes(buffered[2:4], "big")
        offset = 4
    elif length == 127:
        need(10)
        length = int.from_bytes(buffered[2:10], "big")
        offset = 10
    mask = b""
    if masked:
        need(offset + 4)
        mask = buffered[offset : offset + 4]
        offset += 4
    need(offset + length)
    payload = bytearray(buffered[offset : offset + length])
    if masked:
        for index in range(len(payload)):
            payload[index] ^= mask[index % 4]
    return opcode, bytes(payload), buffered[offset + length :]


# ── The processes a real takeover involves ──


class Processes:
    def __init__(self):
        self.started = []

    def start(self, name, cmd, env=None):
        print(f"$ {' '.join(cmd)}   [{name}]")
        sys.stdout.flush()
        log = open(f"/tmp/{name}.log", "wb")
        process = subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT, env=env)
        self.started.append((name, process, log))
        return process

    def log_of(self, name):
        try:
            with open(f"/tmp/{name}.log", "rb") as handle:
                return handle.read().decode("utf-8", "replace")
        except OSError:
            return ""

    def stop_all(self):
        for name, process, log in reversed(self.started):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
            log.close()


def emitted_events(text):
    events = []
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("{"):
            try:
                events.append(json.loads(line))
            except json.JSONDecodeError:
                pass
    return events


def kasmvnc_web_port():
    """The port KasmVNC's own web listener is on, read off the running Xvnc."""
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            with open(f"/proc/{entry}/cmdline", "rb") as handle:
                argv = handle.read().split(b"\x00")
        except OSError:
            continue
        if not argv or b"Xvnc" not in argv[0]:
            continue
        for index, argument in enumerate(argv):
            if argument == b"-websocketPort" and index + 1 < len(argv):
                return int(argv[index + 1])
    return None


def main():
    afui_home = tempfile.mkdtemp(prefix="afui-registry-")
    env = dict(os.environ)
    env["AFUI_CONFIG_DIR"] = afui_home
    procs = Processes()

    print("== building ==")
    run(["cargo", "build", "--features", "cli"], cwd=WORKSPACE)
    run(["cargo", "build", "--bin", "afui"], cwd=AFUI_SOURCE)
    target = os.environ.get("CARGO_TARGET_DIR", f"{WORKSPACE}/target")
    afhttp = f"{target}/debug/afhttp"
    afui = f"{target}/debug/afui"

    try:
        print("\n== a real takeover host ==")
        procs.start(
            "afhttp-host",
            [
                afhttp,
                "host",
                "--listen",
                f"tcp:127.0.0.1:{HOST_PORT}",
                "--takeover-provider",
                "kasmvnc",
                "--token-secret",
                HOST_TOKEN,
            ],
            env=env,
        )
        if not check(
            "afhttp host --takeover-provider kasmvnc is listening",
            wait_for_port(HOST_PORT, timeout=180),
            procs.log_of("afhttp-host")[-2000:],
        ):
            return 1
        web_port = kasmvnc_web_port()
        check("KasmVNC's own web listener was found", web_port is not None, f"port {web_port}")

        print("\n== the panel, announced as a UI session ==")
        procs.start(
            "afhttp-ui-takeover",
            [
                afhttp,
                "ui",
                "takeover",
                "--endpoint-url",
                f"http://127.0.0.1:{HOST_PORT}",
                "--token-secret",
                HOST_TOKEN,
                "--mode",
                "session",
            ],
            env=env,
        )
        session_id = None
        deadline = time.time() + 60
        while time.time() < deadline and session_id is None:
            for event in emitted_events(procs.log_of("afhttp-ui-takeover")):
                candidate = (event.get("progress") or {}).get("session_id")
                if candidate:
                    session_id = candidate
            time.sleep(0.3)
        check(
            "`afhttp ui takeover --mode session` published a session",
            session_id is not None,
            procs.log_of("afhttp-ui-takeover")[-2000:],
        )
        if session_id is None:
            return 1

        listed = subprocess.run(
            [afui, "session", "list"], capture_output=True, text=True, env=env
        )
        listing = json.loads(listed.stdout or "{}")
        entries = listing.get("result", listing).get("sessions", [])
        mine = [entry for entry in entries if entry.get("session_id") == session_id]
        check(
            "afhttp takeover appears in `afui session list`",
            bool(mine),
            json.dumps(entries, indent=2),
        )
        check(
            "the listing carries no credential",
            "handoff_secret" not in listed.stdout,
            listed.stdout.strip()[:500],
        )

        # The local half of the same listing: `afui session open` hands the
        # announced URL to a window of its own. A stub browser stands in for
        # Chromium so the check is about what AFUI opened, not about whether
        # this container has a display.
        stub = tempfile.mkdtemp(prefix="stub-browser-")
        argv_log = f"{stub}/argv"
        target_log = f"{stub}/target"
        # The window is launched on a private local page that forwards to the
        # panel, so the credential never sits in the world-readable arguments.
        # The stub resolves the page while it still exists: the window's
        # directory goes with the window.
        with open(f"{stub}/browser", "w") as handle:
            handle.write(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > " + argv_log + "\n"
                'for arg in "$@"; do\n'
                '  case "$arg" in\n'
                "    --app=file://*) sed -n 's/.*location\\.replace(\"\\(.*\\)\");.*/\\1/p'"
                ' "${arg#--app=file://}" > ' + target_log + " ;;\n"
                "  esac\n"
                "done\n"
                "sleep 2\n"
            )
        os.chmod(f"{stub}/browser", 0o755)
        opened = subprocess.run(
            [afui, "session", "open", session_id],
            capture_output=True,
            text=True,
            env={**env, "AFUI_BROWSER_BINARY": f"{stub}/browser"},
        )
        launched = ""
        target = ""
        try:
            with open(argv_log) as handle:
                launched = handle.read()
            with open(target_log) as handle:
                target = handle.read().strip()
        except OSError:
            pass
        check(
            "`afui session open` opens a window onto the announced panel",
            target.startswith("http://127.0.0.1:9222/takeover/panel?handoff_secret=")
            and "handoff_secret" not in launched,
            (launched or opened.stdout + opened.stderr)[:600],
        )
        shutil.rmtree(stub, ignore_errors=True)

        # The shell, in both the shapes it can take. They differ in one thing:
        # where a framed session lives. Under a path on the page's own origin
        # the frame has to be sandboxed into an opaque origin, which costs it
        # browser storage; under a name of its own it keeps it. Everything else
        # asserted below has to hold either way, and the KasmVNC client is the
        # thing that can only live in the second.
        for index, (name, port, origin_host) in enumerate(
            (
                ("afui-session-serve", SHELL_PORT, None),
                ("afui-session-serve-own-origin", SHELL_PORT + 1, "localhost"),
            )
        ):
            shell = shell_phase(
                procs,
                env,
                afui,
                name,
                port,
                origin_host,
                session_id,
                9333 + index,
            )
            if shell is None:
                return 1

        print("\n== does KasmVNC need the Origin the proxy will not forward? ==")
        if web_port:
            with_origin = ws_open(
                "127.0.0.1",
                web_port,
                "/websockify",
                {
                    "Sec-WebSocket-Protocol": "binary",
                    "Origin": f"http://127.0.0.1:{web_port}",
                },
            )
            with_origin[3].close()
            without_origin = ws_open(
                "127.0.0.1", web_port, "/websockify", {"Sec-WebSocket-Protocol": "binary"}
            )
            without_origin[3].close()
            foreign_origin = ws_open(
                "127.0.0.1",
                web_port,
                "/websockify",
                {"Sec-WebSocket-Protocol": "binary", "Origin": "null"},
            )
            foreign_origin[3].close()
            note(f"KasmVNC direct, Origin matching its own authority: {with_origin[0]}")
            note(f"KasmVNC direct, no Origin at all:                  {without_origin[0]}")
            note(f"KasmVNC direct, Origin: null (what a sandboxed frame sends): {foreign_origin[0]}")
            check(
                "afhttp re-originates the upstream leg, so the proxy never has to forward Origin",
                with_origin[0] == 101,
                "the panel above upgraded with no Origin crossing the shell proxy",
            )

    finally:
        procs.stop_all()
        shutil.rmtree(afui_home, ignore_errors=True)

    print("\n== summary ==")
    failed = [name for name, ok, _ in results if not ok]
    for name, ok, _ in results:
        print(f"{'PASS' if ok else 'FAIL'}  {name}")
    return 1 if failed else 0


# ── One shell, in one of its two shapes ──


class Shell:
    """A running `afui session serve`, and how to reach what it frames.

    Every request connects to loopback and claims its name in a `Host` header.
    That is not a trick: a session with an origin of its own is reached at
    `s-<credential>.localhost`, which browsers resolve to loopback themselves,
    and separating the two here is what lets this script exercise the same
    address a browser would without relying on this container's resolver.
    """

    def __init__(self, serve_url, port, per_origin):
        self.serve_url = serve_url
        self.port = port
        self.per_origin = per_origin
        rest = serve_url[len("http://") :]
        self.host, _, path = rest.partition("/")
        self.page_path = "/" + path
        self.wire = f"http://127.0.0.1:{port}"

    @property
    def label(self):
        return "an origin per session" if self.per_origin else "one origin, sessions on paths"

    def get(self, path, headers=None, host=None):
        headers = dict(headers or {})
        headers.setdefault("Host", host or self.host)
        return http(f"{self.wire}{path}", headers)

    def card(self, session_id):
        status, _, body = self.get(f"{self.page_path}sessions")
        cards = json.loads(body).get("sessions", []) if status == 200 else []
        return next((c for c in cards if c["session_id"] == session_id), None), body

    def frame(self, card):
        """Where one session is framed: the name to claim, the path to ask for."""
        framed = card["proxy_url_secret"]
        if framed.startswith("//"):
            authority, _, path = framed[2:].partition("/")
            return Frame(self, authority, "/" + path)
        return Frame(self, self.host, framed)


class Frame:
    def __init__(self, shell, host, path):
        self.shell = shell
        self.host = host
        self.path = path

    @property
    def browser_url(self):
        """The address a browser is pointed at, which resolves on its own."""
        return f"http://{self.host}{self.path}"

    def get(self, suffix="", headers=None):
        return self.shell.get(f"{self.path}{suffix}", headers, host=self.host)


def shell_phase(procs, env, afui, name, port, origin_host, session_id, debug_port):
    """Everything the shell has to do, in one of its two shapes."""
    shape = "an origin per session" if origin_host else "one origin, sessions on paths"
    print(f"\n== the shell: {shape} ==")
    command = [afui, "session", "serve", "--listen", f"127.0.0.1:{port}"]
    if origin_host:
        command += ["--session-origin-host", origin_host]
    procs.start(name, command, env=env)
    wait_for_port(port, timeout=60)
    serve_url = None
    deadline = time.time() + 30
    while time.time() < deadline and serve_url is None:
        for event in emitted_events(procs.log_of(name)):
            candidate = (event.get("progress") or {}).get("serve_url")
            if candidate:
                serve_url = candidate
        time.sleep(0.3)
    if not check(
        f"[{shape}] `afui session serve` is answering",
        serve_url is not None,
        procs.log_of(name)[-2000:],
    ):
        return None
    shell = Shell(serve_url, port, bool(origin_host))
    note(f"the page is at {serve_url.rsplit('/', 2)[0]}/…/")

    card, body = shell.card(session_id)
    check(
        f"[{shape}] the shell frames the takeover panel",
        card is not None,
        body.decode()[:400],
    )
    if card is None:
        return None
    check(
        f"[{shape}] the page is never told afhttp's credential",
        "handoff_secret" not in body.decode(),
        body.decode()[:400],
    )
    frame = shell.frame(card)
    note(f"framed at {card['proxy_url_secret']}")

    print("\n== what the browser would do, step by step ==")
    status, headers, body = frame.get()
    landing = body.decode("utf-8", "replace")
    check(f"[{shape}] the framed panel answers through the proxy", status == 200, f"status {status}")
    check(
        f"[{shape}] no Set-Cookie reaches the browser",
        "Set-Cookie" not in headers and "set-cookie" not in headers,
        json.dumps(headers, indent=2),
    )
    # afhttp's panel sends no CSP of its own, so the proxy has none to narrow —
    # a Provider that says nothing about framing is not told anything either.
    # When one is present it must name the page, which is a different origin in
    # each shape and blanks every frame if it is wrong. On a path the response
    # also carries the frame's sandbox as a policy of its own, so opening the
    # frame's address by hand does not put the panel on the shell's origin.
    policies = [
        policy.strip()
        for policy in headers.get("content-security-policy", "").split(",")
        if policy.strip()
    ]
    sandbox = [policy for policy in policies if policy.startswith("sandbox ")]
    provider = [policy for policy in policies if not policy.startswith("sandbox ")]
    if origin_host:
        check(f"[{shape}] a session on its own origin carries no sandbox policy", not sandbox, policies)
    else:
        check(
            f"[{shape}] a session on a path carries the frame's sandbox itself",
            sandbox == ["sandbox allow-scripts allow-forms allow-popups allow-popups-to-escape-sandbox"],
            policies,
        )
    if provider:
        wanted = (
            f"frame-ancestors 'self' http://{shell.host}" if origin_host else "frame-ancestors 'self'"
        )
        check(f"[{shape}] only the page that framed it may frame it", wanted in provider[0], provider)
    else:
        note("the panel sends no Content-Security-Policy, so the proxy narrows nothing")
    check(
        f"[{shape}] the landing page derives its WebSocket path from where the browser is",
        "location.pathname" in landing and "path=takeover/panel/websockify" not in landing,
        landing[:600],
    )

    # Exactly what the bootstrap's own line would produce in a browser: the
    # prefix the browser is at, plus the settings the listener seeded.
    seeded = re.search(r'encodeURIComponent\(websockify\) \+ "(.*)"\);', landing)
    websockify_setting = frame.path.lstrip("/") + "websockify"
    settled = (
        f"?path={urllib.parse.quote(websockify_setting, safe='')}"
        f"{seeded.group(1) if seeded else '&resize=scale'}"
    )
    status, _, body = frame.get(settled)
    client = body.decode("utf-8", "replace")
    check(
        f"[{shape}] KasmVNC's own web client is served through the proxy",
        status == 200 and ("noVNC" in client or "KasmVNC" in client),
        f"status {status}; {len(body)} bytes; title "
        + (re.search(r"<title>([^<]*)</title>", client) or ["", "?"])[1],
    )
    asset = re.search(r'src="\.?/?(assets/[^"]+\.js)"', client)
    if asset:
        status, _, body = frame.get(asset.group(1))
        check(
            f"[{shape}] the client's own assets load through the proxy",
            status == 200 and len(body) > 1000,
            f"{asset.group(1)}: status {status}, {len(body)} bytes",
        )

    print("\n== RFB over WebSocket, through two proxies ==")
    status, headers, accept_ok, connection, rest = ws_open(
        "127.0.0.1",
        port,
        frame.path + "websockify",
        {"Sec-WebSocket-Protocol": "binary", "Origin": f"http://{frame.host}"},
        host_header=frame.host,
    )
    upgraded = check(
        f"[{shape}] the RFB socket upgrades through the shell proxy",
        status == 101 and accept_ok,
        f"status {status}; Sec-WebSocket-Accept valid: {accept_ok}; "
        f"subprotocol {headers.get('sec-websocket-protocol')!r}",
    )
    if upgraded:
        try:
            opcode, payload, _ = ws_read_frame(connection, rest, timeout=20)
            check(
                f"[{shape}] RFB frames flow: the display's own protocol greeting arrives",
                payload.startswith(b"RFB 003."),
                f"opcode {opcode}, first bytes {payload[:24]!r}",
            )
        except (TimeoutError, ConnectionError) as error:
            check(f"[{shape}] RFB frames flow", False, str(error))
    connection.close()

    print("\n== the credential the page holds, against the panel it frames ==")
    credential = shell.page_path.strip("/")
    status, _, _ = frame.get(
        "",
        {
            "Cookie": f"afui_session={credential}",
            "Authorization": f"Bearer {credential}",
            "X-Afui-Session": credential,
        },
    )
    afhttp_log = procs.log_of("afhttp-host")
    check(
        f"[{shape}] the page's own credential does not reach afhttp",
        status == 200 and credential not in afhttp_log,
        f"status {status}; the credential appears in afhttp's log: {credential in afhttp_log}",
    )

    print(f"\n== a real browser, looking at the shell: {shape} ==")
    browser_report(shell, frame, procs, debug_port)
    return shell


# ── The browser half ──


def cdp(connection, buffered, message_id, method, params=None, session=None):
    payload = {"id": message_id, "method": method, "params": params or {}}
    if session:
        payload["sessionId"] = session
    frame = json.dumps(payload).encode()
    mask = os.urandom(4)
    masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(frame))
    header = bytearray([0x81])
    if len(frame) < 126:
        header.append(0x80 | len(frame))
    elif len(frame) < 65536:
        header.append(0x80 | 126)
        header += len(frame).to_bytes(2, "big")
    else:
        header.append(0x80 | 127)
        header += len(frame).to_bytes(8, "big")
    connection.sendall(bytes(header) + mask + masked)
    return buffered


def browser_report(shell, frame, procs, debug_port):
    """Load the shell in a real Chromium, and the same panel unframed, and compare.

    The comparison is the whole point. Both go through the same reverse proxy to
    the same panel; the only difference is that one is inside the shell's frame
    and the other is an ordinary top-level page. Whatever only the first one
    suffers is caused by the framing, not by the proxying — which is exactly how
    the storage failure was isolated in the first place.

    The browser resolves the framed session's name itself. Nothing maps it: a
    name under `.localhost` is loopback to Chrome, Firefox and Safari by their
    own rule, and if that stopped being true this check would be the one to say
    so.
    """
    shape = shell.label
    profile = tempfile.mkdtemp(prefix="chrome-e2e-")
    procs.start(
        f"chromium-{debug_port}",
        [
            "chromium",
            "--headless=new",
            "--no-sandbox",
            "--disable-gpu",
            "--disable-dev-shm-usage",
            "--window-size=390,844",
            f"--user-data-dir={profile}",
            f"--remote-debugging-port={debug_port}",
            "about:blank",
        ],
    )
    if not wait_for_port(debug_port, timeout=60):
        check(f"[{shape}] a browser is available to look at the page", False, "no CDP port")
        return
    with urllib.request.urlopen(f"http://127.0.0.1:{debug_port}/json/version", timeout=10) as r:
        endpoint = json.load(r)["webSocketDebuggerUrl"]
    parsed = urllib.parse.urlsplit(endpoint)
    status, _, _, connection, rest = ws_open(parsed.hostname, parsed.port, parsed.path, timeout=30)
    if status != 101:
        check(f"[{shape}] a browser is available to look at the page", False, f"CDP status {status}")
        return

    browser = Cdp(connection, rest)
    browser.send("Target.setAutoAttach", {"autoAttach": True, "waitForDebuggerOnStart": False, "flatten": True})
    browser.pump(2)
    page = None
    for message in browser.messages:
        if message.get("method") == "Target.attachedToTarget":
            if message["params"]["targetInfo"]["type"] == "page":
                page = message["params"]["sessionId"]
    if page is None:
        check(
            f"[{shape}] a browser is available to look at the page", False, "no page target attached"
        )
        return
    browser.send("Runtime.enable", {}, page)
    browser.send("Page.enable", {}, page)
    browser.send("Log.enable", {}, page)
    # And again on the page itself, because the document that matters here is
    # not the page: a frame with an opaque origin is its own target, so without
    # this the only document that could be asked anything would be the shell.
    browser.send(
        "Target.setAutoAttach",
        {"autoAttach": True, "waitForDebuggerOnStart": False, "flatten": True},
        page,
    )

    slug = "own-origin" if shell.per_origin else "one-origin"
    framed = look(
        browser,
        page,
        shell.serve_url,
        frame.browser_url,
        f"the shell, with the panel framed ({shape})",
        f"takeover-in-workbench-framed-{slug}.png",
        settle=18,
    )
    unframed = look(
        browser,
        page,
        frame.browser_url,
        frame.browser_url,
        f"the same panel, unframed, top level ({shape})",
        f"takeover-in-workbench-unframed-{slug}.png",
        settle=18,
    )

    if shell.per_origin:
        check(
            f"[{shape}] the display client runs inside the shell's frame",
            framed.get("blocked") is False,
            framed.get("evidence", "no report from the frame"),
        )
        check(
            f"[{shape}] the framed client reaches its own browser storage",
            framed.get("storage") == "readable",
            f"storage: {framed.get('storage')!r}",
        )
        check(
            f"[{shape}] the framed client connects to the display",
            framed.get("connected") is True,
            f"client state {framed.get('rfb')!r}; #noVNC_status {framed.get('status')!r}; "
            f"canvases {framed.get('size')} are {framed.get('painted')!r}",
        )
    else:
        # The other shape's frame has no origin at all, so it has no storage —
        # asserted because it is a property of the sandbox attribute this shell
        # writes, and losing it would mean the isolation went with it. What the
        # client then does about it is the client's business and only noted:
        # this must not turn into a check that fails the day KasmVNC learns to
        # guard the read.
        check(
            f"[{shape}] an opaque origin denies the framed client browser storage",
            str(framed.get("storage", "")).startswith("SecurityError"),
            f"storage: {framed.get('storage')!r}",
        )
        note(
            "the client's own reaction to that: "
            + ("refused to run" if framed.get("blocked") else "carried on")
            + f"; connected: {framed.get('connected')}"
        )
    if shell.per_origin:
        check(
            f"[{shape}] the same panel, reverse-proxied but not framed, runs",
            unframed.get("blocked") is False,
            unframed.get("evidence", "no report from the page"),
        )
    else:
        # On a path the frame's address is on the shell's own origin, so opened
        # by hand it is sandboxed by the response exactly as the frame is: the
        # same opaque origin, the same missing storage.
        check(
            f"[{shape}] the same panel opened by hand stays sandboxed",
            str(unframed.get("storage", "")).startswith("SecurityError"),
            f"storage: {unframed.get('storage')!r}",
        )
    connection.close()
    shutil.rmtree(profile, ignore_errors=True)


class Cdp:
    """The smallest CDP client that can ask a frame what it thinks happened."""

    def __init__(self, connection, buffered):
        self.connection = connection
        self.buffered = buffered
        self.messages = []
        self.next_id = 100

    def send(self, method, params=None, session=None):
        self.next_id += 1
        payload = {"id": self.next_id, "method": method, "params": params or {}}
        if session:
            payload["sessionId"] = session
        frame = json.dumps(payload).encode()
        mask = os.urandom(4)
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(frame))
        header = bytearray([0x81])
        if len(frame) < 126:
            header.append(0x80 | len(frame))
        elif len(frame) < 65536:
            header.append(0x80 | 126)
            header += len(frame).to_bytes(2, "big")
        else:
            header.append(0x80 | 127)
            header += len(frame).to_bytes(8, "big")
        self.connection.sendall(bytes(header) + mask + masked)
        return self.next_id

    def pump(self, seconds):
        deadline = time.time() + seconds
        self.connection.settimeout(1.0)
        while time.time() < deadline:
            try:
                opcode, payload, self.buffered = ws_read_frame(self.connection, self.buffered, 1.0)
            except (TimeoutError, socket.timeout):
                continue
            except (ConnectionError, OSError):
                return
            if opcode == 1:
                try:
                    self.messages.append(json.loads(payload))
                except json.JSONDecodeError:
                    pass

    def reply_to(self, message_id):
        for message in self.messages:
            if message.get("id") == message_id and "result" in message:
                return message["result"]
        return None


# What every document is asked, framed or not. `try` around each because in an
# opaque origin the answer to some of them is an exception, and the exception is
# the finding.
#
# `painted` is the one that answers "is this thing actually connected", because
# the client's own status line is chrome that a narrow frame does not render:
# what says a remote framebuffer arrived is pixels on the canvas, and a canvas
# painted from a WebSocket is not tainted, so they can be read back.
PROBE = """JSON.stringify({
  url: location.href,
  origin: (function () { try { return location.origin } catch (e) { return 'threw ' + e.name } })(),
  storage: (function () { try { localStorage.getItem('probe'); return 'readable' }
                          catch (e) { return e.name + ': ' + e.message } })(),
  canvas: !!document.querySelector('canvas'),
  size: [].slice.call(document.querySelectorAll('canvas'))
          .map(function (c) { return c.width + 'x' + c.height }).join(','),
  painted: (function () {
    try {
      // Every canvas, not the first: this client keeps more than one, and the
      // one the framebuffer lands on is not the one at the top of the document.
      const all = [].slice.call(document.querySelectorAll('canvas'));
      if (!all.length) return 'no canvas';
      let best = 'blank';
      for (const c of all) {
        if (!c.width || !c.height) continue;
        const context = c.getContext('2d');
        if (!context) continue;
        const w = Math.min(c.width, 128), h = Math.min(c.height, 128);
        const d = context.getImageData(0, 0, w, h).data;
        const seen = new Set();
        for (let i = 0; i < d.length; i += 4) {
          seen.add(d[i] << 16 | d[i + 1] << 8 | d[i + 2]);
        }
        if (seen.size > 1) return 'painted';
        if (seen.size === 1 && [...seen][0] !== 0) best = 'one flat colour';
      }
      return best;
    } catch (e) { return e.name + ': ' + e.message }
  })(),
  status: (document.querySelector('#noVNC_status') || {}).textContent || '',
  rfb: (function () {
    // The client's own answer to "am I connected", for the case where its
    // status line is chrome a narrow frame never draws. Private, and this
    // harness is already this client's — `#noVNC_status` is no less so.
    try {
      const rfb = window.UI && window.UI.rfb;
      return rfb ? (rfb._rfbConnectionState || rfb.rfbConnectionState || 'unknown') : 'no client';
    } catch (e) { return e.name }
  })(),
  visible: (document.body ? document.body.innerText : '').replace(/\\s+/g, ' ').slice(0, 300)
})"""


def connected(report):
    """Whether this document's display client is actually up.

    Three ways of asking the same thing, because only one of them survives a
    narrow frame: the client's own state, the status line it writes when it has
    the room to draw one, and pixels having arrived on a canvas.
    """
    return (
        report.get("rfb") == "connected"
        or "Connected" in (report.get("status") or "")
        or report.get("painted") == "painted"
    )


def look(browser, page, url, panel_prefix, label, screenshot, settle):
    """Load one URL, ask every document in it the same questions, save a picture.

    Asked repeatedly rather than once. A VNC client that has not connected *yet*
    and one that cannot connect *at all* look identical in a single sample, and
    two of them starting at the same moment in a headless container is exactly
    the case where the difference is time. The loop stops as soon as the panel
    says it is connected or says it failed, so a working one costs one round.
    """
    print(f"\n      -- {label} --")
    before = len(browser.messages)
    browser.send("Page.navigate", {"url": url}, page)
    browser.pump(settle)

    reports = []
    for _ in range(8):
        # Every document the browser built, top and framed, recollected each
        # round because a frame may attach late. A framed panel is its own
        # target here, so asking only the page would be asking the shell about
        # itself — a check that passes because it looked at the wrong document.
        sessions = [(page, "page")]
        for message in browser.messages[before:]:
            if message.get("method") == "Target.attachedToTarget":
                info = message["params"]["targetInfo"]
                if info["type"] in ("iframe", "page") and message["params"]["sessionId"] != page:
                    sessions.append((message["params"]["sessionId"], info["type"]))
        for session, _kind in sessions[1:]:
            browser.send("Runtime.enable", {}, session)
        browser.pump(1)

        reports = []
        for session, kind in sessions:
            identifier = browser.send(
                "Runtime.evaluate", {"expression": PROBE, "returnByValue": True}, session
            )
            browser.pump(1)
            reply = browser.reply_to(identifier)
            value = (reply or {}).get("result", {}).get("value")
            if isinstance(value, str):
                try:
                    report = json.loads(value)
                    report["document"] = kind
                    reports.append(report)
                except json.JSONDecodeError:
                    pass
        settled = next((r for r in reports if r.get("url", "").startswith(panel_prefix)), None)
        if settled and (
            connected(settled)
            or "encountered an error" in (settled.get("visible") or "")
            or settled.get("storage") != "readable"
        ):
            break
        browser.pump(3)
    for report in reports:
        note(json.dumps(report))
    shot = browser.send("Page.captureScreenshot", {"format": "png"}, page)
    browser.pump(8)
    reply = browser.reply_to(shot)
    if reply and reply.get("data"):
        path = f"{WORKSPACE}/target/{screenshot}"
        with open(path, "wb") as handle:
            handle.write(base64.b64decode(reply["data"]))
        note(f"screenshot written to {path}")

    # The panel's own document is the one served from its proxy mount, wherever
    # that is in this shape.
    panel = next((r for r in reports if r.get("url", "").startswith(panel_prefix)), None)
    if panel is None:
        return {
            "evidence": f"no document under {panel_prefix} answered; "
            + json.dumps([r.get("url") for r in reports])
        }
    blocked = panel.get("storage") != "readable" or "encountered an error" in panel.get(
        "visible", ""
    )
    return {
        "blocked": bool(blocked),
        "connected": connected(panel),
        "storage": panel.get("storage"),
        "status": panel.get("status"),
        "canvas": panel.get("canvas"),
        "painted": panel.get("painted"),
        "rfb": panel.get("rfb"),
        "size": panel.get("size"),
        "evidence": json.dumps(panel, indent=2),
    }


if __name__ == "__main__":
    sys.exit(main())
