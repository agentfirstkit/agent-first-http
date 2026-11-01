#!/usr/bin/env bash
# Fixed test entrypoint for agent-first-http: every gate runs inside the test
# container.
#
# The fixed container gate used by the monorepo driver and release gate.
# Native CI legs run the corresponding Cargo checks directly.
#
#   tests/test.sh                       # default gates only (no browser)
#   tests/test.sh static                # shellcheck + fmt + build + clippy
#   tests/test.sh unit                  # lib + bin unit tests
#   tests/test.sh download              # browser download-to-disk tests only
#   tests/test.sh integration           # full browser-integration suite
#   tests/test.sh takeover              # display takeover proxy + KasmVNC smoke
#   tests/test.sh all                   # static + unit + integration
#   tests/test.sh release               # everything: all + takeover (release gate)
set -euo pipefail

_AFDATA_BASH_SOURCE="$("${AFDATA_BIN:-afdata}" shell bash)"
# shellcheck source=/dev/stdin
source /dev/stdin <<<"$_AFDATA_BASH_SOURCE"
unset _AFDATA_BASH_SOURCE

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SPORE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

afdata_args_begin "$0 [MODE]"
afdata_args_positional test_mode MODE \
  "Test mode: static, unit, download, integration, takeover, default, runtime, refresh, versions, all, or release" optional
afdata_args_parse "$@"
MODE="${test_mode:-default}"

# Everything cargo touches goes through the container, so the host needs no Rust
# system deps, no chromium, and no chromiumoxide native libraries. Passthrough,
# not --quiet: these are minutes-long builds and browser suites, and a person
# watching one needs to see it move.
run() { afdata_run "$SCRIPT_DIR/run-in-container.sh" bash -c "$1"; }

# Parsing is not linting, and the release waits on a CI leg that runs this — so
# it has to be able to fail here first. On the host rather than in the
# container: the pinned build is the same one CI provisions, and the scripts it
# reads are the ones that drive the container rather than anything inside it.
run_shellcheck() { afdata_run "$SPORE_DIR/scripts/install-shellcheck.sh" --check "$SPORE_DIR"; }

run_static() {
  run_shellcheck
  afdata_run --quiet python3 "$SCRIPT_DIR/test_container_refresh.py"
  run "python3 tests/check_container_pins.py"
  run "bash tests/check_container_third_party_versions.sh"
  run "cargo fmt --all --check"
  run "cargo build --all-features"
  run "cargo build --no-default-features --features sdk"
  run "bash tests/check_sdk_floor.sh"
  run "cargo clippy --all-features --all-targets -- -D warnings"
  run "cargo clippy --no-default-features --features sdk --all-targets -- -D warnings"
}

run_unit() {
  run "cargo test --lib --bins --test host_token_sources --features sdk,cli"
  run "bash tests/check_regressions.sh"
  run "bash tests/check_backend_requirements.sh"
}

run_integration() {
  run "cargo test --all-features \
    --test fetch_http_only \
    --test browser_request_scope \
    --test health_capabilities \
    --test cdp_proxy \
    --test browser_fetch \
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
    --test ui_takeover \
    --test host_extras \
    --test network_artifact \
    --test storage_artifact \
    --test stream_redirect \
    --test unix_listener"
}

run_takeover() {
  run "cargo test --all-features --test display_takeover -- --test-threads=1"
  run "cargo test --all-features --test display_takeover -- --ignored --test-threads=1"
}

run_default() {
  run "cargo fmt --all --check"
  run "cargo build --all-features"
  run "cargo build --no-default-features --features sdk"
  run "bash tests/check_sdk_floor.sh"
  run "cargo clippy --all-features --all-targets -- -D warnings"
  run "cargo test --lib --bins --features sdk,cli"
}

run_versions() {
  run "rustc --version && chromium --version && lightpanda version"
}

case "$MODE" in
  refresh)
    run_shellcheck
    afdata_run --quiet python3 "$SCRIPT_DIR/test_container_refresh.py"
    AFHTTP_TEST_REFRESH_IMAGE=1 run "true"
    run_versions
    ;;
  versions) run_versions ;;
  static) run_static ;;
  unit) run_unit ;;
  runtime)
    run_shellcheck
    run "cargo fmt --all --check"
    run "cargo clippy --features sdk,cli --lib -- -D warnings"
    run "cargo test --features sdk,cli --lib runtime"
    ;;
  download) run "cargo test --all-features --test browser_fetch browser_navigation_download" ;;
  integration) run_integration ;;
  takeover) run_takeover ;;
  default) run_default ;;
  all)
    run_static
    run_unit
    run_integration
    ;;
  release)
    # The full release gate: everything, including the flaky-by-design
    # display-takeover suite that `all` deliberately omits.
    run_static
    run_unit
    run_integration
    run_takeover
    ;;
  *)
    afdata_error unknown_test_mode \
      "unknown test mode: $MODE" \
      "run one of: static, unit, download, integration, takeover, default, runtime, refresh, versions, all, release" || :
    exit 2
    ;;
esac

afdata_result "agent-first-http $MODE gate complete"
