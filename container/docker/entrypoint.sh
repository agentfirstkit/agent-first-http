#!/bin/sh
# afhttp host launcher. Token-by-default: the CDP endpoint is full control of the
# browser and its profile, so the container never serves it unauthenticated. If
# AFHTTP_TOKEN is unset, a token is generated once and persisted to the data
# volume so it survives restarts.
set -eu

XDG_DATA_HOME="${XDG_DATA_HOME:-/data}"
AFHTTP_PROFILE="${AFHTTP_PROFILE:-work}"
AFHTTP_PORT="${AFHTTP_PORT:-9222}"
STATE_DIR="${XDG_DATA_HOME}/afhttp"
TOKEN_FILE="${STATE_DIR}/host-token"

mkdir -p "$STATE_DIR"
chmod 700 "$STATE_DIR" 2>/dev/null || true

# Resolve the bearer token: explicit env > persisted file > freshly generated.
if [ -n "${AFHTTP_TOKEN:-}" ]; then
    printf '%s' "$AFHTTP_TOKEN" > "$TOKEN_FILE"
elif [ -f "$TOKEN_FILE" ]; then
    AFHTTP_TOKEN="$(cat "$TOKEN_FILE")"
else
    AFHTTP_TOKEN="$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')"
    printf '%s' "$AFHTTP_TOKEN" > "$TOKEN_FILE"
fi
chmod 600 "$TOKEN_FILE" 2>/dev/null || true

echo "========================================="
echo "  afhttp host"
echo "  listen:   tcp:0.0.0.0:${AFHTTP_PORT}  (reach it over your private network/mesh as wss://)"
echo "  profile:  ${AFHTTP_PROFILE}"
echo "  token:    stored at ${TOKEN_FILE}"
echo ""
echo "  A driver (run anywhere) connects with:"
echo "    afhttp fetch https://example.com \\"
echo "      --endpoint-url ws://<host>:${AFHTTP_PORT} --token-secret \"\$(cat ${TOKEN_FILE})\""
echo "========================================="

# Extra flags (e.g. --browser camoufox, --takeover kasmvnc) pass through as "$@".
exec afhttp host \
    --listen "tcp:0.0.0.0:${AFHTTP_PORT}" \
    --token-secret "$AFHTTP_TOKEN" \
    --profile "$AFHTTP_PROFILE" \
    "$@"
