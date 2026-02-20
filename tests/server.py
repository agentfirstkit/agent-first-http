"""
Test HTTP server for afhttp stress tests.
Endpoints with configurable size, delay, streaming, redirects, errors.
"""

import http.server
import json
import re
import socketserver
import sys
import threading
import time
import os


class ThreadedHTTPServer(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True
    # Shared state for stateful endpoints
    retry_counters = {}
    retry_lock = threading.Lock()


class TestHandler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass  # suppress request logs

    def read_request_body(self):
        """Read request body, handling both Content-Length and chunked TE."""
        te = self.headers.get("Transfer-Encoding", "")
        if "chunked" in te.lower():
            body = b""
            while True:
                size_line = self.rfile.readline().strip()
                if not size_line:
                    break
                chunk_size = int(size_line, 16)
                if chunk_size == 0:
                    # Read trailing headers/CRLF
                    while True:
                        trailer = self.rfile.readline().strip()
                        if not trailer:
                            break
                    break
                body += self.rfile.read(chunk_size)
                self.rfile.readline()  # consume trailing CRLF
            return body
        else:
            length = int(self.headers.get("Content-Length", 0))
            return self.rfile.read(length) if length > 0 else b""

    def do_GET(self):
        path = self.path

        # /fast — immediate small JSON response
        if path == "/fast":
            body = json.dumps({"ok": True, "ts": time.time()}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        # /delay/<ms> — delayed response
        m = re.match(r"/delay/(\d+)", path)
        if m:
            ms = int(m.group(1))
            time.sleep(ms / 1000.0)
            body = json.dumps({"delayed_ms": ms}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        # /size/<n> — response body of n bytes
        m = re.match(r"/size/(\d+)", path)
        if m:
            n = int(m.group(1))
            body = b"X" * n
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(n))
            self.end_headers()
            self.wfile.write(body)
            return

        # /text/<n> — text response of n bytes
        m = re.match(r"/text/(\d+)", path)
        if m:
            n = int(m.group(1))
            body = ("A" * n).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(n))
            self.end_headers()
            self.wfile.write(body)
            return

        # /json/<n> — JSON response with n-char value
        m = re.match(r"/json/(\d+)", path)
        if m:
            n = int(m.group(1))
            body = json.dumps({"data": "Z" * n}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        # /status/<code> — return specific status code
        m = re.match(r"/status/(\d+)", path)
        if m:
            code = int(m.group(1))
            if code == 204:
                self.send_response(204)
                self.end_headers()
                return
            body = json.dumps({"status": code}).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        # /empty — 204 No Content
        if path == "/empty":
            self.send_response(204)
            self.end_headers()
            return

        # /headers — echo request headers as JSON
        if path == "/headers":
            headers = {k: v for k, v in self.headers.items()}
            body = json.dumps(headers).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        # /redirect/<n> — redirect n times, then 200
        m = re.match(r"/redirect/(\d+)", path)
        if m:
            n = int(m.group(1))
            if n > 0:
                self.send_response(302)
                self.send_header("Location", f"/redirect/{n-1}")
                self.send_header("Content-Length", "0")
                self.end_headers()
            else:
                body = json.dumps({"redirected": True}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            return

        # /stream/sse/<n>/<delay_ms> — SSE stream with n events
        m = re.match(r"/stream/sse/(\d+)/(\d+)", path)
        if m:
            n = int(m.group(1))
            delay_ms = int(m.group(2))
            # Pre-build full body to set Content-Length, but flush in pieces
            events = [f"data: {{\"i\":{i}}}\n\n" for i in range(n)]
            full_body = "".join(events).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(full_body)))
            self.end_headers()
            for event_str in events:
                self.wfile.write(event_str.encode())
                self.wfile.flush()
                if delay_ms > 0:
                    time.sleep(delay_ms / 1000.0)
            return

        # /stream/ndjson/<n>/<delay_ms> — NDJSON stream with n lines
        m = re.match(r"/stream/ndjson/(\d+)/(\d+)", path)
        if m:
            n = int(m.group(1))
            delay_ms = int(m.group(2))
            lines = [json.dumps({"seq": i}) + "\n" for i in range(n)]
            full_body = "".join(lines).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/x-ndjson")
            self.send_header("Content-Length", str(len(full_body)))
            self.end_headers()
            for line_str in lines:
                self.wfile.write(line_str.encode())
                self.wfile.flush()
                if delay_ms > 0:
                    time.sleep(delay_ms / 1000.0)
            return

        # /binary/<n> — n bytes of binary data (non-UTF8)
        m = re.match(r"/binary/(\d+)", path)
        if m:
            n = int(m.group(1))
            body = bytes(range(256)) * (n // 256 + 1)
            body = body[:n]
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(n))
            self.end_headers()
            self.wfile.write(body)
            return

        # /hang — never responds (for timeout testing)
        if path == "/hang":
            time.sleep(120)
            return

        # /multi-header — response with multiple Set-Cookie headers
        if path == "/multi-header":
            body = json.dumps({"ok": True}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Set-Cookie", "a=1")
            self.send_header("Set-Cookie", "b=2")
            self.send_header("X-Multi", "first")
            self.send_header("X-Multi", "second")
            self.end_headers()
            self.wfile.write(body)
            return

        # /gzip — gzip-compressed JSON response
        if path == "/gzip":
            import gzip as gzip_mod
            raw = json.dumps({"compressed": True, "message": "hello from gzip"}).encode()
            body = gzip_mod.compress(raw)
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Encoding", "gzip")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        # /unicode — response with unicode content
        if path == "/unicode":
            body = json.dumps({"text": "你好世界 🌍 café résumé"}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        # /disconnect/<n> — send n bytes then close connection abruptly
        m = re.match(r"/disconnect/(\d+)", path)
        if m:
            n = int(m.group(1))
            # Claim a larger body than we'll send
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(n * 10))  # lie about size
            self.end_headers()
            self.wfile.write(b"X" * n)
            self.wfile.flush()
            # Force close the socket without sending the rest
            self.connection.close()
            return

        # /stream/disconnect/<n>/<delay_ms> — stream n chunks then disconnect
        m = re.match(r"/stream/disconnect/(\d+)/(\d+)", path)
        if m:
            n = int(m.group(1))
            delay_ms = int(m.group(2))
            total_data = "".join(f"line {i}\n" for i in range(n + 5))
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(total_data)))
            self.end_headers()
            for i in range(n):
                try:
                    self.wfile.write(f"line {i}\n".encode())
                    self.wfile.flush()
                except Exception:
                    return
                if delay_ms > 0:
                    time.sleep(delay_ms / 1000.0)
            # Disconnect abruptly
            self.connection.close()
            return

        # /rate-limit/<n> — return 429 with Retry-After header
        m = re.match(r"/rate-limit/(\d+)", path)
        if m:
            n = int(m.group(1))
            body = json.dumps({"error": "rate_limited", "retry_after": n}).encode()
            self.send_response(429)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Retry-After", str(n))
            self.end_headers()
            self.wfile.write(body)
            return

        # /huge-headers — response with many headers
        if path == "/huge-headers":
            body = json.dumps({"ok": True}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            for i in range(50):
                self.send_header(f"X-Header-{i}", f"value-{i}-{'x' * 100}")
            self.end_headers()
            self.wfile.write(body)
            return

        # /slow-body/<total_bytes>/<chunk_size>/<delay_ms> — slow body delivery
        m = re.match(r"/slow-body/(\d+)/(\d+)/(\d+)", path)
        if m:
            total = int(m.group(1))
            chunk_size = int(m.group(2))
            delay_ms = int(m.group(3))
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(total))
            self.end_headers()
            sent = 0
            while sent < total:
                to_send = min(chunk_size, total - sent)
                try:
                    self.wfile.write(b"D" * to_send)
                    self.wfile.flush()
                except Exception:
                    return
                sent += to_send
                if delay_ms > 0 and sent < total:
                    time.sleep(delay_ms / 1000.0)
            return

        # /retry-succeed/<key>/<fail_count> — return 429 for first fail_count calls, then 200
        m = re.match(r"/retry-succeed/([^/]+)/(\d+)", path)
        if m:
            key = m.group(1)
            fail_count = int(m.group(2))
            with self.server.retry_lock:
                count = self.server.retry_counters.get(key, 0)
                self.server.retry_counters[key] = count + 1
            if count < fail_count:
                body = json.dumps({"error": "rate_limited", "attempt": count}).encode()
                self.send_response(429)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Retry-After", "0")
                self.end_headers()
                self.wfile.write(body)
            else:
                body = json.dumps({"ok": True, "attempts": count + 1}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            return

        # /range-file/<n> — supports Range requests for resume testing
        m = re.match(r"/range-file/(\d+)", path)
        if m:
            n = int(m.group(1))
            full_body = b"X" * n
            range_header = self.headers.get("Range", "")
            if range_header.startswith("bytes="):
                try:
                    start = int(range_header[6:].rstrip("-"))
                    partial = full_body[start:]
                    self.send_response(206)
                    self.send_header("Content-Type", "application/octet-stream")
                    self.send_header("Content-Range", f"bytes {start}-{n-1}/{n}")
                    self.send_header("Content-Length", str(len(partial)))
                    self.end_headers()
                    self.wfile.write(partial)
                except (ValueError, IndexError):
                    self.send_response(400)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
            else:
                self.send_response(200)
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Content-Length", str(n))
                self.end_headers()
                self.wfile.write(full_body)
            return

        # /invalid-utf8/text — latin-1 bytes with text/plain (not valid UTF-8)
        if path == "/invalid-utf8/text":
            body = b"caf\xe9 r\xe9sum\xe9"  # "café résumé" in latin-1
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        # /invalid-utf8/json — invalid UTF-8 prefix with application/json
        if path == "/invalid-utf8/json":
            body = b"\xff\xfe{\"key\":\"value\"}"  # BOM + JSON-like, invalid UTF-8
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        # /stream/invalid-utf8 — stream of \n-delimited chunks with invalid UTF-8 bytes
        if path == "/stream/invalid-utf8":
            chunks = [b"chunk1:\xff\xfe", b"chunk2:\xfd\xfc", b"chunk3:\xe9\xf8"]
            body = b"\n".join(chunks) + b"\n"
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        self.send_response(404)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_POST(self):
        path = self.path

        # /echo — echo request body and headers
        if path == "/echo":
            body = self.read_request_body()
            ct = self.headers.get("Content-Type", "")
            resp = {
                "content_type": ct,
                "body_length": len(body),
                "headers": {k: v for k, v in self.headers.items()},
            }
            # Try to include body as string or note it's binary
            try:
                resp["body"] = body.decode("utf-8")
            except UnicodeDecodeError:
                resp["body_binary"] = True
            resp_body = json.dumps(resp).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(resp_body)))
            self.end_headers()
            self.wfile.write(resp_body)
            return

        # /echo-urlencoded — parse and echo application/x-www-form-urlencoded body
        if path == "/echo-urlencoded":
            from urllib.parse import parse_qsl
            body = self.read_request_body()
            ct = self.headers.get("Content-Type", "")
            pairs = parse_qsl(body.decode("utf-8"), keep_blank_values=True)
            resp = {
                "content_type": ct,
                "fields": [{"name": k, "value": v} for k, v in pairs],
            }
            resp_body = json.dumps(resp).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(resp_body)))
            self.end_headers()
            self.wfile.write(resp_body)
            return

        # /echo-multipart — echo multipart parts info
        if path == "/echo-multipart":
            body = self.read_request_body()
            ct = self.headers.get("Content-Type", "")
            resp = {
                "content_type": ct,
                "body_length": len(body),
                "has_multipart": "multipart" in ct.lower(),
            }
            resp_body = json.dumps(resp).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(resp_body)))
            self.end_headers()
            self.wfile.write(resp_body)
            return

        self.send_response(404)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_HEAD(self):
        if self.path == "/head-test":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", "1000")
            self.end_headers()
            return
        self.send_response(404)
        self.end_headers()


def start_server(port=18080):
    server = ThreadedHTTPServer(("127.0.0.1", port), TestHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 18080
    server = ThreadedHTTPServer(("127.0.0.1", port), TestHandler)
    print(f"Test server on http://127.0.0.1:{port}", file=sys.stderr)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        server.shutdown()
