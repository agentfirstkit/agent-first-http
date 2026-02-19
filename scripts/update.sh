#!/bin/bash
# Update lock file for agent-first-http

set -e
ROOTPATH="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "Updating agent-first-http dependencies..."
echo ""

echo "[1/1] Rust - cargo update"
(cd "$ROOTPATH" && cargo update)

echo ""
echo "Update complete!"
