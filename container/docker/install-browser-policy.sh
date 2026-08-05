#!/usr/bin/env bash
# Managed browser policy for the container images.
#
# Used by BOTH the runtime image (container/docker/Dockerfile) and the test
# image (tests/Dockerfile.test), like install-backends.sh, so the two can never
# drift.
#
# The container sets AFHTTP_NO_SANDBOX=1, so the host launches the browser with
# `--no-sandbox --disable-setuid-sandbox` and the container itself is the
# isolation boundary (see docs/deployment.md). Chromium answers those flags with
# a permanent "You are using an unsupported command-line flag" bar across the
# top of the window. On a headless host nobody sees it; on the takeover display
# a human is looking straight at it, and it steals a strip of viewport on every
# session.
#
# Measured, so the next person does not repeat it:
#   - It is not one flag. Dropping --disable-setuid-sandbox only makes the bar
#     name --no-sandbox instead; Chromium reports just the first offending flag.
#   - It is not Brave-specific. Stock Chromium 151 shows the same bar.
#   - --test-type, the usual suppression flag, makes Brave abort on startup
#     (SIGTRAP, core dumped) and CDP never comes up. Do not reach for it.
# The policy below is the supported control, and it changes no launch flag and
# no actual sandbox posture: the flags stay, the container stays the boundary,
# only the notification is suppressed.
#
# Each browser family reads its own policy directory, so this writes all three
# rather than pretending one shared path exists.
set -euo pipefail

for dir in \
    /etc/brave/policies/managed \
    /etc/chromium/policies/managed \
    /etc/opt/chrome/policies/managed
do
    mkdir -p "$dir"
    printf '{"CommandLineFlagSecurityWarningsEnabled": false}\n' > "$dir/afhttp.json"
    chmod 0644 "$dir/afhttp.json"
done
