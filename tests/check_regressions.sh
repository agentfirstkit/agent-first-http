#!/bin/bash
set -euo pipefail

ROOTPATH="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOTPATH/tests/regressions.txt"

if [[ ! -f "$MANIFEST" ]]; then
  echo "regression manifest missing: $MANIFEST" >&2
  exit 1
fi

LIST_OUTPUT="$(cd "$ROOTPATH" && cargo test --bin afhttp -- --list)"
MISSING=0

while IFS= read -r name; do
  [[ -z "$name" || "$name" =~ ^# ]] && continue
  if ! grep -Fq "$name" <<<"$LIST_OUTPUT"; then
    echo "missing regression test: $name" >&2
    MISSING=1
  fi
done < "$MANIFEST"

if [[ $MISSING -ne 0 ]]; then
  exit 1
fi

echo "Regression manifest check passed."
