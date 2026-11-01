#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# Run the agent-first-http test environment under the platform's explicit
# container backend. Automatic selection is Apple, Podman, then Docker.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SPORE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

REFRESH_IMAGE="${AFHTTP_TEST_REFRESH_IMAGE:-0}"
case "$REFRESH_IMAGE" in
  0|1) ;;
  *) echo 'AFHTTP_TEST_REFRESH_IMAGE must be 0 or 1' >&2; exit 2 ;;
esac

# shellcheck source=../scripts/container-runtime.sh
source "$SPORE_DIR/scripts/container-runtime.sh"
af_container_resolve "${AFHTTP_TEST_CONTAINER_RUNTIME:-}"
TEST_CONTAINER_RUNTIME="$CONTAINER_RUNTIME"
TEST_CONTAINER_BIN="$CONTAINER_BIN"
af_container_start >/dev/null
if [ "$TEST_CONTAINER_RUNTIME" = docker ]; then
  export DOCKER_BUILDKIT=1
fi

IMAGE_TAG="${AFHTTP_TEST_CONTAINER_IMAGE:-${AFHTTP_DOCKER_IMAGE:-afhttp-test:local}}"
CAMOUFOX_BUILD="${AFHTTP_TEST_INSTALL_CAMOUFOX:-0}"
IMAGE_KEY="$(printf '%s' "$IMAGE_TAG" | cksum)"
IMAGE_KEY="${IMAGE_KEY%% *}"
IMAGE_STAMP="$SPORE_DIR/target/.container-image-stamp-${TEST_CONTAINER_RUNTIME}-${IMAGE_KEY}"
DOCKERFILE="$SPORE_DIR/tests/Dockerfile.test"
BUILD_INPUTS=(
  "$DOCKERFILE"
  "$DOCKERFILE.dockerignore"
  "$SPORE_DIR/tests/container-entrypoint.sh"
  "$SPORE_DIR/container/docker/install-backends.sh"
  "$SPORE_DIR/container/docker/install-browser-policy.sh"
)

BUILD_KEY="camoufox=$CAMOUFOX_BUILD
$(cksum "${BUILD_INPUTS[@]}")"
NEEDS_BUILD=1
if [ "$REFRESH_IMAGE" = 0 ] && [ -f "$IMAGE_STAMP" ] \
    && [ "$(cat "$IMAGE_STAMP")" = "$BUILD_KEY" ] \
    && "$TEST_CONTAINER_BIN" image inspect "$IMAGE_TAG" >/dev/null 2>&1; then
  NEEDS_BUILD=0
fi

if [ "$NEEDS_BUILD" = "1" ]; then
  BUILD_FLAGS=(--pull)
  if [ "$REFRESH_IMAGE" = 1 ]; then
    BUILD_FLAGS+=(--no-cache)
  fi
  echo "==> $TEST_CONTAINER_BIN build $IMAGE_TAG (refresh requested, recipe changed, or image missing)" >&2
  if [ "$TEST_CONTAINER_RUNTIME" = "apple" ]; then
    container build \
      "${BUILD_FLAGS[@]}" \
      --platform linux/arm64 \
      --cpus "${AFHTTP_TEST_BUILDER_CPUS:-8}" \
      --memory "${AFHTTP_TEST_BUILDER_MEMORY:-16g}" \
      --build-arg "AFHTTP_TEST_INSTALL_CAMOUFOX=$CAMOUFOX_BUILD" \
      -t "$IMAGE_TAG" \
      -f "$DOCKERFILE" \
      "$SPORE_DIR"
  else
    "$TEST_CONTAINER_BIN" build \
      "${BUILD_FLAGS[@]}" \
      --build-arg "AFHTTP_TEST_INSTALL_CAMOUFOX=$CAMOUFOX_BUILD" \
      -t "$IMAGE_TAG" \
      -f "$DOCKERFILE" \
      "$SPORE_DIR"
  fi
  mkdir -p "$SPORE_DIR/target"
  printf '%s\n' "$BUILD_KEY" > "$IMAGE_STAMP"
fi

RUN_FLAGS=(--rm)
if [ -t 0 ] && [ -t 1 ]; then
  RUN_FLAGS+=(-it)
fi
if [ "$TEST_CONTAINER_RUNTIME" = "apple" ]; then
  RUN_FLAGS+=(
    --cpus "${AFHTTP_TEST_CONTAINER_CPUS:-8}"
    --memory "${AFHTTP_TEST_CONTAINER_MEMORY:-16g}"
  )
fi
RUN_FLAGS+=(-e CARGO_HOME=/cargo-home -e CARGO_TARGET_DIR=/cargo-target)

TEST_CONTAINER_USER="${AFHTTP_TEST_CONTAINER_USER:-${AFHTTP_DOCKER_USER:-}}"
if [ -n "$TEST_CONTAINER_USER" ]; then
  RUN_FLAGS+=(--user "$TEST_CONTAINER_USER")
fi

SIBLING_MOUNTS=()
for sibling in agent-first-data agent-first-ui agent-first-slug; do
  if [ -d "$SPORE_DIR/../$sibling" ]; then
    SIBLING_MOUNTS+=(-v "$SPORE_DIR/../$sibling:/$sibling")
  fi
done

exec "$TEST_CONTAINER_BIN" run "${RUN_FLAGS[@]}" \
  -v "$SPORE_DIR:/workspace" \
  ${SIBLING_MOUNTS[@]+"${SIBLING_MOUNTS[@]}"} \
  -v afhttp-cargo-home:/cargo-home \
  -v afhttp-cargo-target:/cargo-target \
  -w /workspace \
  "$IMAGE_TAG" \
  "$@"
