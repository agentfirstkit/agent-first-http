#!/usr/bin/env bash
# Deprecated compatibility name. Runtime selection still follows the canonical
# entrypoint: Apple container locally on macOS, Docker in CI.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/run-in-container.sh" "$@"
