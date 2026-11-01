#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# Manual real-display takeover smoke test (KasmVNC provider, human in the loop).
#
# Runs `examples/manual_display_takeover.rs` inside the test container
# with port 9222 forwarded to the host. The example takes the REAL
# launch path: it spawns KasmVNC's Xvnc, launches the browser headful on
# that X display, and reverse-proxies the KasmVNC web client under
# afhttp's listener. Open http://127.0.0.1:9222/takeover/panel in your own
# browser and drive the in-container browser with real OS-level input.
#
# Nothing is installed on the host: KasmVNC, the X server, and the
# browser all live in the container; your machine only opens a browser
# tab. Counterpart of `tests/manual-screencast-takeover.sh` (lightweight CDP
# panel) and the automated `tests/test.sh takeover`.
#
# Usage (args pass straight through to the example):
#   tests/manual-display-takeover.sh                 # chromium, ephemeral
#   tests/manual-display-takeover.sh camoufox        # camoufox backend
#   tests/manual-display-takeover.sh chromium work   # persistent profile "work"

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SPORE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
IMAGE_TAG="${AFHTTP_TEST_CONTAINER_IMAGE:-${AFHTTP_DOCKER_IMAGE:-afhttp-test:local}}"

# shellcheck source=../scripts/container-runtime.sh
source "$SPORE_DIR/scripts/container-runtime.sh"
af_container_resolve "${AFHTTP_TEST_CONTAINER_RUNTIME:-}"
export AFHTTP_TEST_CONTAINER_RUNTIME="$CONTAINER_RUNTIME"

# Reuse the generic harness's image-build logic so we never duplicate it.
# Running any trivial command makes it build the image if missing.
"$SCRIPT_DIR/run-in-container.sh" bash -c 'true'

CONTAINER_FLAGS=(--rm)
# Only attach a TTY when stdin/stdout are real terminals — running from
# CI, a pipe, or a non-interactive shell otherwise dies with
# "the input device is not a TTY".
if [ -t 0 ] && [ -t 1 ]; then
  CONTAINER_FLAGS+=(-it)
fi
if [ "$CONTAINER_RUNTIME" = "apple" ]; then
  CONTAINER_FLAGS+=(
    --cpus "${AFHTTP_TEST_CONTAINER_CPUS:-8}"
    --memory "${AFHTTP_TEST_CONTAINER_MEMORY:-16g}"
  )
fi
CONTAINER_FLAGS+=(-e CARGO_HOME=/cargo-home -e CARGO_TARGET_DIR=/cargo-target)
# Forward the host port so a Mac browser can hit the in-container listener.
CONTAINER_FLAGS+=(-p 127.0.0.1:9222:9222)
AFDATA_MOUNT=()
if [ -d "$SPORE_DIR/../agent-first-data" ]; then
  AFDATA_MOUNT=(-v "$SPORE_DIR/../agent-first-data:/agent-first-data")
fi

exec "$CONTAINER_BIN" run "${CONTAINER_FLAGS[@]}" \
  -v "$SPORE_DIR:/workspace" \
  "${AFDATA_MOUNT[@]}" \
  -v afhttp-cargo-home:/cargo-home \
  -v afhttp-cargo-target:/cargo-target \
  -w /workspace \
  "$IMAGE_TAG" \
  cargo run --features host --example manual_display_takeover -- "$@"
