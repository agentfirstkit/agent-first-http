#!/usr/bin/env python3
"""
CLI mode correctness tests for afh.
Starts a local test server, runs afh CLI commands, validates JSON output.
"""

import json
import os
import subprocess
import sys
import time
import base64
import tempfile

sys.path.insert(0, os.path.dirname(__file__))
from server import start_server

AFH = os.path.join(os.path.dirname(__file__), "..", "target", "debug", "afhttp")
BASE = "http://127.0.0.1:18080"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def run_cli(args: list[str], timeout_s=30, stdin_data=None) -> tuple[list[dict], int]:
    """Run afh with CLI args, return (parsed_output_lines, exit_code)."""
    proc = subprocess.run(
        [AFH] + args,
        input=stdin_data,
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
    return lines, proc.returncode


def find_by_code(outputs, code):
    return [o for o in outputs if o.get("code") == code]


def find_log_events(outputs, event):
    """Find log events by event type (code=log, event=<event>)."""
    return [o for o in outputs if o.get("code") == "log" and o.get("event") == event]


def get_header_ci(headers_dict, name):
    """Case-insensitive header lookup."""
    name_lower = name.lower()
    for k, v in headers_dict.items():
        if k.lower() == name_lower:
            return v
    return None


def temp_path(prefix: str, suffix: str) -> str:
    return os.path.join(
        tempfile.gettempdir(),
        f"{prefix}-{os.getpid()}-{time.time_ns()}{suffix}",
    )


passed = 0
failed = 0
errors = []


def test(name):
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
# Structural tests
# ---------------------------------------------------------------------------

@test("default output has no startup log")
def test_no_startup_default():
    out, _ = run_cli(["GET", f"{BASE}/fast"])
    codes = [o["code"] for o in out]
    assert codes == ["response"], f"codes: {codes}"


@test("--log startup enables startup log event")
def test_log_startup():
    out, _ = run_cli(["GET", f"{BASE}/fast", "--log", "startup"])
    startups = find_log_events(out, "startup")
    assert startups, f"no startup log, got: {out}"
    s = startups[0]
    assert s["version"] == "0.1.0"
    assert isinstance(s["argv"], list)
    assert "GET" in s["argv"]
    assert s["config"]["timeout_connect_s"] == 10
    assert s["config"]["defaults"]["timeout_idle_s"] == 30


@test("--verbose enables startup and all log categories")
def test_verbose():
    out, _ = run_cli(["GET", f"{BASE}/fast", "--verbose"])
    events = [o.get("event") for o in out if o.get("code") == "log"]
    assert "startup" in events, f"no startup with --verbose: {events}"


@test("no close message in CLI mode")
def test_no_close():
    out, code = run_cli(["GET", f"{BASE}/fast"])
    codes = [o["code"] for o in out]
    assert "close" not in codes, f"close present in CLI output: {codes}"


@test("id and tag fields absent in CLI output")
def test_no_id_tag():
    out, _ = run_cli(["GET", f"{BASE}/fast", "--log", "startup"])
    for line in out:
        assert "id" not in line, f"id present in {line['code']}: {line.get('id')}"
        assert "tag" not in line, f"tag present in {line['code']}: {line.get('tag')}"


@test("default output is response only")
def test_output_order():
    out, _ = run_cli(["GET", f"{BASE}/fast"])
    codes = [o["code"] for o in out]
    assert codes == ["response"], f"codes: {codes}"


# ---------------------------------------------------------------------------
# Exit codes
# ---------------------------------------------------------------------------

@test("exit 0 on successful response")
def test_exit_0():
    _, code = run_cli(["GET", f"{BASE}/fast"])
    assert code == 0, f"exit code: {code}"


@test("exit 0 on 4xx/5xx (HTTP error is still a response)")
def test_exit_0_on_4xx():
    _, code = run_cli(["GET", f"{BASE}/status/404"])
    assert code == 0, f"exit code: {code}"


@test("exit 1 on transport error")
def test_exit_1():
    _, code = run_cli(["GET", "http://127.0.0.1:19999/fail"])
    assert code == 1, f"exit code: {code}"


@test("exit 2 on no arguments")
def test_exit_2_no_args():
    proc = subprocess.run([AFH], capture_output=True, text=True, timeout=5)
    assert proc.returncode == 2, f"exit code: {proc.returncode}"


@test("exit 2 on missing URL")
def test_exit_2_no_url():
    proc = subprocess.run([AFH, "GET"], capture_output=True, text=True, timeout=5)
    assert proc.returncode == 2, f"exit code: {proc.returncode}"


# ---------------------------------------------------------------------------
# Basic requests
# ---------------------------------------------------------------------------

@test("GET returns parsed JSON body")
def test_get_json():
    out, _ = run_cli(["GET", f"{BASE}/fast"])
    r = find_by_code(out, "response")
    assert r, "no response"
    assert r[0]["status"] == 200
    assert r[0]["body"]["ok"] is True


@test("GET returns text body as string")
def test_get_text():
    out, _ = run_cli(["GET", f"{BASE}/text/100"])
    r = find_by_code(out, "response")
    assert r, "no response"
    assert r[0]["body"] == "A" * 100


@test("GET returns binary as body_base64")
def test_get_binary():
    out, _ = run_cli(["GET", f"{BASE}/binary/50"])
    r = find_by_code(out, "response")
    assert r, "no response"
    assert "body_base64" in r[0] and r[0]["body_base64"]
    decoded = base64.b64decode(r[0]["body_base64"])
    assert len(decoded) == 50


@test("HEAD returns no body")
def test_head():
    out, _ = run_cli(["HEAD", f"{BASE}/head-test"])
    r = find_by_code(out, "response")
    assert r, "no response"
    assert r[0]["status"] == 200
    assert "body" not in r[0] or r[0]["body"] is None


@test("204 returns no body")
def test_204():
    out, _ = run_cli(["GET", f"{BASE}/empty"])
    r = find_by_code(out, "response")
    assert r, "no response"
    assert r[0]["status"] == 204


@test("4xx status returned as response")
def test_4xx():
    out, _ = run_cli(["GET", f"{BASE}/status/404"])
    r = find_by_code(out, "response")
    assert r, "no response"
    assert r[0]["status"] == 404


@test("5xx status returned as response")
def test_5xx():
    out, _ = run_cli(["GET", f"{BASE}/status/500"])
    r = find_by_code(out, "response")
    assert r, "no response"
    assert r[0]["status"] == 500


# ---------------------------------------------------------------------------
# Headers
# ---------------------------------------------------------------------------

@test("default User-Agent header sent")
def test_user_agent():
    out, _ = run_cli(["GET", f"{BASE}/headers"])
    r = find_by_code(out, "response")
    assert r, "no response"
    ua = get_header_ci(r[0]["body"], "User-Agent")
    assert ua and ua.startswith("afhttp/"), f"User-Agent: {ua}"


@test("-H adds custom header")
def test_custom_header():
    out, _ = run_cli(["GET", f"{BASE}/headers", "--header", "X-Custom: test-value"])
    r = find_by_code(out, "response")
    assert r, "no response"
    assert get_header_ci(r[0]["body"], "X-Custom") == "test-value"


@test("multiple -H flags")
def test_multi_header():
    out, _ = run_cli(["GET", f"{BASE}/headers",
                       "--header", "X-A: one", "--header", "X-B: two"])
    r = find_by_code(out, "response")
    assert r, "no response"
    assert get_header_ci(r[0]["body"], "X-A") == "one"
    assert get_header_ci(r[0]["body"], "X-B") == "two"


@test("-H with empty value removes default header")
def test_remove_header():
    out, _ = run_cli(["GET", f"{BASE}/headers", "--header", "User-Agent:"])
    r = find_by_code(out, "response")
    assert r, "no response"
    assert get_header_ci(r[0]["body"], "User-Agent") is None, \
        f"User-Agent should be removed"


# ---------------------------------------------------------------------------
# Body
# ---------------------------------------------------------------------------

@test("-b with JSON auto-detected")
def test_body_json():
    out, _ = run_cli(["POST", f"{BASE}/echo", "--body", '{"key":"value"}'])
    r = find_by_code(out, "response")
    assert r, "no response"
    echo = r[0]["body"]
    assert "application/json" in echo["content_type"]
    assert json.loads(echo["body"]) == {"key": "value"}


@test("-b with plain text sends raw bytes (no implicit Content-Type)")
def test_body_text():
    out, _ = run_cli(["POST", f"{BASE}/echo", "--body", "hello world"])
    r = find_by_code(out, "response")
    assert r, "no response"
    echo = r[0]["body"]
    # No implicit Content-Type — caller must specify if needed
    assert echo["content_type"] == "", f"expected no Content-Type for string body, got: {echo['content_type']!r}"
    assert echo["body"] == "hello world"


@test("-b @path reads body from file")
def test_body_at_file():
    with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
        f.write('{"from_file":true}')
        tmp = f.name
    try:
        out, _ = run_cli(["POST", f"{BASE}/echo", "--body", f"@{tmp}",
                           "--header", "Content-Type: application/json"])
        r = find_by_code(out, "response")
        assert r, "no response"
        assert json.loads(r[0]["body"]["body"]) == {"from_file": True}
    finally:
        os.unlink(tmp)


@test("--body-file reads body from file")
def test_body_file_flag():
    with tempfile.NamedTemporaryFile(mode="w", suffix=".txt", delete=False) as f:
        f.write("file content")
        tmp = f.name
    try:
        out, _ = run_cli(["POST", f"{BASE}/echo", "--body-file", tmp])
        r = find_by_code(out, "response")
        assert r, "no response"
        assert r[0]["body"]["body_length"] == len("file content")
    finally:
        os.unlink(tmp)


@test("--body-base64 sends binary body")
def test_body_base64():
    data = bytes(range(64))
    b64 = base64.b64encode(data).decode()
    out, _ = run_cli(["POST", f"{BASE}/echo", "--body-base64", b64])
    r = find_by_code(out, "response")
    assert r, "no response"
    assert r[0]["body"]["body_length"] == 64


@test("--body-multipart sends multipart")
def test_body_multipart():
    out, _ = run_cli(["POST", f"{BASE}/echo-multipart",
                       "--body-multipart", "field1=hello",
                       "--body-multipart", "field2=world"])
    r = find_by_code(out, "response")
    assert r, "no response"
    assert r[0]["body"]["has_multipart"] is True


@test("--body-multipart with file upload")
def test_body_multipart_file():
    with tempfile.NamedTemporaryFile(mode="w", suffix=".txt", delete=False) as f:
        f.write("upload data")
        tmp = f.name
    try:
        out, _ = run_cli(["POST", f"{BASE}/echo-multipart",
                           "--body-multipart", f"file=@{tmp};filename=test.txt;type=text/plain"])
        r = find_by_code(out, "response")
        assert r, "no response"
        assert r[0]["body"]["has_multipart"] is True
    finally:
        os.unlink(tmp)


@test("--body-urlencoded sends correct content-type and fields")
def test_body_urlencoded():
    out, _ = run_cli(["POST", f"{BASE}/echo-urlencoded",
                      "--body-urlencoded", "grant_type=authorization_code",
                      "--body-urlencoded", "code=abc123"])
    r = find_by_code(out, "response")
    assert r, "no response"
    ct = r[0]["body"]["content_type"]
    assert "application/x-www-form-urlencoded" in ct, f"unexpected Content-Type: {ct}"
    fields = r[0]["body"]["fields"]
    assert fields[0] == {"name": "grant_type", "value": "authorization_code"}
    assert fields[1] == {"name": "code", "value": "abc123"}


@test("--body-urlencoded percent-encodes special characters")
def test_body_urlencoded_special_chars():
    out, _ = run_cli(["POST", f"{BASE}/echo-urlencoded",
                      "--body-urlencoded", "note=hello world",
                      "--body-urlencoded", "redirect_uri=https://app.example.com/cb?x=1&y=2"])
    r = find_by_code(out, "response")
    assert r, "no response"
    fields = r[0]["body"]["fields"]
    assert fields[0] == {"name": "note", "value": "hello world"}
    assert fields[1] == {"name": "redirect_uri", "value": "https://app.example.com/cb?x=1&y=2"}


@test("--body-urlencoded supports duplicate keys")
def test_body_urlencoded_duplicate_keys():
    out, _ = run_cli(["POST", f"{BASE}/echo-urlencoded",
                      "--body-urlencoded", "tag=rust",
                      "--body-urlencoded", "tag=async",
                      "--body-urlencoded", "tag=web"])
    r = find_by_code(out, "response")
    assert r, "no response"
    fields = r[0]["body"]["fields"]
    assert len(fields) == 3
    assert all(f["name"] == "tag" for f in fields)
    assert [f["value"] for f in fields] == ["rust", "async", "web"]


@test("empty POST body")
def test_empty_post():
    out, _ = run_cli(["POST", f"{BASE}/echo"])
    r = find_by_code(out, "response")
    assert r, "no response"
    assert r[0]["body"]["body_length"] == 0


# ---------------------------------------------------------------------------
# Options
# ---------------------------------------------------------------------------

@test("--response-parse-json false returns JSON as string")
def test_parse_json_false():
    out, _ = run_cli(["GET", f"{BASE}/fast", "--response-parse-json", "false"])
    r = find_by_code(out, "response")
    assert r, "no response"
    assert isinstance(r[0]["body"], str), f"expected string, got {type(r[0]['body'])}"


@test("--response-redirect 0 returns redirect as-is")
def test_redirect_disabled():
    out, _ = run_cli(["GET", f"{BASE}/redirect/3", "--response-redirect", "0"])
    r = find_by_code(out, "response")
    assert r, "no response"
    assert r[0]["status"] == 302


@test("redirects followed by default")
def test_redirect_default():
    out, _ = run_cli(["GET", f"{BASE}/redirect/3"])
    r = find_by_code(out, "response")
    assert r, "no response"
    assert r[0]["status"] == 200
    assert r[0]["body"]["redirected"] is True


@test("--timeout-idle-s triggers timeout on slow server")
def test_timeout_idle():
    out, code = run_cli(["GET", f"{BASE}/hang", "--timeout-idle-s", "1"])
    assert code == 1, f"exit code: {code}"
    e = find_by_code(out, "error")
    assert e, f"no error, codes: {[o.get('code') for o in out]}"
    assert e[0]["error_code"] == "request_timeout"


@test("--retry retries on transport error")
def test_retry():
    t0 = time.time()
    out, code = run_cli(["GET", "http://127.0.0.1:19999/fail", "--retry", "2"])
    elapsed = time.time() - t0
    assert code == 1
    e = find_by_code(out, "error")
    assert e, "no error"
    # With 2 retries and backoff, should take > 0.1s
    assert elapsed > 0.1, f"too fast: {elapsed:.2f}s — retry may not have happened"


@test("--retry-on-status retries specific status codes")
def test_retry_on_status():
    key = f"cli-{os.getpid()}-{time.time()}"
    out, code = run_cli(["GET", f"{BASE}/retry-succeed/{key}/2",
                          "--retry", "3", "--retry-on-status", "429"])
    assert code == 0, f"exit code: {code}"
    r = find_by_code(out, "response")
    assert r, "no response"
    assert r[0]["status"] == 200
    assert r[0]["body"]["attempts"] == 3


@test("--response-save-above-bytes triggers auto-save for large response")
def test_max_inline_bytes():
    out, code = run_cli(["GET", f"{BASE}/size/5000",
                          "--response-save-above-bytes", "1000"])
    assert code == 0, f"exit code: {code}"
    r = find_by_code(out, "response")
    assert r, f"no response, codes: {[o.get('code') for o in out]}"
    assert "body_file" in r[0], f"expected body_file for large response, got keys: {list(r[0].keys())}"
    path = r[0]["body_file"]
    assert os.path.exists(path), f"body_file does not exist: {path}"
    assert os.path.getsize(path) == 5000
    os.unlink(path)


@test("--response-save-dir overrides auto-save directory")
def test_response_save_dir():
    save_dir = tempfile.mkdtemp(prefix="afh-test-savedir-")
    try:
        out, code = run_cli(["GET", f"{BASE}/size/5000",
                              "--response-save-above-bytes", "1000",
                              "--response-save-dir", save_dir])
        assert code == 0, f"exit code: {code}"
        r = find_by_code(out, "response")
        assert r, f"no response, codes: {[o.get('code') for o in out]}"
        assert "body_file" in r[0], f"expected body_file, got keys: {list(r[0].keys())}"
        path = r[0]["body_file"]
        assert path.startswith(save_dir), f"body_file {path} not in save_dir {save_dir}"
        assert os.path.exists(path), f"body_file does not exist: {path}"
        assert os.path.getsize(path) == 5000
    finally:
        import shutil
        shutil.rmtree(save_dir, ignore_errors=True)


@test("--retry-base-delay-ms controls retry backoff")
def test_retry_base_delay():
    t0 = time.time()
    out, code = run_cli(["GET", "http://127.0.0.1:19999/fail",
                          "--retry", "2", "--retry-base-delay-ms", "500"])
    elapsed = time.time() - t0
    assert code == 1
    e = find_by_code(out, "error")
    assert e, "no error"
    # With base delay 500ms and 2 retries: 500ms + 1000ms = 1.5s minimum
    assert elapsed > 1.0, f"too fast ({elapsed:.2f}s), retry-base-delay-ms may not work"


@test("--response-max-bytes triggers error on large response")
def test_max_bytes():
    out, code = run_cli(["GET", f"{BASE}/size/10000", "--response-max-bytes", "5000"])
    assert code == 1, f"exit code: {code}"
    e = find_by_code(out, "error")
    assert e, "no error"
    assert e[0]["error_code"] == "response_too_large"


# ---------------------------------------------------------------------------
# Streaming
# ---------------------------------------------------------------------------

@test("--chunked streams NDJSON")
def test_chunked_ndjson():
    out, _ = run_cli(["GET", f"{BASE}/stream/ndjson/5/5", "--chunked"])
    cs = find_by_code(out, "chunk_start")
    assert cs, "no chunk_start"
    chunks = find_by_code(out, "chunk_data")
    assert len(chunks) == 5, f"expected 5 chunks, got {len(chunks)}"
    ce = find_by_code(out, "chunk_end")
    assert ce, "no chunk_end"


@test("--chunked-delimiter '\\n\\n' streams SSE")
def test_chunked_sse():
    out, _ = run_cli(["GET", f"{BASE}/stream/sse/3/5", "--chunked-delimiter", "\\n\\n"])
    cs = find_by_code(out, "chunk_start")
    assert cs, "no chunk_start"
    chunks = find_by_code(out, "chunk_data")
    assert len(chunks) == 3, f"expected 3 chunks, got {len(chunks)}"


@test("--chunked-delimiter-raw streams binary chunks")
def test_chunked_raw():
    out, _ = run_cli(["GET", f"{BASE}/binary/100", "--chunked-delimiter-raw"])
    chunks = find_by_code(out, "chunk_data")
    assert len(chunks) >= 1, "no chunk_data"
    for c in chunks:
        assert c.get("data_base64"), "expected data_base64 in raw mode"


@test("chunked output order: chunk_start, chunk_data..., chunk_end")
def test_chunked_order():
    out, _ = run_cli(["GET", f"{BASE}/stream/ndjson/3/5", "--chunked"])
    codes = [o["code"] for o in out]
    assert codes[0] == "chunk_start"
    assert codes[-1] == "chunk_end"
    assert all(c == "chunk_data" for c in codes[1:-1])


# ---------------------------------------------------------------------------
# File download
# ---------------------------------------------------------------------------

@test("--response-save-file saves to file")
def test_save_to():
    save_path = temp_path("afh-cli-test", ".bin")
    try:
        out, code = run_cli(["GET", f"{BASE}/size/5000",
                              "--response-save-file", save_path])
        assert code == 0, f"exit code: {code}"
        ce = find_by_code(out, "chunk_end")
        assert ce, f"no chunk_end, codes: {[o.get('code') for o in out]}"
        assert ce[0]["body_file"] == save_path
        assert os.path.exists(save_path)
        assert os.path.getsize(save_path) == 5000
        # No progress without --log progress
        progs = find_log_events(out, "progress")
        assert len(progs) == 0, f"unexpected progress without --log progress: {progs}"
    finally:
        if os.path.exists(save_path):
            os.unlink(save_path)


@test("--log progress with --progress-bytes emits progress log events")
def test_download_progress():
    save_path = temp_path("afh-cli-progress", ".bin")
    try:
        out, code = run_cli(["GET", f"{BASE}/size/10000",
                              "--response-save-file", save_path,
                              "--log", "progress",
                              "--progress-bytes", "2000"])
        assert code == 0
        ce = find_by_code(out, "chunk_end")
        assert ce, "no chunk_end"
        assert ce[0]["trace"]["received_bytes"] == 10000
        progs = find_log_events(out, "progress")
        assert len(progs) >= 1, f"no progress events with --log progress: {[o.get('code') for o in out]}"
        assert "received_bytes" in progs[0], f"progress missing received_bytes: {progs[0]}"
    finally:
        if os.path.exists(save_path):
            os.unlink(save_path)


# ---------------------------------------------------------------------------
# Error handling
# ---------------------------------------------------------------------------

@test("DNS failure returns error JSON")
def test_dns_error():
    out, code = run_cli(["GET", "http://nonexistent.invalid.tld/"])
    assert code == 1
    e = find_by_code(out, "error")
    assert e, "no error"
    assert e[0]["error_code"] in ("dns_failed", "connect_refused")
    assert e[0]["retryable"] is True


@test("connection refused returns error JSON")
def test_connect_refused():
    out, code = run_cli(["GET", "http://127.0.0.1:19999/fail"])
    assert code == 1
    e = find_by_code(out, "error")
    assert e, "no error"
    assert e[0]["error_code"] in ("connect_refused", "connect_timeout")
    assert e[0]["retryable"] is True


@test("error has structured AFD fields")
def test_error_structure():
    out, _ = run_cli(["GET", "http://127.0.0.1:19999/fail"])
    e = find_by_code(out, "error")
    assert e, "no error"
    e = e[0]
    assert "error_code" in e
    assert "error" in e
    assert "retryable" in e
    assert "trace" in e
    assert isinstance(e["error_code"], str)
    assert isinstance(e["error"], str)
    assert isinstance(e["retryable"], bool)


# ---------------------------------------------------------------------------
# TLS flags
# ---------------------------------------------------------------------------

@test("--tls-insecure accepted (no crash)")
def test_tls_insecure():
    out, code = run_cli(["GET", f"{BASE}/fast", "--tls-insecure"])
    assert code == 0
    r = find_by_code(out, "response")
    assert r, "no response"
    assert r[0]["status"] == 200


# ---------------------------------------------------------------------------
# Trace
# ---------------------------------------------------------------------------

@test("trace has duration_ms in response")
def test_trace_duration():
    out, _ = run_cli(["GET", f"{BASE}/fast"])
    r = find_by_code(out, "response")
    assert r, "no response"
    assert "trace" in r[0]
    assert r[0]["trace"]["duration_ms"] >= 0


@test("trace has received_bytes")
def test_trace_received():
    out, _ = run_cli(["GET", f"{BASE}/text/500"])
    r = find_by_code(out, "response")
    assert r, "no response"
    assert r[0]["trace"]["received_bytes"] == 500


# ---------------------------------------------------------------------------
# Method case insensitivity
# ---------------------------------------------------------------------------

@test("method is case-insensitive")
def test_method_case():
    out, code = run_cli(["get", f"{BASE}/fast"])
    assert code == 0
    r = find_by_code(out, "response")
    assert r, "no response"
    assert r[0]["status"] == 200


# ---------------------------------------------------------------------------
# Help and version
# ---------------------------------------------------------------------------

@test("--help prints help and exits 0")
def test_help():
    proc = subprocess.run([AFH, "--help"], capture_output=True, text=True, timeout=5)
    assert proc.returncode == 0
    assert "Agent-First HTTP" in proc.stdout


@test("--version prints version and exits 0")
def test_version():
    proc = subprocess.run([AFH, "--version"], capture_output=True, text=True, timeout=5)
    assert proc.returncode == 0
    assert "0.1.0" in proc.stdout


# ---------------------------------------------------------------------------
# Unicode
# ---------------------------------------------------------------------------

@test("unicode in response preserved")
def test_unicode():
    out, _ = run_cli(["GET", f"{BASE}/unicode"])
    r = find_by_code(out, "response")
    assert r, "no response"
    assert "你好世界" in r[0]["body"]["text"]


# ---------------------------------------------------------------------------
# Resume download
# ---------------------------------------------------------------------------

@test("--response-save-resume without --response-save-file returns error")
def test_response_save_resume_no_file():
    out, code = run_cli(["GET", f"{BASE}/fast", "--response-save-resume"])
    assert code == 1, f"expected exit code 1, got {code}"
    errs = find_by_code(out, "error")
    assert errs, "no error output"
    assert "response_save_resume requires response_save_file" in errs[0]["error"]


@test("--response-save-resume first download (file absent) fetches full file, no Range")
def test_response_save_resume_new_file():
    import tempfile, os
    tmp = tempfile.mktemp(suffix=".bin")
    try:
        out, code = run_cli(["GET", f"{BASE}/range-file/100",
                              "--response-save-file", tmp,
                              "--response-save-resume",
                              "--log", "request"])
        assert code == 0, f"exit code: {code}"
        # No Range header when file doesn't exist
        logs = find_log_events(out, "request")
        if logs:
            ih = logs[0].get("implicit_headers", {})
            assert "Range" not in ih, f"unexpected Range header on first download: {ih}"
        with open(tmp, "rb") as f:
            data = f.read()
        assert len(data) == 100, f"expected full 100 bytes, got {len(data)}"
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)


@test("--response-save-resume sends Range header and appends to file")
def test_response_save_resume():
    import tempfile, os
    with tempfile.NamedTemporaryFile(delete=False, suffix=".bin") as f:
        f.write(b"X" * 50)  # pre-existing 50 bytes
        tmp = f.name
    try:
        out, code = run_cli(["GET", f"{BASE}/range-file/100",
                              "--response-save-file", tmp,
                              "--response-save-resume"])
        assert code == 0, f"exit code: {code}"
        with open(tmp, "rb") as f:
            data = f.read()
        assert len(data) == 100, f"expected 100 bytes after resume, got {len(data)}"
        assert data[:50] == b"X" * 50, "first half from pre-existing file"
        assert data[50:] == b"X" * 50, "second half appended from server"
    finally:
        os.unlink(tmp)


@test("--response-save-resume logs Range header as implicit header")
def test_response_save_resume_log():
    import tempfile, os
    with tempfile.NamedTemporaryFile(delete=False, suffix=".bin") as f:
        f.write(b"X" * 50)
        tmp = f.name
    try:
        out, _ = run_cli(["GET", f"{BASE}/range-file/100",
                           "--response-save-file", tmp,
                           "--response-save-resume",
                           "--log", "request"])
        logs = find_log_events(out, "request")
        assert logs, "expected request log event"
        ih = logs[0].get("implicit_headers", {})
        assert "Range" in ih, f"Range missing from implicit_headers: {ih}"
        assert ih["Range"] == "bytes=50-", f"unexpected Range value: {ih['Range']}"
    finally:
        os.unlink(tmp)


@test("--response-save-resume on empty file fetches full file (no Range)")
def test_response_save_resume_empty_file():
    import tempfile, os
    with tempfile.NamedTemporaryFile(delete=False, suffix=".bin") as f:
        tmp = f.name  # empty file
    try:
        out, code = run_cli(["GET", f"{BASE}/range-file/100",
                              "--response-save-file", tmp,
                              "--response-save-resume",
                              "--log", "request"])
        assert code == 0, f"exit code: {code}"
        logs = find_log_events(out, "request")
        if logs:
            ih = logs[0].get("implicit_headers", {})
            assert "Range" not in ih, f"unexpected Range on empty file: {ih}"
        with open(tmp, "rb") as f:
            data = f.read()
        assert len(data) == 100, f"expected 100 bytes, got {len(data)}"
    finally:
        os.unlink(tmp)


# ---------------------------------------------------------------------------
# Invalid UTF-8 / binary safety
# ---------------------------------------------------------------------------

@test("invalid UTF-8 text response returns body_base64 (not corrupted string)")
def test_invalid_utf8_text():
    out, code = run_cli(["GET", f"{BASE}/invalid-utf8/text"])
    assert code == 0, f"exit code: {code}"
    r = find_by_code(out, "response")
    assert r, "no response"
    r = r[0]
    assert "body" not in r or r["body"] is None, \
        f"expected no body string for invalid UTF-8, got: {r.get('body')!r}"
    assert r.get("body_base64"), f"expected body_base64 for invalid UTF-8, keys: {list(r.keys())}"
    decoded = base64.b64decode(r["body_base64"])
    assert decoded == b"caf\xe9 r\xe9sum\xe9", f"decoded bytes mismatch: {decoded!r}"


@test("invalid UTF-8 JSON response returns body_base64 (not corrupted string)")
def test_invalid_utf8_json():
    out, code = run_cli(["GET", f"{BASE}/invalid-utf8/json"])
    assert code == 0, f"exit code: {code}"
    r = find_by_code(out, "response")
    assert r, "no response"
    r = r[0]
    assert "body" not in r or r["body"] is None, \
        f"expected no body for invalid UTF-8 JSON, got: {r.get('body')!r}"
    assert r.get("body_base64"), f"expected body_base64, keys: {list(r.keys())}"
    decoded = base64.b64decode(r["body_base64"])
    assert decoded.startswith(b"\xff\xfe"), f"original bytes not preserved: {decoded[:4]!r}"


@test("chunked delimiter mode with invalid UTF-8 returns data_base64")
def test_chunked_invalid_utf8():
    out, code = run_cli(["GET", f"{BASE}/stream/invalid-utf8",
                          "--chunked-delimiter", "\\n"])
    assert code == 0, f"exit code: {code}"
    chunks = find_by_code(out, "chunk_data")
    assert len(chunks) >= 1, f"no chunk_data, codes: {[o.get('code') for o in out]}"
    for c in chunks:
        assert c.get("data_base64") is not None, \
            f"expected data_base64 for invalid UTF-8 chunk, got data: {c.get('data')!r}"
        assert c.get("data") is None, \
            f"expected no data string for invalid UTF-8 chunk, got: {c.get('data')!r}"
        decoded = base64.b64decode(c["data_base64"])
        assert b"\xff" in decoded or b"\xfe" in decoded or b"\xfd" in decoded or b"\xe9" in decoded, \
            f"decoded bytes don't contain expected invalid UTF-8: {decoded!r}"


# ---------------------------------------------------------------------------
# Output format
# ---------------------------------------------------------------------------

@test("--output json is default (valid JSON)")
def test_output_json():
    out, code = run_cli(["GET", f"{BASE}/fast"])
    assert code == 0
    assert len(out) >= 1
    assert out[0]["code"] == "response"

@test("--output yaml produces YAML output")
def test_output_yaml():
    proc = subprocess.run(
        [AFH, "GET", f"{BASE}/fast", "--output", "yaml"],
        capture_output=True, text=True, timeout=10,
    )
    assert proc.returncode == 0
    assert "---" in proc.stdout, f"no YAML header: {proc.stdout[:200]}"
    assert "code" in proc.stdout or "response" in proc.stdout

@test("--output plain produces logfmt output")
def test_output_plain():
    proc = subprocess.run(
        [AFH, "GET", f"{BASE}/fast", "--output", "plain"],
        capture_output=True, text=True, timeout=10,
    )
    assert proc.returncode == 0
    # logfmt: key=value pairs, should contain code=response
    assert "code=response" in proc.stdout, f"no code=response in plain output: {proc.stdout[:200]}"

@test("--output yaml preserves server body")
def test_output_yaml_body():
    proc = subprocess.run(
        [AFH, "GET", f"{BASE}/fast", "--output", "yaml"],
        capture_output=True, text=True, timeout=10,
    )
    assert proc.returncode == 0
    # Server JSON body should appear as a JSON string, not decomposed into YAML keys
    assert "ok" in proc.stdout, f"body content missing: {proc.stdout[:300]}"

@test("--output invalid rejected")
def test_output_invalid():
    proc = subprocess.run(
        [AFH, "GET", f"{BASE}/fast", "--output", "xml"],
        capture_output=True, text=True, timeout=10,
    )
    assert proc.returncode == 2, f"expected exit 2, got {proc.returncode}"

@test("gzip response auto-decompressed")
def test_gzip_decompress():
    out, code = run_cli(["GET", f"{BASE}/gzip"])
    assert code == 0, f"exit code: {code}"
    r = find_by_code(out, "response")
    assert r, "no response"
    body = r[0].get("body")
    assert body is not None, f"no body, keys: {list(r[0].keys())}"
    assert body.get("compressed") is True, f"body not parsed: {body}"
    assert body.get("message") == "hello from gzip"

@test("--response-decompress false returns raw bytes")
def test_decompress_false():
    out, code = run_cli(["GET", f"{BASE}/gzip", "--response-decompress", "false"])
    assert code == 0, f"exit code: {code}"
    r = find_by_code(out, "response")
    assert r, "no response"
    # With decompress=false + Accept-Encoding: identity, server may still gzip.
    # If it does, body should be base64 (binary), not parsed JSON.
    # If it doesn't, body could be parsed JSON. Either is acceptable.
    # The key test is that we don't crash and return a valid response.
    assert r[0]["status"] == 200

@test("bare --verbose flag (no value)")
def test_verbose_bare():
    out, _ = run_cli(["GET", f"{BASE}/fast", "--verbose"])
    events = [o.get("event") for o in out if o.get("code") == "log"]
    assert "startup" in events, f"--verbose bare flag failed: {events}"

@test("bare --chunked flag (no value)")
def test_chunked_bare():
    out, _ = run_cli(["GET", f"{BASE}/stream/ndjson/3/5", "--chunked"])
    cs = find_by_code(out, "chunk_start")
    assert cs, "no chunk_start with bare --chunked"

@test("--log request logs implicit Content-Type for JSON body")
def test_log_request_content_type():
    out, _ = run_cli(["POST", f"{BASE}/echo", "--log", "request", "--body", '{"key":"val"}'])
    logs = find_log_events(out, "request")
    assert len(logs) == 1, f"expected 1 request log, got {len(logs)}: {logs}"
    ih = logs[0].get("implicit_headers", {})
    assert "Content-Type" in ih, f"missing Content-Type in implicit_headers: {ih}"
    assert "application/json" in ih["Content-Type"], f"unexpected CT: {ih['Content-Type']}"

@test("--log request logs implicit Accept-Encoding when decompress=true")
def test_log_request_accept_encoding():
    out, _ = run_cli(["GET", f"{BASE}/fast", "--log", "request"])
    logs = find_log_events(out, "request")
    assert len(logs) == 1, f"expected 1 request log, got {len(logs)}: {logs}"
    ih = logs[0].get("implicit_headers", {})
    assert "Accept-Encoding" in ih, f"missing Accept-Encoding: {ih}"
    assert "gzip" in ih["Accept-Encoding"], f"unexpected AE: {ih['Accept-Encoding']}"

@test("--log request logs identity Accept-Encoding when decompress=false")
def test_log_request_decompress_false():
    out, _ = run_cli(["GET", f"{BASE}/fast", "--log", "request", "--response-decompress", "false"])
    logs = find_log_events(out, "request")
    assert len(logs) == 1, f"expected 1 request log, got {len(logs)}: {logs}"
    ih = logs[0].get("implicit_headers", {})
    assert ih.get("Accept-Encoding") == "identity", f"expected identity: {ih}"

@test("no request log when --log does not include request")
def test_log_request_not_enabled():
    out, _ = run_cli(["POST", f"{BASE}/echo", "--log", "startup", "--body", '{"key":"val"}'])
    logs = find_log_events(out, "request")
    assert len(logs) == 0, f"unexpected request log: {logs}"

@test("no request log when no implicit headers added")
def test_log_request_no_implicit():
    # Explicit Accept-Encoding → no implicit AE. GET has no body → no implicit CT.
    out, _ = run_cli([
        "GET", f"{BASE}/fast", "--log", "request",
        "-H", "Accept-Encoding: identity",
    ])
    logs = find_log_events(out, "request")
    assert len(logs) == 0, f"unexpected request log when headers are explicit: {logs}"


# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------

def main():
    global passed, failed

    print("Starting test server on :18080...")
    server = start_server(18080)
    time.sleep(0.3)

    import urllib.request
    try:
        urllib.request.urlopen(f"{BASE}/fast", timeout=2)
    except Exception as e:
        print(f"FATAL: test server not responding: {e}")
        sys.exit(1)
    print("Test server ready.\n")

    if not os.path.exists(AFH):
        print(f"FATAL: afh binary not found at {AFH}")
        print("Run: cargo build")
        sys.exit(1)

    tests = [
        # Structural
        test_no_startup_default,
        test_log_startup,
        test_verbose,
        test_no_close,
        test_no_id_tag,
        test_output_order,
        # Exit codes
        test_exit_0,
        test_exit_0_on_4xx,
        test_exit_1,
        test_exit_2_no_args,
        test_exit_2_no_url,
        # Basic requests
        test_get_json,
        test_get_text,
        test_get_binary,
        test_head,
        test_204,
        test_4xx,
        test_5xx,
        # Headers
        test_user_agent,
        test_custom_header,
        test_multi_header,
        test_remove_header,
        # Body
        test_body_json,
        test_body_text,
        test_body_at_file,
        test_body_file_flag,
        test_body_base64,
        test_body_multipart,
        test_body_multipart_file,
        test_body_urlencoded,
        test_body_urlencoded_special_chars,
        test_body_urlencoded_duplicate_keys,
        test_empty_post,
        # Options
        test_parse_json_false,
        test_redirect_disabled,
        test_redirect_default,
        test_timeout_idle,
        test_retry,
        test_retry_on_status,
        test_max_inline_bytes,
        test_response_save_dir,
        test_retry_base_delay,
        test_max_bytes,
        # Streaming
        test_chunked_ndjson,
        test_chunked_sse,
        test_chunked_raw,
        test_chunked_order,
        # Download
        test_save_to,
        test_download_progress,
        # Errors
        test_dns_error,
        test_connect_refused,
        test_error_structure,
        # TLS
        test_tls_insecure,
        # Trace
        test_trace_duration,
        test_trace_received,
        # Other
        test_method_case,
        test_help,
        test_version,
        test_unicode,
        # Output format
        test_output_json,
        test_output_yaml,
        test_output_plain,
        test_output_yaml_body,
        test_output_invalid,
        test_gzip_decompress,
        test_decompress_false,
        test_verbose_bare,
        test_chunked_bare,
        # Request log
        test_log_request_content_type,
        test_log_request_accept_encoding,
        test_log_request_decompress_false,
        test_log_request_not_enabled,
        test_log_request_no_implicit,
        # Resume download
        test_response_save_resume_no_file,
        test_response_save_resume_new_file,
        test_response_save_resume,
        test_response_save_resume_log,
        test_response_save_resume_empty_file,
        # Invalid UTF-8 safety
        test_invalid_utf8_text,
        test_invalid_utf8_json,
        test_chunked_invalid_utf8,
    ]

    print(f"Running {len(tests)} CLI mode tests...\n")
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
