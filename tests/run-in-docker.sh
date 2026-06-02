#!/usr/bin/env bash
# Entrypoint for every test/build/clippy command in agent-first-http.
#
# All cargo invocations for this spore go through this script so the host never
# needs Rust system deps, chromium, or chromiumoxide native libraries installed.
# Source is bind-mounted into /work, and the cargo registry + build target dirs
# live in named volumes (afhttp-cargo-home, afhttp-cargo-target) so successive
# runs share compilation caches.
#
# Usage:
#   tests/run-in-docker.sh cargo build --all-features
#   tests/run-in-docker.sh cargo test --workspace
#   tests/run-in-docker.sh bash -lc 'chromium --version'
#
# Helpers tests/build.sh and tests/test.sh dispatch here.

set -euo pipefail

# Required so BuildKit honors the co-located tests/Dockerfile.test.dockerignore
# (there is no .dockerignore at the build-context root by design).
export DOCKER_BUILDKIT=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SPORE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
SPORES_DIR="$(cd "$SPORE_DIR/.." && pwd)"
IMAGE_TAG="${AFHTTP_DOCKER_IMAGE:-afhttp-test:local}"

if ! command -v docker >/dev/null 2>&1; then
  echo "docker not on PATH; install Docker to run the agent-first-http test harness" >&2
  exit 2
fi

# Build the image when the Dockerfile or pinned deps changed since the last build.
IMAGE_STAMP="$SPORE_DIR/target/.docker-image-stamp"
DOCKERFILE="$SPORE_DIR/tests/Dockerfile.test"
NEEDS_BUILD=1
if [ -f "$IMAGE_STAMP" ] && docker image inspect "$IMAGE_TAG" >/dev/null 2>&1; then
  if [ "$DOCKERFILE" -ot "$IMAGE_STAMP" ]; then
    NEEDS_BUILD=0
  fi
fi
if [ "$NEEDS_BUILD" = "1" ]; then
  echo "==> docker build $IMAGE_TAG (tests/Dockerfile.test changed or image missing)" >&2
  docker build -t "$IMAGE_TAG" -f "$DOCKERFILE" "$SPORE_DIR"
  mkdir -p "$SPORE_DIR/target"
  touch "$IMAGE_STAMP"
fi

# TTY/interactive flags only when stdin is a terminal — keeps CI happy.
DOCKER_FLAGS=(--rm)
if [ -t 0 ] && [ -t 1 ]; then
  DOCKER_FLAGS+=(-it)
fi

# On macOS Docker Desktop, bind-mount UID mapping is handled by the VM, so we
# run as root inside the container. Linux users who want host-uid file ownership
# can set AFHTTP_DOCKER_USER=$(id -u):$(id -g) before invoking.
DOCKER_FLAGS+=(-e CARGO_HOME=/cargo-home -e CARGO_TARGET_DIR=/cargo-target)
if [ -n "${AFHTTP_DOCKER_USER:-}" ]; then
  DOCKER_FLAGS+=(--user "$AFHTTP_DOCKER_USER")
fi

exec docker run "${DOCKER_FLAGS[@]}" \
  -v "$SPORES_DIR:/spores" \
  -v afhttp-cargo-home:/cargo-home \
  -v afhttp-cargo-target:/cargo-target \
  -w /spores/agent-first-http \
  "$IMAGE_TAG" \
  "$@"
