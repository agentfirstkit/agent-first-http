#!/bin/bash
# Run tests for agent-first-http

set -e
ROOTPATH="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "Testing agent-first-http..."
echo ""

echo "[1/6] Rust fmt"
(cd "$ROOTPATH" && cargo fmt --all --check)

echo ""
echo "[2/6] Rust build"
(cd "$ROOTPATH" && cargo build)

echo ""
echo "[3/6] Rust clippy"
(cd "$ROOTPATH" && cargo clippy -- -D warnings)

echo ""
echo "[4/6] Pipe mode stress"
(cd "$ROOTPATH" && python3 tests/stress.py)

echo ""
echo "[5/6] CLI mode stress"
(cd "$ROOTPATH" && python3 tests/cli_stress.py)

echo ""
echo "[6/6] WebSocket stress"
(cd "$ROOTPATH" && python3 tests/ws_stress.py)

echo ""
echo "All tests passed!"
