#!/usr/bin/env bash
# Fails when Docker-installed third-party pins drift behind upstream latest.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SPORE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
INSTALLER="$SPORE_DIR/container/docker/install-backends.sh"
TEST_DOCKERFILE="$SPORE_DIR/tests/Dockerfile.test"

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

need curl
need jq

fail=0

chrome_json="$(curl -fsSL https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json)"
latest_chrome="$(printf '%s' "$chrome_json" | jq -r '.channels.Stable.version')"
expect_eq "chrome-for-testing stable" "$(require_pin CHROME_FOR_TESTING_VERSION)" "$latest_chrome" || fail=1

fingerprint_json="$(curl -fsSL https://api.github.com/repos/adryfish/fingerprint-chromium/releases/latest)"
latest_fingerprint="$(printf '%s' "$fingerprint_json" | jq -r '.tag_name')"
fingerprint_asset="ungoogled-chromium-${latest_fingerprint}-1-x86_64_linux.tar.xz"
if ! printf '%s' "$fingerprint_json" | jq -e --arg name "$fingerprint_asset" 'any(.assets[].name; . == $name)' >/dev/null; then
    echo "missing expected fingerprint-chromium linux asset: $fingerprint_asset" >&2
    fail=1
fi
expect_eq "fingerprint-chromium" "$(require_pin FINGERPRINT_CHROMIUM_VERSION)" "$latest_fingerprint" || fail=1

camoufox_json="$(curl -fsSL https://api.github.com/repos/daijro/camoufox/releases/latest)"
latest_camoufox_release="$(printf '%s' "$camoufox_json" | jq -r '.tag_name')"
latest_camoufox_x86="$(printf '%s' "$camoufox_json" | jq -r '.assets[].name | select(test("^camoufox-.*-lin\\.x86_64\\.zip$")) | sub("^camoufox-"; "") | sub("-lin\\.x86_64\\.zip$"; "")' | head -1)"
latest_camoufox_arm="$(printf '%s' "$camoufox_json" | jq -r '.assets[].name | select(test("^camoufox-.*-lin\\.arm64\\.zip$")) | sub("^camoufox-"; "") | sub("-lin\\.arm64\\.zip$"; "")' | head -1)"
expect_eq "camoufox release" "$(require_pin CAMOUFOX_RELEASE)" "$latest_camoufox_release" || fail=1
expect_eq "camoufox linux x86_64 build" "$(require_pin CAMOUFOX_X86_BUILD)" "$latest_camoufox_x86" || fail=1
expect_eq "camoufox linux arm64 build" "$(require_pin CAMOUFOX_ARM_BUILD)" "$latest_camoufox_arm" || fail=1

go_json="$(curl -fsSL 'https://go.dev/dl/?mode=json')"
latest_go="$(printf '%s' "$go_json" | jq -r '.[0].version | sub("^go"; "")')"
expect_eq "go toolchain" "$(require_pin GO_VERSION)" "$latest_go" || fail=1

kasm_json="$(curl -fsSL https://api.github.com/repos/kasmtech/KasmVNC/releases/latest)"
latest_kasm="$(printf '%s' "$kasm_json" | jq -r '.tag_name | sub("^v"; "")')"
expect_eq "kasmvnc" "$(require_pin KASMVNC_VERSION)" "$latest_kasm" || fail=1
for deb_arch in amd64 arm64; do
    kasm_asset="kasmvncserver_bookworm_${latest_kasm}_${deb_arch}.deb"
    if ! printf '%s' "$kasm_json" | jq -e --arg name "$kasm_asset" 'any(.assets[].name; . == $name)' >/dev/null; then
        echo "missing expected KasmVNC bookworm asset: $kasm_asset" >&2
        fail=1
    fi
done

latest_llvm_cov="$(curl -fsSL https://api.github.com/repos/taiki-e/cargo-llvm-cov/releases/latest | jq -r '.tag_name | sub("^v"; "")')"
current_llvm_cov="$(sed -n 's/.*cargo install cargo-llvm-cov --locked --version "\([^"]*\)".*/\1/p' "$TEST_DOCKERFILE")"
expect_eq "cargo-llvm-cov" "$current_llvm_cov" "$latest_llvm_cov" || fail=1

if [ "$fail" -ne 0 ]; then
    echo "Update container/docker/install-backends.sh and tests/Dockerfile.test before release." >&2
    exit 1
fi
