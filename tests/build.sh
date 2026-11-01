#!/usr/bin/env bash
# Build inside the test container, or use --native for a driver that connects
# to an existing local host. Forwards remaining arguments to cargo.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ "${1:-}" = "--native" ]; then
  shift
  cd "$SCRIPT_DIR/.."
  exec cargo build "$@"
fi
exec "$SCRIPT_DIR/run-in-container.sh" cargo build "$@"
