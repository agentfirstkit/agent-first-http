#!/usr/bin/env bash
# Manual ops-panel takeover smoke test.
#
# Runs `examples/manual_screencast_takeover.rs` inside the test container with
# port 9222 forwarded to localhost. Open http://127.0.0.1:9222/ops/screencast in
# your own browser, click the gray box, then press K. The terminal
# walks window.stage 0 → 1 → 2 as your input is replayed to the
# in-container chromium through the ops panel relay.
#
# Counterpart of `tests/test.sh ops`: that test simulates the human
# with a second chromium; this one puts a real human in the loop.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SPORE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
SPORES_DIR="$(cd "$SPORE_DIR/.." && pwd)"
IMAGE_TAG="${AFHTTP_DOCKER_IMAGE:-afhttp-test:local}"

# Reuse run-in-docker.sh's image-build logic so we never duplicate it.
# Running any trivial command makes it build the image if missing.
"$SCRIPT_DIR/run-in-docker.sh" bash -c 'true'

DOCKER_FLAGS=(--rm)
# Only attach a TTY when stdin/stdout are real terminals — running from
# CI, a pipe, or a non-interactive shell otherwise dies with
# "the input device is not a TTY".
if [ -t 0 ] && [ -t 1 ]; then
  DOCKER_FLAGS+=(-it)
fi
DOCKER_FLAGS+=(-e CARGO_HOME=/cargo-home -e CARGO_TARGET_DIR=/cargo-target)
# Forward the host port so a Mac browser can hit the in-container listener.
DOCKER_FLAGS+=(-p 127.0.0.1:9222:9222)

exec docker run "${DOCKER_FLAGS[@]}" \
  -v "$SPORES_DIR:/spores" \
  -v afhttp-cargo-home:/cargo-home \
  -v afhttp-cargo-target:/cargo-target \
  -w /spores/agent-first-http \
  "$IMAGE_TAG" \
  cargo run --features host --example manual_screencast_takeover -- "$@"
