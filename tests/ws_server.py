"""
WebSocket test server for afh WebSocket tests.
Requires: pip install websockets  (>=10)
"""

import asyncio
import json
import re
import threading

WS_PORT = 18081
WS_BASE = f"ws://127.0.0.1:{WS_PORT}"


async def handler(websocket):
    path = websocket.request.path

    # /ws/push/<n> — send n JSON messages then close
    m = re.match(r"^/ws/push/(\d+)$", path)
    if m:
        n = int(m.group(1))
        for i in range(n):
            await websocket.send(json.dumps({"seq": i}))
        return  # handler return → server closes connection

    # /ws/push/<n>/<delay_ms> — send n messages with inter-message delay
    m = re.match(r"^/ws/push/(\d+)/(\d+)$", path)
    if m:
        n, delay_ms = int(m.group(1)), int(m.group(2))
        for i in range(n):
            await websocket.send(json.dumps({"seq": i}))
            if i < n - 1:
                await asyncio.sleep(delay_ms / 1000.0)
        return

    # /ws/echo — echo every message back until client closes
    if path == "/ws/echo":
        async for message in websocket:
            await websocket.send(message)
        return

    # /ws/binary — send one 16-byte binary frame then close
    if path == "/ws/binary":
        await websocket.send(bytes(range(16)))
        return

    # /ws/headers — echo the HTTP upgrade request headers as a JSON text message
    if path == "/ws/headers":
        headers = {k: v for k, v in websocket.request.headers.items()}
        await websocket.send(json.dumps(headers))
        return

    # Unknown path — reject
    await websocket.close(1008, "unknown path")


def start_ws_server(port: int = WS_PORT) -> threading.Thread:
    """Start WS test server in a daemon thread. Blocks until server is ready."""
    import websockets

    ready = threading.Event()

    async def _main():
        async with websockets.serve(handler, "127.0.0.1", port, ping_interval=None):
            ready.set()
            await asyncio.Future()  # run until daemon thread is killed

    def _run():
        asyncio.run(_main())

    thread = threading.Thread(target=_run, daemon=True)
    thread.start()
    if not ready.wait(timeout=5):
        raise RuntimeError("WebSocket test server failed to start")
    return thread


if __name__ == "__main__":
    import sys
    import websockets

    port = int(sys.argv[1]) if len(sys.argv) > 1 else WS_PORT
    print(f"WebSocket test server on ws://127.0.0.1:{port}", file=sys.stderr)

    async def _serve():
        async with websockets.serve(handler, "127.0.0.1", port, ping_interval=None):
            await asyncio.Future()

    asyncio.run(_serve())
