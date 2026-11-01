#!/bin/sh
# Canonical runtime policy; standalone repositories carry an identical copy.
# Resolve once, then fail on errors from that runtime rather than switching stores.

af_container_available() {
  _af_binary_path="$(command -v "$1")" || return 1
  [ -f "$_af_binary_path" ] && [ -x "$_af_binary_path" ]
}

af_container_resolve() {
  CONTAINER_RUNTIME="${1:-${AF_CONTAINER_RUNTIME:-}}"
  if [ -z "$CONTAINER_RUNTIME" ]; then
    if [ "$(uname -s)" = Darwin ] && af_container_available container; then
      CONTAINER_RUNTIME=apple
    elif af_container_available podman; then
      CONTAINER_RUNTIME=podman
    elif af_container_available docker; then
      CONTAINER_RUNTIME=docker
    else
      printf '%s\n' 'No container runtime found; install Apple container, Podman, or Docker' >&2
      return 2
    fi
  fi
  case "$CONTAINER_RUNTIME" in
    apple | container) CONTAINER_RUNTIME=apple; CONTAINER_BIN=container ;;
    podman | docker) CONTAINER_BIN="$CONTAINER_RUNTIME" ;;
    *) printf '%s\n' 'Container runtime must be apple, podman, or docker' >&2; return 2 ;;
  esac
  if [ "$CONTAINER_RUNTIME" = apple ] && [ "$(uname -s)" != Darwin ]; then
    printf '%s\n' 'Apple container requires macOS' >&2
    return 2
  fi
  af_container_available "$CONTAINER_BIN" || {
    printf '%s\n' "Selected container runtime is unavailable: $CONTAINER_RUNTIME" >&2
    return 2
  }
  printf '%s\n' "Container runtime: $CONTAINER_RUNTIME (Apple → Podman → Docker; explicit selection takes precedence)" >&2
}

af_container_start() {
  if [ "$CONTAINER_RUNTIME" = apple ]; then
    "$CONTAINER_BIN" system start
  else
    "$CONTAINER_BIN" info
  fi
}
