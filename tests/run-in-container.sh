#!/usr/bin/env bash
# Run the agent-first-http test environment under the platform's explicit
# container backend. Local macOS uses Apple `container`; CI uses Docker.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SPORE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

resolve_runtime() {
  local requested="${AFHTTP_TEST_CONTAINER_RUNTIME:-}"
  if [ -z "$requested" ]; then
    if [ "${CI:-}" = "true" ]; then
      requested="docker"
    elif [ "$(uname -s)" = "Darwin" ]; then
      requested="apple"
    else
      requested="docker"
    fi
  fi

  case "$requested" in
    apple)
      TEST_CONTAINER_RUNTIME="apple"
      TEST_CONTAINER_BIN="container"
      ;;
    docker)
      TEST_CONTAINER_RUNTIME="docker"
      TEST_CONTAINER_BIN="docker"
      ;;
    *)
      echo "AFHTTP_TEST_CONTAINER_RUNTIME must be apple or docker" >&2
      exit 2
      ;;
  esac

  if ! command -v "$TEST_CONTAINER_BIN" >/dev/null 2>&1; then
    echo "$TEST_CONTAINER_BIN is required for the $TEST_CONTAINER_RUNTIME test runtime" >&2
    exit 2
  fi
}

resolve_runtime

if [ "$TEST_CONTAINER_RUNTIME" = "apple" ]; then
  container system start >/dev/null
else
  docker info >/dev/null
  # Required so BuildKit honors tests/Dockerfile.test.dockerignore.
  export DOCKER_BUILDKIT=1
fi

IMAGE_TAG="${AFHTTP_TEST_CONTAINER_IMAGE:-${AFHTTP_DOCKER_IMAGE:-afhttp-test:local}}"
CAMOUFOX_BUILD="${AFHTTP_TEST_INSTALL_CAMOUFOX:-0}"
IMAGE_STAMP="$SPORE_DIR/target/.container-image-stamp-${TEST_CONTAINER_RUNTIME}-camoufox-${CAMOUFOX_BUILD}"
DOCKERFILE="$SPORE_DIR/tests/Dockerfile.test"
BUILD_INPUTS=(
  "$DOCKERFILE"
  "$SPORE_DIR/container/docker/install-backends.sh"
  "$SPORE_DIR/container/docker/install-browser-policy.sh"
)

NEEDS_BUILD=1
if [ -f "$IMAGE_STAMP" ] && "$TEST_CONTAINER_BIN" image inspect "$IMAGE_TAG" >/dev/null 2>&1; then
  NEEDS_BUILD=0
  for build_input in "${BUILD_INPUTS[@]}"; do
    if [ ! "$build_input" -ot "$IMAGE_STAMP" ]; then
      NEEDS_BUILD=1
      break
    fi
  done
fi

if [ "$NEEDS_BUILD" = "1" ]; then
  echo "==> $TEST_CONTAINER_BIN build $IMAGE_TAG (recipe changed or image missing)" >&2
  if [ "$TEST_CONTAINER_RUNTIME" = "apple" ]; then
    container build \
      --platform linux/arm64 \
      --cpus "${AFHTTP_TEST_BUILDER_CPUS:-8}" \
      --memory "${AFHTTP_TEST_BUILDER_MEMORY:-16g}" \
      --build-arg "AFHTTP_TEST_INSTALL_CAMOUFOX=$CAMOUFOX_BUILD" \
      -t "$IMAGE_TAG" \
      -f "$DOCKERFILE" \
      "$SPORE_DIR"
  else
    docker build \
      --build-arg "AFHTTP_TEST_INSTALL_CAMOUFOX=$CAMOUFOX_BUILD" \
      -t "$IMAGE_TAG" \
      -f "$DOCKERFILE" \
      "$SPORE_DIR"
  fi
  mkdir -p "$SPORE_DIR/target"
  touch "$IMAGE_STAMP"
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
  "${SIBLING_MOUNTS[@]}" \
  -v afhttp-cargo-home:/cargo-home \
  -v afhttp-cargo-target:/cargo-target \
  -w /workspace \
  "$IMAGE_TAG" \
  "$@"
