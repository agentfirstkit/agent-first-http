#!/bin/bash
set -euo pipefail

ROOTPATH="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TIER="${1:-all}"

run_static() {
  echo "[static] fmt/build/clippy"
  (cd "$ROOTPATH" && cargo fmt --all --check)
  (cd "$ROOTPATH" && cargo build)
  (cd "$ROOTPATH" && cargo clippy -- -D warnings)
}

run_unit_component() {
  echo "[unit+component] Rust tests"
  (cd "$ROOTPATH" && cargo test --bin afhttp)
  (cd "$ROOTPATH" && ./scripts/check_regressions.sh)
}

run_e2e() {
  echo "[e2e] stress suites"
  (cd "$ROOTPATH" && cargo build)
  (cd "$ROOTPATH" && python3 tests/stress.py)
  (cd "$ROOTPATH" && python3 tests/cli_stress.py)
  (cd "$ROOTPATH" && python3 tests/ws_stress.py)
}

run_coverage_gate() {
  echo "[coverage] gate"
  (cd "$ROOTPATH" && python3 scripts/coverage_gate.py)
}

case "$TIER" in
  static)
    run_static
    ;;
  unit)
    run_unit_component
    ;;
  e2e)
    run_e2e
    ;;
  coverage)
    run_coverage_gate
    ;;
  all)
    run_static
    run_unit_component
    run_e2e
    run_coverage_gate
    ;;
  *)
    echo "Usage: $0 [static|unit|e2e|coverage|all]" >&2
    exit 2
    ;;
esac

echo "Tier '$TIER' passed."
