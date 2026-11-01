#!/usr/bin/env bash
# The SDK floor only fetches: it talks to a host over HTTP, reads its events
# with plain serde, and leaves redaction to the host and CLI. The crates below
# belong to the host and CLI, so none of them may reach the SDK floor, directly
# or through another dependency (agent-first-ui brings agent-first-data with
# it). Runs in the local gate via tests/test.sh and in CI's static job.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.."

HOST_ONLY=(agent-first-data agent-first-ui chromiumoxide axum)

tree="$(cargo tree -e normal --no-default-features --features sdk --prefix none)"
status=0
for crate in "${HOST_ONLY[@]}"; do
  if grep -q "^$crate v" <<<"$tree"; then
    echo "the sdk feature links $crate, which belongs to the host and CLI:" >&2
    cargo tree -e normal --no-default-features --features sdk -i "$crate" >&2
    status=1
  fi
done
exit "$status"
