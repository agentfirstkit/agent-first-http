#!/usr/bin/env bash
# Run the agent-first-http test suite inside the test container.
#
#   tests/test.sh                       # default gates only (no browser)
#   tests/test.sh static                # fmt + build + clippy
#   tests/test.sh unit                  # lib + bin unit tests
#   tests/test.sh integration           # full browser-integration suite
#   tests/test.sh ops                   # ops-panel chromium tests (serialized)
#   tests/test.sh takeover              # display-takeover proxy + KasmVNC smoke
#   tests/test.sh coverage              # cargo-llvm-cov with gate
#   tests/test.sh all                   # static + unit + integration + coverage
#   tests/test.sh release               # everything: all + ops + takeover (release gate)
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MODE="${1:-default}"

run() { "$SCRIPT_DIR/run-in-docker.sh" bash -c "$1"; }

case "$MODE" in
  static)
    run "cargo fmt --all --check"
    run "cargo build --all-features"
    run "cargo build --no-default-features --features sdk"
    run "cargo clippy --all-features --all-targets -- -D warnings"
    run "cargo clippy --no-default-features --features sdk --all-targets -- -D warnings"
    ;;
  unit)
    run "cargo test --lib --bins --features sdk,cli"
    run "bash tests/check_regressions.sh"
    ;;
  integration)
    run "cargo test --all-features \
      --test fetch_http_only \
      --test health_capabilities \
      --test cdp_proxy \
      --test browser_fetch \
      --test chrome_shell_capabilities \
      --test fingerprint_chromium \
      --test lightpanda_backend \
      --test camoufox_foxbridge \
      --test cookie_jar \
      --test cookie_jar_isolation \
      --test retry_backoff \
      --test tabs_management \
      --test proxy_tls \
      --test env_isolation \
      --test display_takeover \
      --test host_extras \
      --test network_artifact \
      --test storage_artifact \
      --test unix_listener"
    ;;
  ops)
    # Ops-panel tests spawn chromium and are marked #[ignore] in the
    # default suite because chromiumoxide leaks chromium children
    # between tests under Docker resource pressure. Run them here in
    # isolation. ops_panel_two_browser spawns *two* chromiums per test
    # (a target + an operator), so the serialized run is mandatory.
    run "cargo test --all-features \
      --test ops_panel_live \
      --test ops_panel_two_browser \
      -- --ignored --test-threads=1"
    ;;
  takeover)
    run "cargo test --all-features --test display_takeover -- --test-threads=1"
    run "cargo test --all-features --test display_takeover -- --ignored --test-threads=1"
    ;;
  coverage)
    run "cargo llvm-cov --all-features --tests --fail-under-lines 68 --fail-under-regions 65"
    ;;
  default)
    run "cargo fmt --all --check"
    run "cargo build --all-features"
    run "cargo build --no-default-features --features sdk"
    run "cargo clippy --all-features --all-targets -- -D warnings"
    run "cargo test --lib --bins --features sdk,cli"
    ;;
  all)
    "$0" static
    "$0" unit
    "$0" integration
    "$0" coverage
    ;;
  release)
    # The full release gate: everything, including the flaky-by-design
    # ops/takeover suites that `all` deliberately omits.
    "$0" all
    "$0" ops
    "$0" takeover
    ;;
  *)
    echo "Usage: $0 [static|unit|integration|ops|takeover|coverage|default|all|release]" >&2
    exit 2
    ;;
esac
