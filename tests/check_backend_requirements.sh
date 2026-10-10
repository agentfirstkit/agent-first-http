#!/usr/bin/env bash
# Exercise real integration binaries with explicit missing backend paths.
set -euo pipefail
cd "$(dirname "$0")/.."
output_file=$(mktemp)
trap 'rm -f "$output_file"' EXIT
check_missing() {
  local backend="$1" target="$2" test_name="$3" bin_env="$4"
  if env AFHTTP_TEST_REQUIRE_BACKENDS="$backend" "$bin_env=/nonexistent/afhttp-test-backend" \
    cargo test --all-features --test "$target" "$test_name" -- --exact --nocapture >"$output_file" 2>&1; then
    cat "$output_file"
    echo "missing required $backend silently passed" >&2
    return 1
  fi
  if ! grep -Fq "required backend $backend is missing" "$output_file"; then
    cat "$output_file"
    echo "required backend failure did not exercise the missing-backend guard" >&2
    return 1
  fi
}
check_missing chromium browser_fetch render_always_returns_rendered_html_and_artifacts AFHTTP_TEST_BROWSER_BIN
check_missing lightpanda lightpanda_backend lightpanda_launches_and_exposes_subset_capability_matrix AFHTTP_TEST_LIGHTPANDA_BIN
check_missing fingerprint-chromium fingerprint_chromium fingerprint_chromium_reports_family_in_capabilities AFHTTP_TEST_FINGERPRINT_CHROMIUM_BIN
check_missing camoufox camoufox_foxbridge camoufox_launches_and_exposes_subset_capability_matrix AFHTTP_TEST_FOXBRIDGE_BIN
# An optional backend still follows the explicit skip contract.
env AFHTTP_TEST_REQUIRE_BACKENDS= AFHTTP_TEST_BROWSER_BIN=/nonexistent/afhttp-test-backend \
  cargo test --all-features --test browser_fetch render_always_returns_rendered_html_and_artifacts -- --exact --nocapture >"$output_file" 2>&1
grep -Fq '(skipping:' "$output_file"
echo 'required-backend failures and optional skip verified'
