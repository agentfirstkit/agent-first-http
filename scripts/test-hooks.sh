#!/bin/bash

cmn_test_usage() {
  echo "Usage: $(cmn_script_invocation) [static|unit|e2e|coverage|all]" >&2
}

cmn_test_run_static() {
  echo "[static] fmt/build/clippy"
  cmn_cargo fmt --all --check
  cmn_cargo build
  cmn_cargo clippy -- -D warnings
}

cmn_test_run_unit() {
  echo "[unit+component] Rust tests"
  cmn_cargo test --bin afhttp
  "$(cmn_project_root)/scripts/check_regressions.sh"
}

cmn_test_run_e2e() {
  echo "[e2e] stress suites"
  cmn_cargo build
  (
    cd "$(cmn_project_root)"
    python3 tests/stress.py
    python3 tests/cli_stress.py
    python3 tests/ws_stress.py
  )
}

cmn_test_run_coverage() {
  echo "[coverage] gate"
  (
    cd "$(cmn_project_root)"
    python3 scripts/coverage_gate.py
  )
}

cmn_test_run_all() {
  cmn_test_run_static
  cmn_test_run_unit
  cmn_test_run_e2e
  cmn_test_run_coverage
}
