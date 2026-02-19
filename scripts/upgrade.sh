#!/bin/bash
# Upgrade dependencies to latest versions for agent-first-http

set -e
ROOTPATH="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "Upgrading agent-first-http dependencies..."
echo ""

if ! cargo upgrade -V >/dev/null 2>&1; then
  echo "cargo upgrade not found; installing cargo-edit..."
  cargo install cargo-edit
fi

echo "[1/1] Rust - cargo upgrade"
(cd "$ROOTPATH" && cargo upgrade --incompatible && cargo update)

echo ""
echo "Upgrade complete!"
