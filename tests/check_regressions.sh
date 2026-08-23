#!/usr/bin/env bash
# Verifies that every test name listed in regressions.txt still exists as a
# discoverable test in the test binaries. Runs inside the test container
# via tests/test.sh.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REG="$ROOT/tests/regressions.txt"

if [ ! -s "$REG" ]; then
  exit 0
fi

# Discover known tests from both bin/lib and integration test binaries.
#
# A listing that fails to build lists nothing, and every registered name then
# reads as deleted — which sends a person looking for tests that are still
# there. So a failure here is reported as itself and stops the check, rather
# than being swallowed into a count of missing names.
list_known() {
  local target="$1"
  shift
  if ! cargo test "$@" --features sdk,cli -- --list 2>"$LIST_ERRORS"; then
    echo "could not list $target tests; the registry was not checked" >&2
    tail -n 20 "$LIST_ERRORS" >&2
    exit 1
  fi
}

LIST_ERRORS="$(mktemp)"
# shellcheck disable=SC2064
trap "rm -f '$LIST_ERRORS'" EXIT

KNOWN="$(cd "$ROOT" && list_known "unit" --bin afhttp --lib && list_known "integration" --tests)"

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
