#!/usr/bin/env python3
"""
WebSocket correctness tests for afh.
Starts an HTTP test server and a WebSocket test server, pipes JSONL into afh.

Requires: pip install websockets
"""

import base64
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
from server import start_server
from ws_server import WS_BASE, WS_PORT, start_ws_server

AFH = os.path.join(os.path.dirname(__file__), "..", "target", "debug", "afhttp")
HTTP_BASE = "http://127.0.0.1:18080"


# ---------------------------------------------------------------------------
# Helpers (mirrors stress.py)
# ---------------------------------------------------------------------------


def run_afh(inputs, timeout_s=30):
    """Send JSONL lines to afh stdin (all at once), collect parsed output."""
    payload = "\n".join(inputs) + "\n"
    proc = subprocess.run(
        [AFH, "--pipe"],
        input=payload,
        capture_output=True,
        text=True,
        timeout=timeout_s,
    )
    return _parse_stdout(proc.stdout)


def run_afh_interactive(inputs_with_delays, timeout_s=30):
    """
    Send inputs with timed gaps (for tests that need to wait between commands).
    inputs_with_delays: list of (sleep_seconds_before_write, json_line) tuples.
    """
    proc = subprocess.Popen(
        [AFH, "--pipe"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    for delay, line_str in inputs_with_delays:
        if delay > 0:
            time.sleep(delay)
        try:
            proc.stdin.write((line_str + "\n").encode())
            proc.stdin.flush()
        except (BrokenPipeError, OSError):
            break
    try:
        proc.stdin.close()
    except (BrokenPipeError, OSError):
        pass
    try:
        proc.wait(timeout=timeout_s)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
    return _parse_stdout(proc.stdout.read().decode())


def _parse_stdout(text: str) -> list[dict]:
    lines = []
    for line in text.strip().split("\n"):
        line = line.strip()
        if line:
            try:
                lines.append(json.loads(line))
            except json.JSONDecodeError:
                lines.append({"_raw": line})
    return lines


def find(outputs, code, req_id):
    for o in outputs:
        if o.get("code") == code and o.get("id") == req_id:
            return o
    return None


def find_all(outputs, code, req_id):
    return [o for o in outputs if o.get("code") == code and o.get("id") == req_id]


# ---------------------------------------------------------------------------
# Test runner
# ---------------------------------------------------------------------------

passed = 0
failed = 0
errors = []


def test(name: str):
    def decorator(fn):
        def wrapper():
            global passed, failed
            try:
                fn()
                passed += 1
                print(f"  \033[32mPASS\033[0m {name}")
            except Exception as e:
                failed += 1
                errors.append((name, str(e)))
                print(f"  \033[31mFAIL\033[0m {name}: {e}")

        wrapper.__name__ = name
        return wrapper

    return decorator


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


@test("server push: receives N messages then chunk_end")
def test_ws_push():
    out = run_afh([
        json.dumps({"code": "request", "id": "ws1", "method": "GET",
                    "url": f"{WS_BASE}/ws/push/3",
                    "options": {"upgrade": "websocket"}}),
        json.dumps({"code": "close"}),
    ])
    start = find(out, "chunk_start", "ws1")
    assert start, f"no chunk_start, got codes: {[o.get('code') for o in out]}"
    assert start["status"] == 101

    chunks = find_all(out, "chunk_data", "ws1")
    assert len(chunks) == 3, f"expected 3 chunks, got {len(chunks)}: {chunks}"
    for i, chunk in enumerate(chunks):
        data = json.loads(chunk["data"])
        assert data["seq"] == i, f"chunk {i}: expected seq={i}, got {data}"

    end = find(out, "chunk_end", "ws1")
    assert end, "no chunk_end"
    assert end["trace"]["chunks"] == 3
    assert end["trace"]["http_version"] == "ws"


@test("handshake: chunk_start has status 101 and WebSocket headers")
def test_ws_handshake_headers():
    out = run_afh([
        json.dumps({"code": "request", "id": "ws1", "method": "GET",
                    "url": f"{WS_BASE}/ws/push/1",
                    "options": {"upgrade": "websocket"}}),
        json.dumps({"code": "close"}),
    ])
    start = find(out, "chunk_start", "ws1")
    assert start, "no chunk_start"
    assert start["status"] == 101
    headers = start["headers"]
    assert "upgrade" in headers, f"missing upgrade header, got: {list(headers)}"
    assert "sec-websocket-accept" in headers, "missing sec-websocket-accept"


@test("echo: agent sends text messages, server echoes back")
def test_ws_echo():
    out = run_afh_interactive([
        (0.0,  json.dumps({"code": "request", "id": "ws1", "method": "GET",
                            "url": f"{WS_BASE}/ws/echo",
                            "options": {"upgrade": "websocket"}})),
        (0.2,  json.dumps({"code": "send", "id": "ws1", "data": "hello"})),
        (0.05, json.dumps({"code": "send", "id": "ws1", "data": "world"})),
        (0.15, json.dumps({"code": "cancel", "id": "ws1"})),
    ])
    start = find(out, "chunk_start", "ws1")
    assert start and start["status"] == 101, "no chunk_start with 101"

    chunks = find_all(out, "chunk_data", "ws1")
    assert len(chunks) == 2, f"expected 2 echo chunks, got {len(chunks)}: {chunks}"
    assert chunks[0]["data"] == "hello", f"first echo: {chunks[0]}"
    assert chunks[1]["data"] == "world", f"second echo: {chunks[1]}"

    end = find(out, "chunk_end", "ws1")
    assert end, "no chunk_end after cancel"


@test("send: JSON object is serialized to text frame")
def test_ws_send_json_object():
    out = run_afh_interactive([
        (0.0,  json.dumps({"code": "request", "id": "ws1", "method": "GET",
                            "url": f"{WS_BASE}/ws/echo",
                            "options": {"upgrade": "websocket"}})),
        (0.2,  json.dumps({"code": "send", "id": "ws1",
                            "data": {"type": "subscribe", "channel": "prices"}})),
        (0.15, json.dumps({"code": "cancel", "id": "ws1"})),
    ])
    chunks = find_all(out, "chunk_data", "ws1")
    assert len(chunks) == 1, f"expected 1 chunk, got {len(chunks)}: {chunks}"
    echoed = json.loads(chunks[0]["data"])
    assert echoed == {"type": "subscribe", "channel": "prices"}, f"echo mismatch: {echoed}"


@test("binary rx: server binary frame received as data_base64")
def test_ws_binary_rx():
    out = run_afh([
        json.dumps({"code": "request", "id": "ws1", "method": "GET",
                    "url": f"{WS_BASE}/ws/binary",
                    "options": {"upgrade": "websocket"}}),
        json.dumps({"code": "close"}),
    ])
    chunks = find_all(out, "chunk_data", "ws1")
    assert len(chunks) == 1, f"expected 1 binary chunk, got {len(chunks)}"
    chunk = chunks[0]
    assert "data_base64" in chunk and chunk["data_base64"], "expected data_base64"
    assert chunk.get("data") is None, "data should be absent for binary frame"
    decoded = base64.b64decode(chunk["data_base64"])
    assert decoded == bytes(range(16)), f"wrong binary content: {decoded[:8]!r}"


@test("binary tx: data_base64 sends binary frame, echoed as data_base64")
def test_ws_binary_tx():
    payload = bytes([0xDE, 0xAD, 0xBE, 0xEF])
    b64 = base64.b64encode(payload).decode()
    out = run_afh_interactive([
        (0.0,  json.dumps({"code": "request", "id": "ws1", "method": "GET",
                            "url": f"{WS_BASE}/ws/echo",
                            "options": {"upgrade": "websocket"}})),
        (0.2,  json.dumps({"code": "send", "id": "ws1", "data_base64": b64})),
        (0.15, json.dumps({"code": "cancel", "id": "ws1"})),
    ])
    chunks = find_all(out, "chunk_data", "ws1")
    assert len(chunks) == 1, f"expected 1 binary echo, got {len(chunks)}"
    assert "data_base64" in chunks[0], "expected data_base64 for binary echo"
    echoed = base64.b64decode(chunks[0]["data_base64"])
    assert echoed == payload, f"binary echo mismatch: {echoed!r}"


@test("headers: custom headers forwarded in WebSocket handshake")
def test_ws_custom_headers():
    out = run_afh([
        json.dumps({"code": "request", "id": "ws1", "method": "GET",
                    "url": f"{WS_BASE}/ws/headers",
                    "headers": {"X-Agent-Id": "test-agent-42"},
                    "options": {"upgrade": "websocket"}}),
        json.dumps({"code": "close"}),
    ])
    chunks = find_all(out, "chunk_data", "ws1")
    assert len(chunks) == 1, f"expected 1 headers-echo chunk, got {len(chunks)}"
    echoed = json.loads(chunks[0]["data"])
    # Keys may be case-normalised by the server
    found = any(v == "test-agent-42"
                for k, v in echoed.items()
                if k.lower() == "x-agent-id")
    assert found, f"x-agent-id not in echoed headers: {echoed}"


@test("cancel: sends graceful close frame, produces chunk_end (no error)")
def test_ws_cancel():
    out = run_afh_interactive([
        (0.0, json.dumps({"code": "request", "id": "ws1", "method": "GET",
                           "url": f"{WS_BASE}/ws/echo",
                           "options": {"upgrade": "websocket"}})),
        (0.2, json.dumps({"code": "cancel", "id": "ws1"})),
    ])
    start = find(out, "chunk_start", "ws1")
    assert start and start["status"] == 101, "no chunk_start"
    end = find(out, "chunk_end", "ws1")
    assert end, "no chunk_end after cancel"
    # cancel on WebSocket is graceful — no error output for that id
    ws_errors = [o for o in out if o.get("code") == "error" and o.get("id") == "ws1"]
    assert not ws_errors, f"unexpected error after cancel: {ws_errors}"


@test("send to unknown id returns invalid_request error")
def test_ws_send_unknown_id():
    out = run_afh([
        json.dumps({"code": "send", "id": "no-such-ws", "data": "hello"}),
        json.dumps({"code": "close"}),
    ])
    err = find(out, "error", "no-such-ws")
    assert err, f"expected error, got: {[o.get('code') for o in out]}"
    assert err["error_code"] == "invalid_request"


@test("concurrent: multiple WebSocket connections in parallel")
def test_ws_concurrent():
    out = run_afh([
        json.dumps({"code": "request", "id": "ws-a", "method": "GET",
                    "url": f"{WS_BASE}/ws/push/2",
                    "options": {"upgrade": "websocket"}}),
        json.dumps({"code": "request", "id": "ws-b", "method": "GET",
                    "url": f"{WS_BASE}/ws/push/2",
                    "options": {"upgrade": "websocket"}}),
        json.dumps({"code": "close"}),
    ])
    for ws_id in ("ws-a", "ws-b"):
        start = find(out, "chunk_start", ws_id)
        assert start, f"no chunk_start for {ws_id}"
        assert start["status"] == 101
        chunks = find_all(out, "chunk_data", ws_id)
        assert len(chunks) == 2, f"{ws_id}: expected 2 chunks, got {len(chunks)}"
        end = find(out, "chunk_end", ws_id)
        assert end, f"no chunk_end for {ws_id}"


@test("concurrent: WebSocket and HTTP requests coexist")
def test_ws_and_http():
    out = run_afh([
        json.dumps({"code": "request", "id": "ws1", "method": "GET",
                    "url": f"{WS_BASE}/ws/push/2",
                    "options": {"upgrade": "websocket"}}),
        json.dumps({"code": "request", "id": "http1", "method": "GET",
                    "url": f"{HTTP_BASE}/fast"}),
        json.dumps({"code": "close"}),
    ])
    ws_start = find(out, "chunk_start", "ws1")
    assert ws_start and ws_start["status"] == 101, "no ws chunk_start"
    http_resp = find(out, "response", "http1")
    assert http_resp and http_resp["status"] == 200, "no http response"
    ws_chunks = find_all(out, "chunk_data", "ws1")
    assert len(ws_chunks) == 2, f"expected 2 ws chunks, got {len(ws_chunks)}"
    ws_end = find(out, "chunk_end", "ws1")
    assert ws_end, "no ws chunk_end"


@test("shutdown: afh close command drains open WebSocket connections")
def test_ws_shutdown_drains():
    # Echo server keeps connection open; close command should flush it
    out = run_afh_interactive([
        (0.0, json.dumps({"code": "request", "id": "ws1", "method": "GET",
                           "url": f"{WS_BASE}/ws/echo",
                           "options": {"upgrade": "websocket"}})),
        (0.2, json.dumps({"code": "close"})),
    ])
    end = find(out, "chunk_end", "ws1")
    assert end, f"no chunk_end on shutdown, got: {[o.get('code') for o in out]}"
    close_msg = next((o for o in out if o.get("code") == "close"), None)
    assert close_msg, "no process-level close acknowledgement"


@test("connect error: bad host returns error with error_code")
def test_ws_connect_error():
    out = run_afh([
        json.dumps({"code": "request", "id": "ws1", "method": "GET",
                    "url": "ws://127.0.0.1:1/ws/echo",
                    "options": {"upgrade": "websocket"}}),
        json.dumps({"code": "close"}),
    ])
    err = find(out, "error", "ws1")
    assert err, f"expected error, got: {[o.get('code') for o in out]}"
    assert err["error_code"] in ("connect_refused", "connect_timeout", "dns_failed"), \
        f"unexpected error_code: {err['error_code']}"


@test("websocket_tls_config_ignored log is gated by request log category")
def test_ws_tls_warning_log_gated():
    out_without_request_log = run_afh([
        json.dumps({"code": "config", "tls": {"insecure": True}}),
        json.dumps({"code": "request", "id": "ws1", "method": "GET",
                    "url": f"{WS_BASE}/ws/push/1",
                    "options": {"upgrade": "websocket"}}),
        json.dumps({"code": "close"}),
    ])
    logs_without = [
        o for o in out_without_request_log
        if o.get("code") == "log" and o.get("event") == "websocket_tls_config_ignored"
    ]
    assert not logs_without, f"unexpected tls warning log without request category: {logs_without}"

    out_with_request_log = run_afh([
        json.dumps({"code": "config", "tls": {"insecure": True}, "log": ["request"]}),
        json.dumps({"code": "request", "id": "ws1", "method": "GET",
                    "url": f"{WS_BASE}/ws/push/1",
                    "options": {"upgrade": "websocket"}}),
        json.dumps({"code": "close"}),
    ])
    logs_with = [
        o for o in out_with_request_log
        if o.get("code") == "log" and o.get("event") == "websocket_tls_config_ignored"
    ]
    assert logs_with, f"expected tls warning log when request category enabled: {out_with_request_log}"


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main():
    print("Building afh...")
    build = subprocess.run(
        ["cargo", "build"],
        cwd=os.path.join(os.path.dirname(__file__), ".."),
        capture_output=True,
    )
    if build.returncode != 0:
        print("Build failed:", build.stderr.decode(), file=sys.stderr)
        sys.exit(1)
    print("Build OK")

    print("Starting servers...")
    http_server = start_server(18080)
    start_ws_server(WS_PORT)
    time.sleep(0.1)
    print("Servers ready\n")

    tests = [
        test_ws_push,
        test_ws_handshake_headers,
        test_ws_echo,
        test_ws_send_json_object,
        test_ws_binary_rx,
        test_ws_binary_tx,
        test_ws_custom_headers,
        test_ws_cancel,
        test_ws_send_unknown_id,
        test_ws_concurrent,
        test_ws_and_http,
        test_ws_shutdown_drains,
        test_ws_connect_error,
        test_ws_tls_warning_log_gated,
    ]

    print(f"Running {len(tests)} WebSocket tests...\n")
    for t in tests:
        t()

    print(f"\n{'='*60}")
    print(f"  {passed} passed, {failed} failed, {passed + failed} total")
    if errors:
        print("\n  Failures:")
        for name, msg in errors:
            print(f"    {name}: {msg}")
    print(f"{'='*60}")

    http_server.shutdown()
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    import subprocess  # noqa: F811 (already imported above, needed for main)
    main()
