#!/bin/sh
# afhttp host launcher. Token-by-default: the CDP endpoint is full control of the
# browser and its profile, so the container never serves it unauthenticated. If
# AFHTTP_TOKEN_SECRET is unset, a token secret is generated once and persisted
# to the data volume so it survives restarts.
set -eu

XDG_DATA_HOME="${XDG_DATA_HOME:-/data}"
AFHTTP_PROFILE="${AFHTTP_PROFILE:--}"
AFHTTP_PORT="${AFHTTP_PORT:-9222}"
STATE_DIR="${XDG_DATA_HOME}/afhttp"
TOKEN_FILE="${STATE_DIR}/host-token"

mkdir -p "$STATE_DIR"
chmod 700 "$STATE_DIR" 2>/dev/null || true

# Resolve the bearer token secret: explicit env > persisted file > freshly generated.
if [ -n "${AFHTTP_TOKEN_SECRET:-}" ]; then
    printf '%s' "$AFHTTP_TOKEN_SECRET" > "$TOKEN_FILE"
elif [ -f "$TOKEN_FILE" ]; then
    AFHTTP_TOKEN_SECRET="$(cat "$TOKEN_FILE")"
else
    AFHTTP_TOKEN_SECRET="$(head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -d '=\n')"
    printf '%s' "$AFHTTP_TOKEN_SECRET" > "$TOKEN_FILE"
fi
chmod 600 "$TOKEN_FILE" 2>/dev/null || true

echo "========================================="
echo "  afhttp host"
echo "  listen:   tcp:0.0.0.0:${AFHTTP_PORT}  (reach it over your private network/mesh as wss://)"
echo "  profile:  ${AFHTTP_PROFILE}"
echo "  token_secret: stored at ${TOKEN_FILE}"
echo ""
echo "  A driver (run anywhere) connects with:"
echo "    afhttp fetch https://example.com \\"
echo "      --endpoint-url ws://<host>:${AFHTTP_PORT} --token-secret \"\$(cat ${TOKEN_FILE})\""
echo "========================================="

# Extra flags (e.g. --browser camoufox, --takeover-provider kasmvnc) pass through as "$@".
exec afhttp host \
    --listen "tcp:0.0.0.0:${AFHTTP_PORT}" \
    --token-secret "$AFHTTP_TOKEN_SECRET" \
    --profile "$AFHTTP_PROFILE" \
    "$@"
