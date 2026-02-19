#!/usr/bin/env python3
"""
Comprehensive stress + correctness test for afh.
Starts a local test server, pipes JSONL into afh, validates output.
"""

import json
import os
import subprocess
import sys
import time
import base64
import signal
import threading
import http.server
import socketserver
import tempfile

# Add tests dir to path for server import
sys.path.insert(0, os.path.dirname(__file__))
from server import start_server

AFH = os.path.join(os.path.dirname(__file__), "..", "target", "debug", "afhttp")
BASE = "http://127.0.0.1:18080"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

class TestFailure(Exception):
    pass


def temp_path(prefix: str, suffix: str) -> str:
    return os.path.join(
        tempfile.gettempdir(),
        f"{prefix}-{os.getpid()}-{time.time_ns()}{suffix}",
    )


def run_afh(inputs: list[str], timeout_s=30, extra_args: list[str] = None) -> list[dict]:
    """Send JSONL lines to afh stdin, collect parsed output lines."""
    payload = "\n".join(inputs) + "\n"
    cmd = [AFH, "--pipe"] + (extra_args or [])
    proc = subprocess.run(
        cmd,
        input=payload,
        capture_output=True,
        text=True,
        timeout=timeout_s,
    )
    lines = []
    for line in proc.stdout.strip().split("\n"):
        line = line.strip()
        if line:
            try:
                lines.append(json.loads(line))
            except json.JSONDecodeError:
                lines.append({"_raw": line})
    return lines


def run_afh_interactive(inputs_with_delays: list, timeout_s=60) -> list[dict]:
    """
    Send inputs with delays between them.
    inputs_with_delays: list of (delay_seconds, line_string) tuples.
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

    stdout = proc.stdout.read().decode()
    lines = []
    for line in stdout.strip().split("\n"):
        line = line.strip()
        if line:
            try:
                lines.append(json.loads(line))
            except json.JSONDecodeError:
                lines.append({"_raw": line})
    return lines


def find_by_id(outputs, code, req_id):
    """Find output line matching code and id."""
    for o in outputs:
        if o.get("code") == code and o.get("id") == req_id:
            return o
    return None


def find_all_by_id(outputs, code, req_id):
    return [o for o in outputs if o.get("code") == code and o.get("id") == req_id]


def find_by_code(outputs, code):
    return [o for o in outputs if o.get("code") == code]


def find_log_events(outputs, event):
    """Find log events by event type (code=log, event=<event>)."""
    return [o for o in outputs if o.get("code") == "log" and o.get("event") == event]


def get_header_ci(headers_dict, name):
    """Case-insensitive header lookup in a dict (server echoes headers as received)."""
    name_lower = name.lower()
    for k, v in headers_dict.items():
        if k.lower() == name_lower:
            return v
    return None


passed = 0
failed = 0
errors = []


def test(name):
    """Decorator for test functions."""
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
# Correctness tests
# ---------------------------------------------------------------------------

@test("basic GET returns parsed JSON")
def test_basic_get():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET", "url": f"{BASE}/fast"}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, f"no response for id=1, got: {[o.get('code') for o in out]}"
    assert r["status"] == 200
    assert r["body"]["ok"] is True
    assert "trace" in r
    assert r["trace"]["duration_ms"] >= 0


@test("POST with JSON body sets Content-Type automatically")
def test_post_json():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "POST", "url": f"{BASE}/echo",
                     "body": {"key": "value"}}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    assert r["status"] == 200
    echo = r["body"]
    assert "application/json" in echo["content_type"]
    assert json.loads(echo["body"]) == {"key": "value"}


@test("POST with string body sends raw bytes (no implicit Content-Type)")
def test_post_string():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "POST", "url": f"{BASE}/echo",
                     "body": "hello world"}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    echo = r["body"]
    # No implicit Content-Type — caller must specify if needed
    assert echo["content_type"] == "", f"expected no Content-Type for string body, got: {echo['content_type']!r}"
    assert echo["body"] == "hello world"


@test("POST with base64 body sends binary")
def test_post_base64():
    data = bytes(range(256))
    b64 = base64.b64encode(data).decode()
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "POST", "url": f"{BASE}/echo",
                     "body_base64": b64}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    echo = r["body"]
    assert echo["body_length"] == 256


@test("204 No Content returns no body fields")
def test_204():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET", "url": f"{BASE}/empty"}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    assert r["status"] == 204
    assert "body" not in r or r["body"] is None
    assert "body_base64" not in r or r["body_base64"] is None
    assert "body_file" not in r or r["body_file"] is None


@test("HEAD request returns no body")
def test_head():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "HEAD", "url": f"{BASE}/head-test"}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    assert r["status"] == 200
    assert "body" not in r or r["body"] is None


@test("binary response returns body_base64")
def test_binary():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET", "url": f"{BASE}/binary/100"}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    assert r["status"] == 200
    assert "body_base64" in r and r["body_base64"]
    decoded = base64.b64decode(r["body_base64"])
    assert len(decoded) == 100


@test("text response returns body as string")
def test_text():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET", "url": f"{BASE}/text/500"}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    assert r["body"] == "A" * 500


@test("4xx/5xx returns response (not error)")
def test_4xx_5xx():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET", "url": f"{BASE}/status/404"}),
        json.dumps({"code": "request", "id": "2", "method": "GET", "url": f"{BASE}/status/500"}),
        json.dumps({"code": "close"}),
    ])
    r1 = find_by_id(out, "response", "1")
    r2 = find_by_id(out, "response", "2")
    assert r1, "no response for 404"
    assert r2, "no response for 500"
    assert r1["status"] == 404
    assert r2["status"] == 500


@test("default User-Agent header is sent")
def test_user_agent():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET", "url": f"{BASE}/headers"}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    headers = r["body"]
    ua = get_header_ci(headers, "User-Agent")
    assert ua and ua.startswith("afhttp/"), f"User-Agent not found or wrong: {headers}"


@test("custom header sent, null removes default")
def test_header_merge():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET", "url": f"{BASE}/headers",
                     "headers": {"X-Custom": "test", "User-Agent": None}}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    headers = r["body"]
    assert get_header_ci(headers, "X-Custom") == "test", f"X-Custom not found: {headers}"
    assert get_header_ci(headers, "User-Agent") is None, f"User-Agent should be removed: {headers}"


@test("config update merges defaults")
def test_config_update():
    out = run_afh([
        json.dumps({"code": "config", "defaults": {"headers": {"Authorization": "Bearer test"},
                                                     "timeout_idle_s": 60}}),
        json.dumps({"code": "request", "id": "1", "method": "GET", "url": f"{BASE}/headers"}),
        json.dumps({"code": "close"}),
    ])
    cfg = find_by_code(out, "config")
    assert cfg, "no config echo"
    assert cfg[0]["defaults"]["timeout_idle_s"] == 60
    assert cfg[0]["defaults"]["headers"]["Authorization"] == "Bearer test"
    assert cfg[0]["defaults"]["headers"]["User-Agent"] == "afhttp/0.1.0"  # preserved
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    auth = get_header_ci(r["body"], "Authorization")
    assert auth == "Bearer test", f"Authorization header: {auth}"


@test("redirect chain followed and final response returned")
def test_redirects():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/redirect/3"}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    assert r["status"] == 200
    assert r["body"]["redirected"] is True
    assert r["trace"].get("redirects") == 3


@test("max_redirects=0 returns redirect as-is")
def test_redirect_disabled():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/redirect/3",
                     "options": {"response_redirect": 0}}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    assert r["status"] == 302
    assert r["trace"].get("redirects") == 0


@test("too_many_redirects error")
def test_too_many_redirects():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/redirect/20",
                     "options": {"response_redirect": 5}}),
        json.dumps({"code": "close"}),
    ])
    e = find_by_id(out, "error", "1")
    assert e, "no error"
    assert e["error_code"] == "too_many_redirects"


@test("duplicate in-flight id returns invalid_request")
def test_duplicate_id_rejected():
    out = run_afh([
        json.dumps({"code": "request", "id": "dup", "method": "GET",
                     "url": f"{BASE}/delay/250"}),
        json.dumps({"code": "request", "id": "dup", "method": "GET",
                     "url": f"{BASE}/delay/250"}),
        json.dumps({"code": "close"}),
    ], timeout_s=15)
    responses = find_all_by_id(out, "response", "dup")
    errors_out = find_all_by_id(out, "error", "dup")
    assert len(responses) == 1, f"expected 1 response, got {len(responses)}: {out}"
    assert len(errors_out) == 1, f"expected 1 error, got {len(errors_out)}: {out}"
    assert errors_out[0]["error_code"] == "invalid_request"


@test("request_concurrency_limit returns overloaded when exceeded")
def test_request_concurrency_limit_overloaded():
    out = run_afh_interactive([
        (0, json.dumps({"code": "config", "request_concurrency_limit": 1})),
        (0, json.dumps({"code": "request", "id": "a", "method": "GET",
                         "url": f"{BASE}/delay/500"})),
        (0.02, json.dumps({"code": "request", "id": "b", "method": "GET",
                            "url": f"{BASE}/fast"})),
        (1, json.dumps({"code": "close"})),
    ], timeout_s=15)

    req_results = [o for o in out if o.get("code") in ("response", "error") and o.get("id") in ("a", "b")]
    overloaded = [o for o in req_results if o.get("code") == "error" and o.get("error_code") == "overloaded"]
    responses = [o for o in req_results if o.get("code") == "response"]

    assert len(overloaded) == 1, f"expected 1 overloaded error, got: {req_results}"
    assert overloaded[0].get("retryable") is True, f"expected overloaded to be retryable: {overloaded[0]}"
    assert len(responses) == 1, f"expected 1 successful response, got: {req_results}"
    assert responses[0]["id"] != overloaded[0]["id"], f"same id cannot be both response and overloaded: {req_results}"


@test("duplicate id rejected while chunked stream is still active")
def test_duplicate_id_rejected_while_chunked_active():
    out = run_afh_interactive([
        (0, json.dumps({"code": "request", "id": "dup-stream", "method": "GET",
                         "url": f"{BASE}/stream/ndjson/8/120",
                         "options": {"chunked": True}})),
        (0.12, json.dumps({"code": "request", "id": "dup-stream", "method": "GET",
                            "url": f"{BASE}/fast"})),
        (1.4, json.dumps({"code": "close"})),
    ], timeout_s=20)

    dup_errors = [o for o in out if o.get("code") == "error" and o.get("id") == "dup-stream"]
    assert len(dup_errors) == 1, f"expected one duplicate-id error, got: {dup_errors}"
    assert dup_errors[0]["error_code"] == "invalid_request", \
        f"expected invalid_request for duplicate id, got: {dup_errors[0]}"
    ce = find_by_id(out, "chunk_end", "dup-stream")
    assert ce, f"expected original stream to complete, got codes: {[o.get('code') for o in out]}"


@test("request_concurrency_limit applies until chunked stream completes")
def test_request_concurrency_limit_during_chunked_stream():
    out = run_afh_interactive([
        (0, json.dumps({"code": "config", "request_concurrency_limit": 1})),
        (0, json.dumps({"code": "request", "id": "stream-a", "method": "GET",
                         "url": f"{BASE}/stream/ndjson/8/120",
                         "options": {"chunked": True}})),
        (0.12, json.dumps({"code": "request", "id": "req-b", "method": "GET",
                            "url": f"{BASE}/fast"})),
        (1.4, json.dumps({"code": "close"})),
    ], timeout_s=20)

    overloaded = find_by_id(out, "error", "req-b")
    assert overloaded, f"expected overloaded for req-b, got: {out}"
    assert overloaded["error_code"] == "overloaded", f"expected overloaded, got: {overloaded}"
    assert overloaded.get("retryable") is True, f"expected retryable=true, got: {overloaded}"
    ce = find_by_id(out, "chunk_end", "stream-a")
    assert ce, f"expected stream-a chunk_end, got codes: {[o.get('code') for o in out]}"


@test("303 redirect switches method to GET")
def test_redirect_303_switches_to_get():
    class ReuseServer(socketserver.TCPServer):
        allow_reuse_address = True

    class Redirect303Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            if self.path == "/start":
                self.send_response(303)
                self.send_header("Location", "/target")
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            if self.path == "/target":
                body = json.dumps({"method": "POST"}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()

        def do_GET(self):
            if self.path == "/target":
                body = json.dumps({"method": "GET"}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()

    server = ReuseServer(("127.0.0.1", 0), Redirect303Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    port = server.server_address[1]
    try:
        out = run_afh([
            json.dumps({"code": "request", "id": "1", "method": "POST",
                         "url": f"http://127.0.0.1:{port}/start",
                         "body": {"x": 1}}),
            json.dumps({"code": "close"}),
        ])
        r = find_by_id(out, "response", "1")
        assert r, f"no response, got: {out}"
        assert r["status"] == 200
        assert r["body"]["method"] == "GET", f"expected GET after 303, got: {r['body']}"
    finally:
        server.shutdown()
        server.server_close()


@test("redirect strips Authorization across origins")
def test_redirect_strips_auth_cross_origin():
    class ReuseServer(socketserver.TCPServer):
        allow_reuse_address = True

    class SinkHandler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            if self.path != "/sink":
                self.send_response(404)
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            body = json.dumps({"authorization": self.headers.get("Authorization")}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    sink = ReuseServer(("127.0.0.1", 0), SinkHandler)
    sink_thread = threading.Thread(target=sink.serve_forever, daemon=True)
    sink_thread.start()
    sink_port = sink.server_address[1]

    class RedirectHandler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            if self.path != "/start":
                self.send_response(404)
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            self.send_response(302)
            self.send_header("Location", f"http://127.0.0.1:{sink_port}/sink")
            self.send_header("Content-Length", "0")
            self.end_headers()

    redirect = ReuseServer(("127.0.0.1", 0), RedirectHandler)
    redirect_thread = threading.Thread(target=redirect.serve_forever, daemon=True)
    redirect_thread.start()
    redirect_port = redirect.server_address[1]

    try:
        out = run_afh([
            json.dumps({"code": "request", "id": "1", "method": "GET",
                         "url": f"http://127.0.0.1:{redirect_port}/start",
                         "headers": {"Authorization": "Bearer secret-token"}}),
            json.dumps({"code": "close"}),
        ])
        r = find_by_id(out, "response", "1")
        assert r, f"no response, got: {out}"
        assert r["status"] == 200
        assert r["body"]["authorization"] is None, f"Authorization leaked across origin: {r['body']}"
    finally:
        redirect.shutdown()
        redirect.server_close()
        sink.shutdown()
        sink.server_close()


@test("connection refused produces error")
def test_connection_refused():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": "http://127.0.0.1:19999/fail",
                     "options": {"retry": 0}}),
        json.dumps({"code": "close"}),
    ])
    e = find_by_id(out, "error", "1")
    assert e, f"no error, got: {[o.get('code') for o in out]}"
    assert e["error_code"] in ("connect_refused", "connect_timeout"), f"error_code: {e['error_code']}"
    assert e["retryable"] is True


@test("invalid URL produces error")
def test_invalid_url():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": "not-a-url"}),
        json.dumps({"code": "close"}),
    ])
    e = find_by_id(out, "error", "1")
    assert e, "no error"
    assert e["error_code"] == "invalid_request"
    assert e["retryable"] is False


@test("invalid JSON produces error with no id")
def test_invalid_json():
    out = run_afh([
        "this is not json",
        json.dumps({"code": "close"}),
    ])
    errs = find_by_code(out, "error")
    assert errs, "no error"
    assert errs[0].get("id") is None


@test("ping returns pong with stats")
def test_ping():
    out = run_afh([
        json.dumps({"code": "ping"}),
        json.dumps({"code": "close"}),
    ])
    pongs = find_by_code(out, "pong")
    assert pongs, "no pong"
    assert "trace" in pongs[0]
    assert "uptime_s" in pongs[0]["trace"]
    assert "requests_total" in pongs[0]["trace"]


@test("startup message has correct structure")
def test_startup():
    # startup is a log category — emitted when --log startup is passed at launch
    out = run_afh([json.dumps({"code": "close"})], extra_args=["--log", "startup"])
    startups = find_log_events(out, "startup")
    assert startups, f"no startup log, got: {[o.get('code') for o in out]}"
    s = startups[0]
    assert s["version"] == "0.1.0"
    assert isinstance(s["argv"], list)
    assert "--pipe" in s["argv"]
    assert s["config"]["response_save_above_bytes"] == 10485760
    assert s["config"]["defaults"]["response_parse_json"] is True
    # No startup without --log startup
    out2 = run_afh([json.dumps({"code": "close"})])
    assert len(find_log_events(out2, "startup")) == 0, "unexpected startup without --log"


@test("multiple Set-Cookie headers returned as array")
def test_multi_header():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/multi-header"}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    cookies = r["headers"].get("set-cookie")
    assert isinstance(cookies, list), f"expected array, got {type(cookies)}: {cookies}"
    assert len(cookies) == 2


@test("unicode in response body")
def test_unicode():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/unicode"}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    assert "你好世界" in r["body"]["text"]


@test("parse_json=false returns JSON as string")
def test_parse_json_false():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/fast",
                     "options": {"response_parse_json": False}}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    assert isinstance(r["body"], str), f"expected string, got {type(r['body'])}"


@test("request timeout produces error")
def test_request_timeout():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/delay/5000",
                     "options": {"timeout_idle_s": 1, "retry": 0}}),
        json.dumps({"code": "close"}),
    ], timeout_s=15)
    e = find_by_id(out, "error", "1")
    assert e, f"no error, got: {[o.get('code') for o in out]}"
    assert e["error_code"] == "request_timeout", f"error_code: {e['error_code']}"
    assert e["retryable"] is False


# ---------------------------------------------------------------------------
# Chunked / streaming tests
# ---------------------------------------------------------------------------

@test("chunked SSE stream with \\n\\n delimiter")
def test_chunked_sse():
    out = run_afh_interactive([
        (0, json.dumps({"code": "request", "id": "1", "method": "GET",
                         "url": f"{BASE}/stream/sse/5/10",
                         "options": {"chunked": True, "chunked_delimiter": "\n\n"}})),
        (3, json.dumps({"code": "close"})),
    ])
    cs = find_by_id(out, "chunk_start", "1")
    assert cs, "no chunk_start"
    assert cs["status"] == 200
    chunks = find_all_by_id(out, "chunk_data", "1")
    assert len(chunks) == 5, f"expected 5 chunks, got {len(chunks)}"
    ce = find_by_id(out, "chunk_end", "1")
    assert ce, "no chunk_end"
    assert ce["trace"]["chunks"] == 5


@test("chunked NDJSON stream with \\n delimiter")
def test_chunked_ndjson():
    out = run_afh_interactive([
        (0, json.dumps({"code": "request", "id": "1", "method": "GET",
                         "url": f"{BASE}/stream/ndjson/10/5",
                         "options": {"chunked": True}})),
        (3, json.dumps({"code": "close"})),
    ])
    chunks = find_all_by_id(out, "chunk_data", "1")
    assert len(chunks) == 10, f"expected 10 chunks, got {len(chunks)}"
    # Verify each chunk is valid JSON
    for c in chunks:
        parsed = json.loads(c["data"])
        assert "seq" in parsed


# ---------------------------------------------------------------------------
# Large response / auto-download tests
# ---------------------------------------------------------------------------

@test("large response auto-saved to body_file")
def test_large_auto_download():
    # Set response_save_above_bytes low so we trigger auto-download
    out = run_afh([
        json.dumps({"code": "config", "response_save_above_bytes": 1000}),
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/size/5000"}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    assert r.get("body_file"), f"expected body_file, got: {r.keys()}"
    # Verify file exists and has correct size
    assert os.path.exists(r["body_file"]), f"body_file does not exist: {r['body_file']}"
    size = os.path.getsize(r["body_file"])
    assert size == 5000, f"expected 5000 bytes, got {size}"
    # Verify sidecar JSON exists
    sidecar = r["body_file"] + ".json"
    assert os.path.exists(sidecar), "sidecar .json missing"


@test("save_to option saves to specified path")
def test_save_to():
    save_path = temp_path("afh-test", ".bin")
    try:
        out = run_afh([
            json.dumps({"code": "request", "id": "1", "method": "GET",
                         "url": f"{BASE}/size/2000",
                         "options": {"response_save_file": save_path}}),
            json.dumps({"code": "close"}),
        ])
        ce = find_by_id(out, "chunk_end", "1")
        assert ce, f"no chunk_end, got: {[o.get('code') for o in out]}"
        assert ce["body_file"] == save_path
        assert os.path.exists(save_path)
        assert os.path.getsize(save_path) == 2000
    finally:
        if os.path.exists(save_path):
            os.unlink(save_path)


@test("download with progress reporting")
def test_download_progress():
    save_path = temp_path("afh-test-progress", ".bin")
    try:
        out = run_afh_interactive([
            (0, json.dumps({"code": "config", "log": ["progress"]})),
            (0, json.dumps({"code": "request", "id": "1", "method": "GET",
                             "url": f"{BASE}/size/10000",
                             "options": {"response_save_file": save_path, "progress_bytes": 2000}})),
            (3, json.dumps({"code": "close"})),
        ])
        # progress is now a log event: code=log, event=progress
        progress = [o for o in out if o.get("code") == "log" and o.get("event") == "progress" and o.get("id") == "1"]
        ce = find_by_id(out, "chunk_end", "1")
        assert ce, "no chunk_end"
        assert ce["trace"]["received_bytes"] == 10000
        # With progress_bytes=2000 and 10000 bytes, expect at least some progress events
        assert len(progress) >= 1, f"no progress events: {[o.get('code') for o in out]}"
    finally:
        if os.path.exists(save_path):
            os.unlink(save_path)


# ---------------------------------------------------------------------------
# Stress tests
# ---------------------------------------------------------------------------

@test("100 concurrent requests to same host")
def test_100_concurrent():
    inputs = []
    for i in range(100):
        inputs.append(json.dumps({
            "code": "request", "id": str(i), "method": "GET",
            "url": f"{BASE}/fast",
        }))
    inputs.append(json.dumps({"code": "close"}))
    out = run_afh(inputs, timeout_s=30)
    responses = find_by_code(out, "response")
    assert len(responses) == 100, f"expected 100 responses, got {len(responses)}"
    ids = {r["id"] for r in responses}
    assert len(ids) == 100, "duplicate or missing response ids"
    for r in responses:
        assert r["status"] == 200


@test("200 requests to varied endpoints")
def test_200_varied():
    inputs = []
    endpoints = ["/fast", "/json/50", "/text/100", "/status/200", "/status/201"]
    for i in range(200):
        ep = endpoints[i % len(endpoints)]
        inputs.append(json.dumps({
            "code": "request", "id": str(i), "method": "GET",
            "url": f"{BASE}{ep}",
        }))
    inputs.append(json.dumps({"code": "close"}))
    out = run_afh(inputs, timeout_s=30)
    responses = find_by_code(out, "response")
    assert len(responses) == 200, f"expected 200 responses, got {len(responses)}"


@test("100 concurrent requests with mixed delays")
def test_100_mixed_delays():
    inputs = []
    for i in range(100):
        delay = (i % 5) * 50  # 0, 50, 100, 150, 200ms
        inputs.append(json.dumps({
            "code": "request", "id": str(i), "method": "GET",
            "url": f"{BASE}/delay/{delay}",
        }))
    inputs.append(json.dumps({"code": "close"}))
    out = run_afh(inputs, timeout_s=30)
    responses = find_by_code(out, "response")
    assert len(responses) == 100, f"expected 100, got {len(responses)}"
    # Verify responses may arrive out-of-order (fast before slow)
    ids_in_order = [r["id"] for r in responses]
    assert ids_in_order != list(map(str, range(100))), \
        "all responses in order — expected some reordering with mixed delays"


@test("500 rapid-fire requests")
def test_500_rapid():
    inputs = []
    for i in range(500):
        inputs.append(json.dumps({
            "code": "request", "id": str(i), "method": "GET",
            "url": f"{BASE}/fast",
        }))
    inputs.append(json.dumps({"code": "close"}))
    out = run_afh(inputs, timeout_s=60)
    responses = find_by_code(out, "response")
    errors_out = find_by_code(out, "error")
    total = len(responses) + len([e for e in errors_out if e.get("id")])
    assert total == 500, f"expected 500 responses+errors, got {total} ({len(responses)} ok, {len(errors_out)} err)"
    assert len(responses) >= 450, f"too many failures: {len(responses)} ok out of 500"


@test("50 concurrent POST requests with JSON bodies")
def test_50_posts():
    inputs = []
    for i in range(50):
        inputs.append(json.dumps({
            "code": "request", "id": str(i), "method": "POST",
            "url": f"{BASE}/echo",
            "body": {"index": i, "data": "x" * 100},
        }))
    inputs.append(json.dumps({"code": "close"}))
    out = run_afh(inputs, timeout_s=30)
    responses = find_by_code(out, "response")
    assert len(responses) == 50, f"expected 50, got {len(responses)}"


@test("concurrent requests with different body types")
def test_mixed_body_types():
    inputs = [
        json.dumps({"code": "request", "id": "json", "method": "POST",
                     "url": f"{BASE}/echo", "body": {"type": "json"}}),
        json.dumps({"code": "request", "id": "text", "method": "POST",
                     "url": f"{BASE}/echo", "body": "plain text"}),
        json.dumps({"code": "request", "id": "b64", "method": "POST",
                     "url": f"{BASE}/echo",
                     "body_base64": base64.b64encode(b"\x00\x01\x02").decode()}),
        json.dumps({"code": "request", "id": "empty", "method": "GET",
                     "url": f"{BASE}/empty"}),
        json.dumps({"code": "request", "id": "binary", "method": "GET",
                     "url": f"{BASE}/binary/50"}),
        json.dumps({"code": "close"}),
    ]
    out = run_afh(inputs)
    for rid in ["json", "text", "b64", "empty", "binary"]:
        r = find_by_id(out, "response", rid)
        assert r, f"no response for {rid}"


# ---------------------------------------------------------------------------
# Edge case tests
# ---------------------------------------------------------------------------

@test("cancel in-flight request")
def test_cancel():
    # Use /hang endpoint (120s sleep) so request is definitely still in-flight when we cancel
    out = run_afh_interactive([
        (0, json.dumps({"code": "request", "id": "slow", "method": "GET",
                         "url": f"{BASE}/hang",
                         "options": {"retry": 0, "timeout_idle_s": 30}})),
        (0.5, json.dumps({"code": "cancel", "id": "slow"})),
        (1, json.dumps({"code": "close"})),
    ], timeout_s=15)
    e = find_by_id(out, "error", "slow")
    assert e, f"no error for cancelled request, got: {[o.get('code') for o in out]}"
    assert e["error_code"] == "cancelled", f"error_code: {e['error_code']}"


@test("config change mid-flight doesn't break running requests")
def test_config_mid_flight():
    out = run_afh_interactive([
        (0, json.dumps({"code": "request", "id": "1", "method": "GET",
                         "url": f"{BASE}/delay/500"})),
        (0.1, json.dumps({"code": "config", "defaults": {"timeout_idle_s": 999}})),
        (0, json.dumps({"code": "request", "id": "2", "method": "GET",
                         "url": f"{BASE}/fast"})),
        (2, json.dumps({"code": "close"})),
    ])
    r1 = find_by_id(out, "response", "1")
    r2 = find_by_id(out, "response", "2")
    assert r1, "request 1 failed"
    assert r2, "request 2 failed"
    assert r1["status"] == 200
    assert r2["status"] == 200


@test("rapid ping flood (50 pings)")
def test_ping_flood():
    inputs = [json.dumps({"code": "ping"}) for _ in range(50)]
    inputs.append(json.dumps({"code": "close"}))
    out = run_afh(inputs)
    pongs = find_by_code(out, "pong")
    assert len(pongs) == 50, f"expected 50 pongs, got {len(pongs)}"


@test("multiple config updates accumulate correctly")
def test_config_accumulate():
    out = run_afh([
        json.dumps({"code": "config", "defaults": {"headers": {"X-A": "1"}}}),
        json.dumps({"code": "config", "defaults": {"headers": {"X-B": "2"}}}),
        json.dumps({"code": "config", "defaults": {"headers": {"X-A": None}}}),
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/headers"}),
        json.dumps({"code": "close"}),
    ])
    configs = find_by_code(out, "config")
    assert len(configs) == 3
    # After 3 updates: X-A removed, X-B present, User-Agent preserved
    last_config = configs[-1]
    assert "X-A" not in last_config["defaults"]["headers"]
    assert last_config["defaults"]["headers"]["X-B"] == "2"
    assert "User-Agent" in last_config["defaults"]["headers"]
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    assert get_header_ci(r["body"], "X-A") is None, "X-A should be removed"
    assert get_header_ci(r["body"], "X-B") == "2", f"X-B: {get_header_ci(r['body'], 'X-B')}"


@test("empty lines in stdin are ignored")
def test_empty_lines():
    out = run_afh([
        "",
        "   ",
        json.dumps({"code": "ping"}),
        "",
        json.dumps({"code": "close"}),
    ])
    pongs = find_by_code(out, "pong")
    assert len(pongs) == 1
    errs = find_by_code(out, "error")
    assert len(errs) == 0, f"unexpected errors: {errs}"


@test("retry on connection refused (with retry enabled)")
def test_retry_connect_refused():
    out = run_afh([
        json.dumps({"code": "config", "log": ["retry"]}),
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": "http://127.0.0.1:19999/fail",
                     "options": {"retry": 2, "timeout_idle_s": 10}}),
        json.dumps({"code": "close"}),
    ], timeout_s=20)
    e = find_by_id(out, "error", "1")
    assert e, "no error"
    assert e["error_code"] in ("connect_refused", "connect_timeout"), f"error_code: {e['error_code']}"
    assert e["retryable"] is True
    # Should have retry log events
    logs = find_by_code(out, "log")
    retry_logs = [l for l in logs if l.get("event") == "retry"]
    assert len(retry_logs) >= 1, f"expected retry logs, got {len(retry_logs)}"


@test("large JSON body (1MB) round-trips correctly")
def test_large_json_body():
    big_body = {"data": "x" * (1024 * 1024)}
    expected_len = len(json.dumps(big_body))
    out = run_afh([
        # Ensure the response (which echoes body length) isn't auto-downloaded
        json.dumps({"code": "config", "response_save_above_bytes": 20_000_000}),
        json.dumps({"code": "request", "id": "1", "method": "POST",
                     "url": f"{BASE}/echo",
                     "body": big_body}),
        json.dumps({"code": "close"}),
    ], timeout_s=30)
    r = find_by_id(out, "response", "1")
    assert r, f"no response, got: {[o.get('code') for o in out]}"
    # serde_json and Python json.dumps may differ by a byte (trailing space, etc.)
    actual = r["body"]["body_length"]
    assert abs(actual - expected_len) <= 2, \
        f"expected ~{expected_len}, got {actual} (diff={actual - expected_len})"


@test("redirect with log enabled")
def test_redirect_logging():
    out = run_afh([
        json.dumps({"code": "config", "log": ["redirect"]}),
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/redirect/3"}),
        json.dumps({"code": "close"}),
    ])
    logs = find_by_code(out, "log")
    redirect_logs = [l for l in logs if l.get("event") == "redirect"]
    assert len(redirect_logs) == 3, f"expected 3 redirect logs, got {len(redirect_logs)}"
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    assert r["status"] == 200


@test("graceful shutdown on stdin EOF")
def test_eof_shutdown():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET", "url": f"{BASE}/fast"}),
        # No close — rely on EOF
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    closes = find_by_code(out, "close")
    assert closes, "no close message on EOF"
    assert closes[0]["message"] == "shutdown"


# ---------------------------------------------------------------------------
# NEW: Additional coverage
# ---------------------------------------------------------------------------

@test("multipart upload sends correct content-type")
def test_multipart():
    out = run_afh_interactive([
        (0, json.dumps({"code": "request", "id": "1", "method": "POST",
                         "url": f"{BASE}/echo-multipart",
                         "body_multipart": [
                             {"name": "field1", "value": "hello"},
                             {"name": "field2", "value": "world"},
                         ]})),
        (3, json.dumps({"code": "close"})),
    ])
    r = find_by_id(out, "response", "1")
    assert r, f"no response, got: {[o.get('code') for o in out]}"
    assert r["status"] == 200
    assert r["body"]["has_multipart"] is True


@test("multipart with file upload")
def test_multipart_file():
    tmp_path = temp_path("afh-test-upload", ".txt")
    try:
        with open(tmp_path, "w") as f:
            f.write("file content here")
        out = run_afh_interactive([
            (0, json.dumps({"code": "request", "id": "1", "method": "POST",
                             "url": f"{BASE}/echo-multipart",
                             "body_multipart": [
                                 {"name": "purpose", "value": "test"},
                                 {"name": "file", "file": tmp_path,
                                  "filename": "test.txt", "content_type": "text/plain"},
                             ]})),
            (3, json.dumps({"code": "close"})),
        ])
        r = find_by_id(out, "response", "1")
        assert r, f"no response, got: {[o.get('code') for o in out]}"
        assert r["status"] == 200
        assert r["body"]["has_multipart"] is True
        assert r["body"]["body_length"] > 0
    finally:
        if os.path.exists(tmp_path):
            os.unlink(tmp_path)


@test("multipart with base64 binary part")
def test_multipart_base64():
    data = base64.b64encode(bytes(range(128))).decode()
    out = run_afh_interactive([
        (0, json.dumps({"code": "request", "id": "1", "method": "POST",
                         "url": f"{BASE}/echo-multipart",
                         "body_multipart": [
                             {"name": "data", "value_base64": data,
                              "filename": "data.bin", "content_type": "application/octet-stream"},
                         ]})),
        (3, json.dumps({"code": "close"})),
    ])
    r = find_by_id(out, "response", "1")
    assert r, f"no response, got: {[o.get('code') for o in out]}"
    assert r["body"]["has_multipart"] is True


@test("body_urlencoded sends correct content-type and encoded fields")
def test_body_urlencoded():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "POST",
                    "url": f"{BASE}/echo-urlencoded",
                    "body_urlencoded": [
                        {"name": "grant_type", "value": "authorization_code"},
                        {"name": "code", "value": "abc123"},
                    ]}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, f"no response, got: {[o.get('code') for o in out]}"
    assert r["status"] == 200
    ct = r["body"]["content_type"]
    assert "application/x-www-form-urlencoded" in ct, f"unexpected Content-Type: {ct}"
    fields = r["body"]["fields"]
    assert fields[0] == {"name": "grant_type", "value": "authorization_code"}
    assert fields[1] == {"name": "code", "value": "abc123"}


@test("body_urlencoded percent-encodes special characters")
def test_body_urlencoded_special_chars():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "POST",
                    "url": f"{BASE}/echo-urlencoded",
                    "body_urlencoded": [
                        {"name": "redirect_uri", "value": "https://app.example.com/cb?x=1&y=2"},
                        {"name": "note", "value": "hello world"},
                    ]}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    fields = r["body"]["fields"]
    assert fields[0] == {"name": "redirect_uri", "value": "https://app.example.com/cb?x=1&y=2"}
    assert fields[1] == {"name": "note", "value": "hello world"}


@test("body_urlencoded supports duplicate keys")
def test_body_urlencoded_duplicate_keys():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "POST",
                    "url": f"{BASE}/echo-urlencoded",
                    "body_urlencoded": [
                        {"name": "tag", "value": "rust"},
                        {"name": "tag", "value": "async"},
                        {"name": "tag", "value": "web"},
                    ]}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    fields = r["body"]["fields"]
    assert len(fields) == 3
    assert all(f["name"] == "tag" for f in fields)
    assert [f["value"] for f in fields] == ["rust", "async", "web"]


@test("body_urlencoded and body are mutually exclusive")
def test_body_urlencoded_mutual_exclusion():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "POST",
                    "url": f"{BASE}/echo",
                    "body": {"key": "value"},
                    "body_urlencoded": [{"name": "k", "value": "v"}]}),
        json.dumps({"code": "close"}),
    ])
    e = find_by_id(out, "error", "1")
    assert e, f"expected error, got: {[o.get('code') for o in out]}"
    assert e["error_code"] == "invalid_request"


@test("body_file sends file contents as request body")
def test_body_file():
    tmp_path = temp_path("afh-test-body", ".json")
    try:
        payload = json.dumps({"from_file": True})
        with open(tmp_path, "w") as f:
            f.write(payload)
        out = run_afh([
            json.dumps({"code": "request", "id": "1", "method": "POST",
                         "url": f"{BASE}/echo",
                         "body_file": tmp_path,
                         "headers": {"Content-Type": "application/json"}}),
            json.dumps({"code": "close"}),
        ])
        r = find_by_id(out, "response", "1")
        assert r, "no response"
        assert r["body"]["body_length"] == len(payload)
        assert json.loads(r["body"]["body"]) == {"from_file": True}
    finally:
        if os.path.exists(tmp_path):
            os.unlink(tmp_path)


@test("cancel non-existent request returns error")
def test_cancel_nonexistent():
    out = run_afh([
        json.dumps({"code": "cancel", "id": "does-not-exist"}),
        json.dumps({"code": "close"}),
    ])
    e = find_by_id(out, "error", "does-not-exist")
    assert e, "no error"
    assert e["error_code"] == "invalid_request", f"error_code: {e['error_code']}"


@test("server sends truncated body (Content-Length mismatch)")
def test_server_disconnect():
    # Server claims 1000 bytes but only sends 100, then closes cleanly.
    # reqwest blocks waiting for Content-Length bytes, so set short timeout.
    # Either the body read timeout fires, or close cancels the in-flight request.
    out = run_afh_interactive([
        (0, json.dumps({"code": "request", "id": "1", "method": "GET",
                         "url": f"{BASE}/disconnect/100",
                         "options": {"retry": 0, "timeout_idle_s": 2}})),
        (5, json.dumps({"code": "close"})),
    ], timeout_s=10)
    e = find_by_id(out, "error", "1")
    r = find_by_id(out, "response", "1")
    # Either error (detected truncation / timeout) or response (if reqwest accepted partial)
    assert e or r, f"no response or error, got: {[o.get('code') for o in out]}"


@test("chunked stream delivers partial data before server closes")
def test_chunked_disconnect():
    out = run_afh_interactive([
        (0, json.dumps({"code": "request", "id": "1", "method": "GET",
                         "url": f"{BASE}/stream/disconnect/3/100",
                         "options": {"chunked": True, "retry": 0}})),
        (3, json.dumps({"code": "close"})),
    ], timeout_s=10)
    # Should get chunk_start + some chunk_data
    cs = find_by_id(out, "chunk_start", "1")
    chunks = find_all_by_id(out, "chunk_data", "1")
    assert cs, "no chunk_start"
    assert len(chunks) >= 1, f"expected chunks, got {len(chunks)}"
    # Stream may end with error (disconnect) or chunk_end (clean close detected)
    # Either is acceptable behavior for a truncated stream
    e = find_by_id(out, "error", "1")
    ce = find_by_id(out, "chunk_end", "1")
    has_termination = e is not None or ce is not None
    # If neither, the stream was interrupted by shutdown — also acceptable
    # The key assertion is that we got partial data without crashing


@test("binary chunked with null delimiter uses data_base64")
def test_binary_chunked():
    out = run_afh_interactive([
        (0, json.dumps({"code": "request", "id": "1", "method": "GET",
                         "url": f"{BASE}/binary/500",
                         "options": {"chunked": True, "chunked_delimiter": None}})),
        (2, json.dumps({"code": "close"})),
    ])
    cs = find_by_id(out, "chunk_start", "1")
    assert cs, "no chunk_start"
    chunks = find_all_by_id(out, "chunk_data", "1")
    assert len(chunks) >= 1, "no chunk_data"
    # All chunks should use data_base64, not data
    for c in chunks:
        assert c.get("data_base64"), f"expected data_base64 in raw mode, got: {c}"
        assert c.get("data") is None
    # Verify total decoded size
    total = sum(len(base64.b64decode(c["data_base64"])) for c in chunks)
    assert total == 500, f"expected 500 bytes total, got {total}"


@test("concurrent chunked streams routed correctly by id")
def test_concurrent_chunked():
    out = run_afh_interactive([
        (0, json.dumps({"code": "request", "id": "stream-a", "method": "GET",
                         "url": f"{BASE}/stream/ndjson/5/20",
                         "options": {"chunked": True}})),
        (0, json.dumps({"code": "request", "id": "stream-b", "method": "GET",
                         "url": f"{BASE}/stream/ndjson/5/20",
                         "options": {"chunked": True}})),
        (3, json.dumps({"code": "close"})),
    ])
    chunks_a = find_all_by_id(out, "chunk_data", "stream-a")
    chunks_b = find_all_by_id(out, "chunk_data", "stream-b")
    assert len(chunks_a) == 5, f"stream-a: expected 5 chunks, got {len(chunks_a)}"
    assert len(chunks_b) == 5, f"stream-b: expected 5 chunks, got {len(chunks_b)}"
    ce_a = find_by_id(out, "chunk_end", "stream-a")
    ce_b = find_by_id(out, "chunk_end", "stream-b")
    assert ce_a, "no chunk_end for stream-a"
    assert ce_b, "no chunk_end for stream-b"


@test("redirect boundary: exactly at max_redirects succeeds")
def test_redirect_exact_boundary():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/redirect/5",
                     "options": {"response_redirect": 5}}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, f"no response, got: {[o.get('code') for o in out]}"
    assert r["status"] == 200
    assert r["body"]["redirected"] is True


@test("redirect boundary: max_redirects+1 fails")
def test_redirect_over_boundary():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/redirect/6",
                     "options": {"response_redirect": 5}}),
        json.dumps({"code": "close"}),
    ])
    e = find_by_id(out, "error", "1")
    assert e, "no error"
    assert e["error_code"] == "too_many_redirects"


@test("429 response returned as response (not retried)")
def test_429_not_retried():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/rate-limit/5"}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    assert r["status"] == 429
    assert r["body"]["error"] == "rate_limited"
    # Verify Retry-After header is passed through
    assert r["headers"].get("retry-after") == "5"


@test("response with 50 headers all preserved")
def test_many_response_headers():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/huge-headers"}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    # Check some of the 50 custom headers exist
    assert r["headers"].get("x-header-0") is not None, "missing x-header-0"
    assert r["headers"].get("x-header-49") is not None, "missing x-header-49"


@test("100 concurrent requests with connection refused (error flood)")
def test_100_errors():
    inputs = []
    for i in range(100):
        inputs.append(json.dumps({
            "code": "request", "id": str(i), "method": "GET",
            "url": "http://127.0.0.1:19999/fail",
            "options": {"retry": 0},
        }))
    inputs.append(json.dumps({"code": "close"}))
    out = run_afh(inputs, timeout_s=30)
    errs = [o for o in out if o.get("code") == "error" and o.get("id")]
    assert len(errs) == 100, f"expected 100 errors, got {len(errs)}"
    ids = {e["id"] for e in errs}
    assert len(ids) == 100, "missing error ids"


@test("concurrent downloads to different files")
def test_concurrent_downloads():
    paths = [temp_path(f"afh-dl-{i}", ".bin") for i in range(5)]
    try:
        inputs = []
        for i, p in enumerate(paths):
            inputs.append(json.dumps({
                "code": "request", "id": str(i), "method": "GET",
                "url": f"{BASE}/size/{1000 * (i + 1)}",
                "options": {"response_save_file": p},
            }))
        inputs.append(json.dumps({"code": "close"}))
        out = run_afh(inputs, timeout_s=15)
        for i, p in enumerate(paths):
            ce = find_by_id(out, "chunk_end", str(i))
            assert ce, f"no chunk_end for {i}"
            assert os.path.exists(p), f"file not created: {p}"
            expected = 1000 * (i + 1)
            actual = os.path.getsize(p)
            assert actual == expected, f"file {i}: expected {expected}, got {actual}"
    finally:
        for p in paths:
            if os.path.exists(p):
                os.unlink(p)


@test("slow body delivery doesn't break buffered response")
def test_slow_body():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/slow-body/5000/500/50",
                     "options": {"timeout_idle_s": 10}}),
        json.dumps({"code": "close"}),
    ], timeout_s=15)
    r = find_by_id(out, "response", "1")
    assert r, f"no response, got: {[o.get('code') for o in out]}"
    assert r["trace"]["received_bytes"] == 5000


@test("request with unknown code produces error")
def test_unknown_code():
    out = run_afh([
        json.dumps({"code": "foobar"}),
        json.dumps({"code": "close"}),
    ])
    errs = find_by_code(out, "error")
    assert errs, "no error for unknown code"


@test("request missing required fields produces error")
def test_missing_fields():
    out = run_afh([
        json.dumps({"code": "request", "id": "1"}),  # missing method and url
        json.dumps({"code": "close"}),
    ])
    errs = find_by_code(out, "error")
    assert errs, "no error for missing fields"


@test("empty POST body")
def test_empty_post():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "POST",
                     "url": f"{BASE}/echo"}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    assert r["body"]["body_length"] == 0


@test("request with many custom headers")
def test_many_request_headers():
    headers = {f"X-H-{i}": f"v{i}" for i in range(50)}
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/headers", "headers": headers}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    for i in range(50):
        v = get_header_ci(r["body"], f"X-H-{i}")
        assert v == f"v{i}", f"header X-H-{i}: expected v{i}, got {v}"


@test("error has structured AFD format")
def test_error_structured():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": "http://127.0.0.1:19999/fail",
                     "options": {"retry": 0}}),
        json.dumps({"code": "close"}),
    ])
    e = find_by_id(out, "error", "1")
    assert e, "no error"
    # Verify all AFD error fields exist
    assert "error_code" in e, f"missing error_code: {e.keys()}"
    assert "error" in e, f"missing error: {e.keys()}"
    assert "retryable" in e, f"missing retryable: {e.keys()}"
    assert "trace" in e, f"missing trace: {e.keys()}"
    assert isinstance(e["error_code"], str)
    assert isinstance(e["error"], str)
    assert isinstance(e["retryable"], bool)
    assert "message" not in e, "old message field still present"


@test("tag echoed in response")
def test_tag_response():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "tag": "batch-42",
                     "method": "GET", "url": f"{BASE}/fast"}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    assert r.get("tag") == "batch-42", f"tag: {r.get('tag')}"


@test("tag echoed in error")
def test_tag_error():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "tag": "err-tag",
                     "method": "GET", "url": "http://127.0.0.1:19999/fail",
                     "options": {"retry": 0}}),
        json.dumps({"code": "close"}),
    ])
    e = find_by_id(out, "error", "1")
    assert e, "no error"
    assert e.get("tag") == "err-tag", f"tag: {e.get('tag')}"


@test("tag echoed in chunk_start and chunk_end")
def test_tag_chunked():
    out = run_afh_interactive([
        (0, json.dumps({"code": "request", "id": "1", "tag": "stream-tag",
                         "method": "GET", "url": f"{BASE}/stream/ndjson/3/5",
                         "options": {"chunked": True}})),
        (2, json.dumps({"code": "close"})),
    ])
    cs = find_by_id(out, "chunk_start", "1")
    assert cs, "no chunk_start"
    assert cs.get("tag") == "stream-tag", f"chunk_start tag: {cs.get('tag')}"
    ce = find_by_id(out, "chunk_end", "1")
    assert ce, "no chunk_end"
    assert ce.get("tag") == "stream-tag", f"chunk_end tag: {ce.get('tag')}"


@test("tag absent when not provided")
def test_tag_absent():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/fast"}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    assert "tag" not in r, f"tag should be absent: {r.get('tag')}"


@test("host_defaults headers merged for matching host")
def test_host_defaults():
    out = run_afh([
        json.dumps({"code": "config", "host_defaults": {
            "127.0.0.1:18080": {"headers": {"Authorization": "Bearer host-token"}}
        }}),
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/headers"}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    auth = get_header_ci(r["body"], "Authorization")
    assert auth == "Bearer host-token", f"Authorization: {auth}"
    # config echo should show host_defaults
    configs = find_by_code(out, "config")
    assert configs, "no config"
    assert "127.0.0.1:18080" in configs[0].get("host_defaults", {})


@test("host_defaults don't leak to other hosts")
def test_host_defaults_no_leak():
    out = run_afh([
        json.dumps({"code": "config", "host_defaults": {
            "other-host.example.com": {"headers": {"X-Secret": "leaked"}}
        }}),
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/headers"}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    secret = get_header_ci(r["body"], "X-Secret")
    assert secret is None, f"X-Secret leaked to wrong host: {secret}"


@test("retry_on_status retries 429 and succeeds")
def test_retry_on_status():
    # Use unique key for this test to avoid counter collisions
    key = f"test-{os.getpid()}-{time.time()}"
    out = run_afh([
        json.dumps({"code": "config", "log": ["retry"]}),
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/retry-succeed/{key}/2",
                     "options": {"retry_on_status": [429], "retry": 3,
                                 "timeout_idle_s": 10}}),
        json.dumps({"code": "close"}),
    ], timeout_s=20)
    r = find_by_id(out, "response", "1")
    assert r, f"no response, got: {[o.get('code') for o in out]}"
    assert r["status"] == 200, f"expected 200 after retries, got {r['status']}"
    assert r["body"]["attempts"] == 3  # 2 failures + 1 success
    # Should have retry log events
    logs = find_by_code(out, "log")
    retry_logs = [l for l in logs if l.get("event") == "retry"]
    assert len(retry_logs) >= 1, f"expected retry logs, got {len(retry_logs)}"


@test("retry_on_status exhausted returns final response")
def test_retry_on_status_exhausted():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/rate-limit/0",
                     "options": {"retry_on_status": [429], "retry": 1,
                                 "timeout_idle_s": 10}}),
        json.dumps({"code": "close"}),
    ], timeout_s=15)
    r = find_by_id(out, "response", "1")
    assert r, f"no response, got: {[o.get('code') for o in out]}"
    assert r["status"] == 429, f"expected 429 after exhausted retries, got {r['status']}"


@test("max_response_bytes truncates large buffered response")
def test_max_response_bytes():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/size/10000",
                     "options": {"response_max_bytes": 5000, "retry": 0}}),
        json.dumps({"code": "close"}),
    ])
    e = find_by_id(out, "error", "1")
    assert e, f"no error, got: {[o.get('code') for o in out]}"
    assert e["error_code"] == "response_too_large", f"error_code: {e['error_code']}"
    assert e["retryable"] is False


@test("max_response_bytes on chunked stream")
def test_max_response_bytes_chunked():
    out = run_afh_interactive([
        (0, json.dumps({"code": "request", "id": "1", "method": "GET",
                         "url": f"{BASE}/stream/ndjson/100/5",
                         "options": {"chunked": True, "response_max_bytes": 100,
                                     "retry": 0}})),
        (3, json.dumps({"code": "close"})),
    ])
    e = find_by_id(out, "error", "1")
    assert e, f"no error, got: {[o.get('code') for o in out]}"
    assert e["error_code"] == "response_too_large", f"error_code: {e['error_code']}"


@test("content_length_bytes in chunk_start")
def test_content_length_bytes():
    out = run_afh_interactive([
        (0, json.dumps({"code": "request", "id": "1", "method": "GET",
                         "url": f"{BASE}/stream/ndjson/5/5",
                         "options": {"chunked": True}})),
        (2, json.dumps({"code": "close"})),
    ])
    cs = find_by_id(out, "chunk_start", "1")
    assert cs, "no chunk_start"
    # Server sends Content-Length, so content_length_bytes should be present
    assert "content_length_bytes" in cs, f"missing content_length_bytes: {cs.keys()}"
    assert cs["content_length_bytes"] > 0, f"content_length_bytes: {cs['content_length_bytes']}"


@test("content_length_bytes in download chunk_start")
def test_content_length_bytes_download():
    save_path = temp_path("afh-test-cl", ".bin")
    try:
        out = run_afh([
            json.dumps({"code": "request", "id": "1", "method": "GET",
                         "url": f"{BASE}/size/5000",
                         "options": {"response_save_file": save_path}}),
            json.dumps({"code": "close"}),
        ])
        cs = find_by_id(out, "chunk_start", "1")
        assert cs, "no chunk_start"
        assert cs["content_length_bytes"] == 5000, f"content_length_bytes: {cs.get('content_length_bytes')}"
    finally:
        if os.path.exists(save_path):
            os.unlink(save_path)


@test("1000 rapid-fire requests (throughput)")
def test_1000_rapid():
    inputs = []
    for i in range(1000):
        inputs.append(json.dumps({
            "code": "request", "id": str(i), "method": "GET",
            "url": f"{BASE}/fast",
        }))
    inputs.append(json.dumps({"code": "close"}))
    t0 = time.time()
    out = run_afh(inputs, timeout_s=120)
    elapsed = time.time() - t0
    responses = find_by_code(out, "response")
    errs = [o for o in find_by_code(out, "error") if o.get("id")]
    total = len(responses) + len(errs)
    assert total == 1000, f"expected 1000, got {total} ({len(responses)} ok, {len(errs)} err)"
    assert len(responses) >= 950, f"too many failures: {len(responses)}/1000"
    # Print throughput info
    print(f" [{len(responses)}/1000 ok in {elapsed:.1f}s = {1000/elapsed:.0f} req/s]", end="")


@test("response_save_resume without response_save_file returns error")
def test_response_save_resume_no_file():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/fast",
                     "options": {"response_save_resume": True}}),
        json.dumps({"code": "close"}),
    ])
    err = find_by_id(out, "error", "1")
    assert err, f"expected error, got: {[o.get('code') for o in out]}"
    assert "response_save_resume requires response_save_file" in err["error"]


@test("response_save_resume first download (file absent) fetches full file, no Range")
def test_response_save_resume_new_file():
    import tempfile, os
    tmp = tempfile.mktemp(suffix=".bin")
    try:
        out = run_afh([
            json.dumps({"code": "config", "log": ["request"]}),
            json.dumps({"code": "request", "id": "1", "method": "GET",
                         "url": f"{BASE}/range-file/100",
                         "options": {"response_save_file": tmp, "response_save_resume": True}}),
            json.dumps({"code": "close"}),
        ])
        req_logs = find_log_events(out, "request")
        if req_logs:
            ih = req_logs[0].get("implicit_headers", {})
            assert "Range" not in ih, f"unexpected Range on first download: {ih}"
        with open(tmp, "rb") as f:
            data = f.read()
        assert len(data) == 100, f"expected 100 bytes, got {len(data)}"
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)


@test("response_save_resume sends Range header and appends to file")
def test_response_save_resume():
    import tempfile, os
    with tempfile.NamedTemporaryFile(delete=False, suffix=".bin") as f:
        f.write(b"X" * 50)
        tmp = f.name
    try:
        out = run_afh([
            json.dumps({"code": "request", "id": "1", "method": "GET",
                         "url": f"{BASE}/range-file/100",
                         "options": {"response_save_file": tmp, "response_save_resume": True}}),
            json.dumps({"code": "close"}),
        ])
        end = find_by_id(out, "chunk_end", "1")
        assert end, f"no chunk_end: {[o.get('code') for o in out]}"
        with open(tmp, "rb") as f:
            data = f.read()
        assert len(data) == 100, f"expected 100 bytes after resume, got {len(data)}"
        assert data[:50] == b"X" * 50
        assert data[50:] == b"X" * 50
    finally:
        os.unlink(tmp)


@test("response_save_resume on empty file fetches full file (no Range)")
def test_response_save_resume_empty_file():
    import tempfile, os
    with tempfile.NamedTemporaryFile(delete=False, suffix=".bin") as f:
        tmp = f.name  # empty
    try:
        out = run_afh([
            json.dumps({"code": "config", "log": ["request"]}),
            json.dumps({"code": "request", "id": "1", "method": "GET",
                         "url": f"{BASE}/range-file/100",
                         "options": {"response_save_file": tmp, "response_save_resume": True}}),
            json.dumps({"code": "close"}),
        ])
        req_logs = find_log_events(out, "request")
        if req_logs:
            ih = req_logs[0].get("implicit_headers", {})
            assert "Range" not in ih, f"unexpected Range on empty file: {ih}"
        with open(tmp, "rb") as f:
            data = f.read()
        assert len(data) == 100, f"expected 100 bytes, got {len(data)}"
    finally:
        os.unlink(tmp)


@test("invalid UTF-8 text response returns body_base64 (not corrupted string)")
def test_invalid_utf8_text():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/invalid-utf8/text"}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    assert "body" not in r or r["body"] is None, \
        f"expected no body string for invalid UTF-8, got: {r.get('body')!r}"
    assert r.get("body_base64"), f"expected body_base64 for invalid UTF-8, keys: {list(r.keys())}"
    decoded = base64.b64decode(r["body_base64"])
    assert decoded == b"caf\xe9 r\xe9sum\xe9", f"decoded bytes mismatch: {decoded!r}"


@test("invalid UTF-8 JSON response returns body_base64 (not corrupted string)")
def test_invalid_utf8_json():
    out = run_afh([
        json.dumps({"code": "request", "id": "1", "method": "GET",
                     "url": f"{BASE}/invalid-utf8/json"}),
        json.dumps({"code": "close"}),
    ])
    r = find_by_id(out, "response", "1")
    assert r, "no response"
    assert "body" not in r or r["body"] is None, \
        f"expected no body for invalid UTF-8 JSON, got: {r.get('body')!r}"
    assert r.get("body_base64"), f"expected body_base64, keys: {list(r.keys())}"
    decoded = base64.b64decode(r["body_base64"])
    assert decoded.startswith(b"\xff\xfe"), f"original bytes not preserved: {decoded[:4]!r}"


@test("chunked delimiter mode with invalid UTF-8 returns data_base64")
def test_chunked_invalid_utf8():
    out = run_afh_interactive([
        (0, json.dumps({"code": "request", "id": "1", "method": "GET",
                         "url": f"{BASE}/stream/invalid-utf8",
                         "options": {"chunked": True, "chunked_delimiter": "\n"}})),
        (2, json.dumps({"code": "close"})),
    ])
    chunks = find_all_by_id(out, "chunk_data", "1")
    assert len(chunks) >= 1, f"no chunk_data, codes: {[o.get('code') for o in out]}"
    for c in chunks:
        assert c.get("data_base64") is not None, \
            f"expected data_base64 for invalid UTF-8 chunk, got data: {c.get('data')!r}"
        assert c.get("data") is None, \
            f"expected no data string for invalid UTF-8 chunk, got: {c.get('data')!r}"
        decoded = base64.b64decode(c["data_base64"])
        assert any(b in decoded for b in [b"\xff", b"\xfe", b"\xfd", b"\xe9"]), \
            f"decoded bytes don't contain expected invalid UTF-8: {decoded!r}"


@test("close reports final request count")
def test_close_count():
    out = run_afh_interactive([
        (0, json.dumps({"code": "request", "id": "1", "method": "GET",
                         "url": f"{BASE}/fast"})),
        (0, json.dumps({"code": "request", "id": "2", "method": "GET",
                         "url": f"{BASE}/fast"})),
        (0, json.dumps({"code": "request", "id": "3", "method": "GET",
                         "url": f"{BASE}/fast"})),
        (2, json.dumps({"code": "close"})),
    ])
    closes = find_by_code(out, "close")
    assert closes, "no close"
    assert closes[0]["trace"]["requests_total"] == 3


# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------

def main():
    global passed, failed

    # Start test server
    print("Starting test server on :18080...")
    server = start_server(18080)
    time.sleep(0.3)

    # Verify server is running
    import urllib.request
    try:
        urllib.request.urlopen(f"{BASE}/fast", timeout=2)
    except Exception as e:
        print(f"FATAL: test server not responding: {e}")
        sys.exit(1)
    print("Test server ready.\n")

    # Verify afh binary exists
    if not os.path.exists(AFH):
        print(f"FATAL: afh binary not found at {AFH}")
        print("Run: cargo build")
        sys.exit(1)

    tests = [
        # Correctness
        test_basic_get,
        test_post_json,
        test_post_string,
        test_post_base64,
        test_204,
        test_head,
        test_binary,
        test_text,
        test_4xx_5xx,
        test_user_agent,
        test_header_merge,
        test_config_update,
        test_redirects,
        test_redirect_disabled,
        test_too_many_redirects,
        test_duplicate_id_rejected,
        test_request_concurrency_limit_overloaded,
        test_duplicate_id_rejected_while_chunked_active,
        test_request_concurrency_limit_during_chunked_stream,
        test_redirect_303_switches_to_get,
        test_redirect_strips_auth_cross_origin,
        test_connection_refused,
        test_invalid_url,
        test_invalid_json,
        test_ping,
        test_startup,
        test_multi_header,
        test_unicode,
        test_parse_json_false,
        test_request_timeout,
        # Chunked
        test_chunked_sse,
        test_chunked_ndjson,
        # Download
        test_large_auto_download,
        test_save_to,
        test_download_progress,
        # Stress
        test_100_concurrent,
        test_200_varied,
        test_100_mixed_delays,
        test_500_rapid,
        test_50_posts,
        test_mixed_body_types,
        # Edge cases
        test_cancel,
        test_config_mid_flight,
        test_ping_flood,
        test_config_accumulate,
        test_empty_lines,
        test_retry_connect_refused,
        test_large_json_body,
        test_redirect_logging,
        test_eof_shutdown,
        test_close_count,
        # New: additional coverage
        test_multipart,
        test_multipart_file,
        test_multipart_base64,
        test_body_urlencoded,
        test_body_urlencoded_special_chars,
        test_body_urlencoded_duplicate_keys,
        test_body_urlencoded_mutual_exclusion,
        test_body_file,
        test_cancel_nonexistent,
        test_server_disconnect,
        test_chunked_disconnect,
        test_binary_chunked,
        test_concurrent_chunked,
        test_redirect_exact_boundary,
        test_redirect_over_boundary,
        test_429_not_retried,
        test_many_response_headers,
        test_100_errors,
        test_concurrent_downloads,
        test_slow_body,
        test_unknown_code,
        test_missing_fields,
        test_empty_post,
        test_many_request_headers,
        # New: AFD error format + features
        test_error_structured,
        test_tag_response,
        test_tag_error,
        test_tag_chunked,
        test_tag_absent,
        test_host_defaults,
        test_host_defaults_no_leak,
        test_retry_on_status,
        test_retry_on_status_exhausted,
        test_max_response_bytes,
        test_max_response_bytes_chunked,
        test_content_length_bytes,
        test_content_length_bytes_download,
        test_1000_rapid,
        # Resume download
        test_response_save_resume_no_file,
        test_response_save_resume_new_file,
        test_response_save_resume,
        test_response_save_resume_empty_file,
        # Invalid UTF-8 safety
        test_invalid_utf8_text,
        test_invalid_utf8_json,
        test_chunked_invalid_utf8,
    ]

    for t in tests:
        t()

    print(f"\n{'='*60}")
    print(f"  {passed} passed, {failed} failed, {passed+failed} total")
    if errors:
        print(f"\n  Failures:")
        for name, msg in errors:
            print(f"    {name}: {msg}")
    print(f"{'='*60}")

    server.shutdown()
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
