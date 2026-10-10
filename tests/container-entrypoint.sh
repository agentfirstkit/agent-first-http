#!/usr/bin/env bash
set -euo pipefail
# Require the backends the image installs for this architecture.
AFHTTP_TEST_REQUIRE_BACKENDS="${AFHTTP_TEST_REQUIRE_BACKENDS:-chromium},lightpanda"
if [ "$(uname -m)" = "x86_64" ]; then
  AFHTTP_TEST_REQUIRE_BACKENDS+=",fingerprint-chromium"
fi
if [ "${AFHTTP_TEST_INSTALL_CAMOUFOX:-0}" = "1" ]; then
  AFHTTP_TEST_REQUIRE_BACKENDS+=",camoufox"
fi
export AFHTTP_TEST_REQUIRE_BACKENDS
exec "$@"
