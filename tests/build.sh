#!/usr/bin/env bash
# Build agent-first-http inside the test container. Forwards extra args to cargo.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/run-in-docker.sh" cargo build "$@"
