#!/usr/bin/env bash
# Drive a real KasmVNC takeover through `afui session serve` and report on it.
#
# The takeover panel is the hardest thing the AFUI remote shell has to carry:
# the content is opaque (a VNC canvas), the transport is RFB over WebSocket, the
# credential is minted by afhttp rather than by AFUI, and the whole thing is
# already behind one reverse proxy before the shell adds a second. This runs all
# of it for real — Xvnc, a headful browser on its display, afhttp's listener,
# the announced session, the shell, and a Chromium looking at the result — and
# prints what it observed rather than only whether it passed.
#
# Needs a checkout of agent-first-ui beside this repository, because the shell
# being tested is that crate's `afui session serve`.
#
# Usage:
#   tests/takeover-in-workbench.sh
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SPORE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

if [ ! -d "$SPORE_DIR/../agent-first-ui" ]; then
  echo "agent-first-ui is not checked out next to this repository;" >&2
  echo "this harness drives its \`afui session serve\` and cannot run without it." >&2
  exit 2
fi

exec "$SCRIPT_DIR/run-in-docker.sh" python3 tests/takeover_in_workbench.py "$@"
