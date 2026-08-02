#!/usr/bin/env bash
# Run the agent-first-http test suite inside the test container.
#
#   tests/test.sh                       # default gates only (no browser)
#   tests/test.sh static                # fmt + build + clippy
#   tests/test.sh unit                  # lib + bin unit tests
#   tests/test.sh integration           # full browser-integration suite
#   tests/test.sh takeover              # display takeover proxy + KasmVNC smoke
#   tests/test.sh all                   # static + unit + integration
#   tests/test.sh release               # everything: all + takeover (release gate)
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MODE="${1:-default}"

run() { "$SCRIPT_DIR/run-in-docker.sh" bash -c "$1"; }

case "$MODE" in
  static)
    run "bash tests/check_container_third_party_versions.sh"
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
    # Chrome-for-Testing ships linux64 only, so `install_chrome_headless_shell`
    # self-skips on other architectures and the two chrome-shell tests then
    # early-return — printing a notice that cargo captures, so the suite reports
    # a clean pass over assertions that never ran. Say so out loud: on arm64 the
    # pinned CHROME_FOR_TESTING_VERSION is exercised only by the x86 CI leg, and
    # a bump verified "green" here has not actually been verified.
    run 'if [ "$(uname -m)" != "x86_64" ]; then
           printf "WARNING: %s is not x86_64 — chrome-headless-shell is absent, so\n" "$(uname -m)" >&2
           printf "         chrome_shell_capabilities asserts nothing in this run.\n" >&2
           printf "         The pinned chrome-for-testing version is covered by CI only.\n" >&2
         fi'
    run "cargo test --all-features \
      --test fetch_http_only \
      --test health_capabilities \
      --test cdp_proxy \
      --test browser_fetch \
      --test chrome_shell_capabilities \
      --test fingerprint_chromium \
      --test lightpanda_backend \
      --test camoufox_foxbridge \
      --test takeover_backends \
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
      --test stream_redirect \
      --test unix_listener"
    ;;
  takeover)
    run "cargo test --all-features --test display_takeover -- --test-threads=1"
    run "cargo test --all-features --test display_takeover -- --ignored --test-threads=1"
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
    ;;
  release)
    # The full release gate: everything, including the flaky-by-design
    # display-takeover suite that `all` deliberately omits.
    "$0" all
    "$0" takeover
    ;;
  *)
    echo "Usage: $0 [static|unit|integration|takeover|default|all|release]" >&2
    exit 2
    ;;
esac
