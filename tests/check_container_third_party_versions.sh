#!/usr/bin/env bash
# Fails when Docker-installed third-party pins drift behind upstream latest.
#
# Drift is a hard failure. An unreachable upstream is not: it is evidence of
# nothing, least of all that a pin is stale. This script is step 1 of the
# `static` gate, so a single connection reset used to redden an otherwise-clean
# run and abort a release that had nothing wrong with it. Every fetch now
# retries with backoff; only once the retries are exhausted does the check warn
# loudly and skip that one comparison.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SPORE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
INSTALLER="$SPORE_DIR/container/docker/install-backends.sh"

need() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "missing required command: $1" >&2
        exit 2
    }
}

pin() {
    sed -n "s/^$1=\"\\(.*\\)\"$/\\1/p" "$INSTALLER"
}

require_pin() {
    local name="$1" value
    value="$(pin "$name")"
    if [ -z "$value" ]; then
        echo "missing pin $name in $INSTALLER" >&2
        exit 2
    fi
    printf '%s\n' "$value"
}

expect_eq() {
    local label="$1" current="$2" latest="$3"
    if [ "$current" != "$latest" ]; then
        echo "outdated: $label current=$current latest=$latest" >&2
        return 1
    fi
    echo "ok: $label $current"
}

# Same retry budget as the crates.io query in scripts/release/lib.sh: every
# error class is retryable because none of them distinguish a stale pin.
curl_retry=(--retry 5 --retry-all-errors --retry-delay 2 --retry-max-time 60)

# Echoes the response body, or warns and returns 1 when the endpoint stays
# unreachable. Callers must set unreachable=1 themselves — this runs inside a
# command substitution, so assignments here would be lost with the subshell.
fetch_json() {
    local label="$1" url="$2" body
    if ! body="$(curl -fsSL "${curl_retry[@]}" "$url")" || [ -z "$body" ]; then
        echo "unreachable: $label <$url> did not answer after retries" >&2
        return 1
    fi
    printf '%s' "$body"
}

# A 200 carrying no version — rate-limit JSON, a reshaped API, a truncated body
# — is not drift evidence either. Comparing the pin against "null" would report
# a drift that upstream never claimed.
usable() {
    local label="$1" value="$2"
    if [ -z "$value" ] || [ "$value" = "null" ]; then
        echo "unreadable: $label answered without a usable version" >&2
        return 1
    fi
}

need curl
need jq

fail=0
unreachable=0

if fingerprint_json="$(fetch_json "fingerprint-chromium" "https://api.github.com/repos/adryfish/fingerprint-chromium/releases/latest")"; then
    latest_fingerprint="$(printf '%s' "$fingerprint_json" | jq -r '.tag_name')" || latest_fingerprint=""
    if usable "fingerprint-chromium" "$latest_fingerprint"; then
        fingerprint_asset="ungoogled-chromium-${latest_fingerprint}-1-x86_64_linux.tar.xz"
        if ! printf '%s' "$fingerprint_json" | jq -e --arg name "$fingerprint_asset" 'any(.assets[].name; . == $name)' >/dev/null; then
            echo "missing expected fingerprint-chromium linux asset: $fingerprint_asset" >&2
            fail=1
        fi
        expect_eq "fingerprint-chromium" "$(require_pin FINGERPRINT_CHROMIUM_VERSION)" "$latest_fingerprint" || fail=1
    else
        unreachable=1
    fi
else
    unreachable=1
fi

if camoufox_json="$(fetch_json "camoufox" "https://api.github.com/repos/daijro/camoufox/releases/latest")"; then
    latest_camoufox_release="$(printf '%s' "$camoufox_json" | jq -r '.tag_name')" || latest_camoufox_release=""
    latest_camoufox_x86="$(printf '%s' "$camoufox_json" | jq -r '.assets[].name | select(test("^camoufox-.*-lin\\.x86_64\\.zip$")) | sub("^camoufox-"; "") | sub("-lin\\.x86_64\\.zip$"; "")' | head -1)" || latest_camoufox_x86=""
    latest_camoufox_arm="$(printf '%s' "$camoufox_json" | jq -r '.assets[].name | select(test("^camoufox-.*-lin\\.arm64\\.zip$")) | sub("^camoufox-"; "") | sub("-lin\\.arm64\\.zip$"; "")' | head -1)" || latest_camoufox_arm=""
    if usable "camoufox release" "$latest_camoufox_release" \
        && usable "camoufox linux x86_64 build" "$latest_camoufox_x86" \
        && usable "camoufox linux arm64 build" "$latest_camoufox_arm"; then
        expect_eq "camoufox release" "$(require_pin CAMOUFOX_RELEASE)" "$latest_camoufox_release" || fail=1
        expect_eq "camoufox linux x86_64 build" "$(require_pin CAMOUFOX_X86_BUILD)" "$latest_camoufox_x86" || fail=1
        expect_eq "camoufox linux arm64 build" "$(require_pin CAMOUFOX_ARM_BUILD)" "$latest_camoufox_arm" || fail=1
    else
        unreachable=1
    fi
else
    unreachable=1
fi

if go_json="$(fetch_json "go toolchain" "https://go.dev/dl/?mode=json")"; then
    latest_go="$(printf '%s' "$go_json" | jq -r '.[0].version | sub("^go"; "")')" || latest_go=""
    if usable "go toolchain" "$latest_go"; then
        expect_eq "go toolchain" "$(require_pin GO_VERSION)" "$latest_go" || fail=1
    else
        unreachable=1
    fi
else
    unreachable=1
fi

if kasm_json="$(fetch_json "kasmvnc" "https://api.github.com/repos/kasmtech/KasmVNC/releases/latest")"; then
    latest_kasm="$(printf '%s' "$kasm_json" | jq -r '.tag_name | sub("^v"; "")')" || latest_kasm=""
    if usable "kasmvnc" "$latest_kasm"; then
        expect_eq "kasmvnc" "$(require_pin KASMVNC_VERSION)" "$latest_kasm" || fail=1
        for deb_arch in amd64 arm64; do
            kasm_asset="kasmvncserver_debian_trixie_${latest_kasm}_${deb_arch}.deb"
            if ! printf '%s' "$kasm_json" | jq -e --arg name "$kasm_asset" 'any(.assets[].name; . == $name)' >/dev/null; then
                echo "missing expected KasmVNC trixie asset: $kasm_asset" >&2
                fail=1
            fi
        done
    else
        unreachable=1
    fi
else
    unreachable=1
fi

if [ "$fail" -ne 0 ]; then
    echo "Update container/docker/install-backends.sh and tests/Dockerfile.test before release." >&2
    exit 1
fi

if [ "$unreachable" -ne 0 ]; then
    echo "WARNING: some upstreams stayed unreachable after retries; those pins were NOT checked." >&2
    echo "WARNING: this run does not certify them current. Re-run on a working network before release." >&2
fi
