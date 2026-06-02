#!/usr/bin/env bash
# Verifies that every test name listed in regressions.txt still exists as a
# discoverable test in the test binaries. Runs inside the Docker container
# via tests/test.sh.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REG="$ROOT/tests/regressions.txt"

if [ ! -s "$REG" ]; then
  exit 0
fi

# Discover known tests from both bin/lib and integration test binaries.
list_known() {
  (
    cd "$ROOT"
    cargo test --no-run --quiet --bin afhttp --lib --features sdk,cli >/dev/null 2>&1 || true
    cargo test --bin afhttp --lib --features sdk,cli -- --list 2>/dev/null || true
    cargo test --tests --features sdk,cli -- --list 2>/dev/null || true
  )
}

KNOWN="$(list_known)"

missing=0
while IFS= read -r entry; do
  case "$entry" in ""|\#*) continue ;; esac
  if ! grep -F -q -- "$entry" <<<"$KNOWN"; then
    echo "regression test missing: $entry" >&2
    missing=$((missing + 1))
  fi
done < "$REG"

if [ "$missing" -gt 0 ]; then
  echo "$missing regression test(s) missing" >&2
  exit 1
fi
