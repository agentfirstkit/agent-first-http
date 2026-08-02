#!/usr/bin/env bash
# Single source of truth for afhttp's optional browser backends.
#
# Used by BOTH the runtime image (container/docker/Dockerfile) and the test
# image (tests/Dockerfile.test) so pinned versions can never drift between them.
# Each backend is a subcommand that installs one binary onto PATH (afhttp's
# host resolves backends by name via resolve_named_bin / PATH candidates).
#
# Several upstreams publish x86_64-only Linux builds; those subcommands self-skip
# on other architectures (the matching afhttp backend then simply stays
# unavailable, and its tests self-skip).
#
#   install-backends.sh chrome-headless-shell | lightpanda | fingerprint-chromium
#                       | camoufox | chrome | brave | kasmvnc
set -euo pipefail

# Pinned versions for direct-download backends — bump these (and re-verify both
# images build) together. Apt-backed Chrome and Brave track their vendors'
# stable repositories.
CHROME_FOR_TESTING_VERSION="151.0.7922.71"
FINGERPRINT_CHROMIUM_VERSION="148.0.7778.215"
CAMOUFOX_RELEASE="v152.0.4-beta.28"
CAMOUFOX_X86_BUILD="152.0.4-beta.28"
CAMOUFOX_ARM_BUILD="152.0.4-beta.28"
GO_VERSION="1.26.5"
KASMVNC_VERSION="1.5.0"

arch="$(uname -m)"

download() {
    local output="$1"
    local url="$2"
    echo "downloading ${url}"
    rm -f "$output"
    curl -fL --retry 5 --retry-all-errors --retry-delay 5 --connect-timeout 20 \
        --speed-time 60 --speed-limit 1024 --progress-bar -o "$output" "$url"
}

install_chrome_headless_shell() {
    # Chrome-for-Testing publishes linux64 only (no arm64 as of this writing).
    case "$arch" in
        x86_64) cft_arch="linux64" ;;
        *) echo "skipping chrome-headless-shell on unsupported arch $arch"; return 0 ;;
    esac
    download /tmp/chs.zip \
        "https://storage.googleapis.com/chrome-for-testing-public/${CHROME_FOR_TESTING_VERSION}/${cft_arch}/chrome-headless-shell-${cft_arch}.zip"
    unzip -q /tmp/chs.zip -d /opt/
    ln -sf "/opt/chrome-headless-shell-${cft_arch}/chrome-headless-shell" /usr/local/bin/chrome-headless-shell
    rm /tmp/chs.zip
}

install_lightpanda() {
    case "$arch" in
        x86_64) lp_arch="x86_64" ;;
        aarch64|arm64) lp_arch="aarch64" ;;
        *) echo "skipping lightpanda on unsupported arch $arch"; return 0 ;;
    esac
    download /usr/local/bin/lightpanda \
        "https://github.com/lightpanda-io/browser/releases/download/nightly/lightpanda-${lp_arch}-linux"
    chmod +x /usr/local/bin/lightpanda
}

install_fingerprint_chromium() {
    # Upstream publishes linux x86_64 only.
    case "$arch" in
        x86_64) ;;
        *) echo "skipping fingerprint-chromium on unsupported arch $arch"; return 0 ;;
    esac
    local url="https://github.com/adryfish/fingerprint-chromium/releases/download/${FINGERPRINT_CHROMIUM_VERSION}/ungoogled-chromium-${FINGERPRINT_CHROMIUM_VERSION}-1-x86_64_linux.tar.xz"
    download /tmp/fpc.tar.xz "$url"
    mkdir -p /opt/fingerprint-chromium
    tar -xJf /tmp/fpc.tar.xz -C /opt/fingerprint-chromium --strip-components=1
    rm /tmp/fpc.tar.xz
    local bin
    bin="$(find /opt/fingerprint-chromium -maxdepth 3 -name chrome -type f 2>/dev/null | head -1)"
    [ -z "$bin" ] && bin="$(find /opt/fingerprint-chromium -maxdepth 3 -name chromium -type f 2>/dev/null | head -1)"
    if [ -n "$bin" ]; then
        ln -sf "$bin" /usr/local/bin/fingerprint-chromium
    else
        echo "fingerprint-chromium binary not found in extracted tree"; return 1
    fi
}

install_camoufox() {
    # Camoufox (stealth Firefox fork) + foxbridge (CDP→Juggler proxy). Camoufox
    # ships amd64 + arm64 binaries; foxbridge has no release binaries, so build
    # it with a throwaway Go toolchain.
    local cf_url go_arch
    case "$arch" in
        x86_64)
            cf_url="https://github.com/daijro/camoufox/releases/download/${CAMOUFOX_RELEASE}/camoufox-${CAMOUFOX_X86_BUILD}-lin.x86_64.zip"
            go_arch="amd64" ;;
        aarch64|arm64)
            cf_url="https://github.com/daijro/camoufox/releases/download/${CAMOUFOX_RELEASE}/camoufox-${CAMOUFOX_ARM_BUILD}-lin.arm64.zip"
            go_arch="arm64" ;;
        *) echo "skipping camoufox/foxbridge on unsupported arch $arch"; return 0 ;;
    esac
    apt-get update && apt-get install -y --no-install-recommends \
        libdbus-glib-1-2 libxt6 libasound2
    rm -rf /var/lib/apt/lists/*
    download /tmp/go.tgz "https://go.dev/dl/go${GO_VERSION}.linux-${go_arch}.tar.gz"
    tar -xzf /tmp/go.tgz -C /usr/local
    rm /tmp/go.tgz
    export PATH="/usr/local/go/bin:$PATH" GOPATH=/go GOBIN=/usr/local/bin
    mkdir -p "$GOPATH"
    go install github.com/VulpineOS/foxbridge/cmd/foxbridge@latest \
        || echo "warning: foxbridge install failed; camoufox backend will be unavailable"
    download /tmp/cf.zip "$cf_url"
    mkdir -p /opt/camoufox
    unzip -q /tmp/cf.zip -d /opt/camoufox
    rm /tmp/cf.zip
    local cf_bin
    cf_bin="$(find /opt/camoufox -maxdepth 4 -name camoufox -type f -executable 2>/dev/null | head -1)"
    [ -z "$cf_bin" ] && cf_bin="$(find /opt/camoufox -maxdepth 4 -name firefox -type f -executable 2>/dev/null | head -1)"
    if [ -n "$cf_bin" ]; then
        ln -sf "$cf_bin" /usr/local/bin/camoufox
    else
        echo "camoufox binary not found in extracted tree"
    fi
    rm -rf /usr/local/go "$GOPATH"
}

install_chrome() {
    # Google Chrome, from Google's own apt repository. Chrome is proprietary and
    # cannot be redistributed, so this only ever runs in an image the user builds
    # locally — it is opt-in via `--with chrome` and never part of a published
    # artifact. Google shipped native arm64 Linux builds in July 2026, so both
    # architectures now resolve; before that only amd64 existed.
    local deb_arch
    case "$arch" in
        x86_64) deb_arch="amd64" ;;
        aarch64|arm64) deb_arch="arm64" ;;
        *) echo "skipping Chrome on unsupported arch $arch"; return 0 ;;
    esac
    # Google publishes an ASCII-armored key; apt reads it as-is from a .asc file,
    # so this needs no gnupg in the image (Brave ships a pre-dearmored .gpg).
    curl -4 --retry 5 --retry-all-errors -fsSLo /usr/share/keyrings/google-chrome.asc \
        https://dl.google.com/linux/linux_signing_key.pub
    echo "deb [signed-by=/usr/share/keyrings/google-chrome.asc arch=${deb_arch}] https://dl.google.com/linux/chrome/deb/ stable main" \
        > /etc/apt/sources.list.d/google-chrome.list
    apt-get update
    apt-get install -y --no-install-recommends google-chrome-stable
    rm -rf /var/lib/apt/lists/*
}

install_brave() {
    # Brave publishes Debian packages for both amd64 and arm64. It stays in the
    # Chromium/CDP family, so persistent profiles and iframe targets keep the
    # same semantics as the stock chromium backend while adding Brave Shields.
    local deb_arch
    case "$arch" in
        x86_64) deb_arch="amd64" ;;
        aarch64|arm64) deb_arch="arm64" ;;
        *) echo "skipping Brave on unsupported arch $arch"; return 0 ;;
    esac
    curl -4 --retry 5 --retry-all-errors -fsSLo /usr/share/keyrings/brave-browser-archive-keyring.gpg \
        https://brave-browser-apt-release.s3.brave.com/brave-browser-archive-keyring.gpg
    echo "deb [signed-by=/usr/share/keyrings/brave-browser-archive-keyring.gpg arch=${deb_arch}] https://brave-browser-apt-release.s3.brave.com/ stable main" \
        > /etc/apt/sources.list.d/brave-browser-release.list
    apt-get update
    apt-get install -y --no-install-recommends brave-browser
    rm -rf /var/lib/apt/lists/*
}

install_kasmvnc() {
    # External GPL process; afhttp only locates Xvnc on PATH and never links it.
    # matchbox is the minimal WM the headful display path needs.
    local deb_arch
    case "$arch" in
        x86_64) deb_arch="amd64" ;;
        aarch64|arm64) deb_arch="arm64" ;;
        *) echo "skipping KasmVNC on unsupported arch $arch"; return 0 ;;
    esac
    apt-get update
    apt-get install -y --no-install-recommends matchbox-window-manager
    download /tmp/kasmvnc.deb \
        "https://github.com/kasmtech/KasmVNC/releases/download/v${KASMVNC_VERSION}/kasmvncserver_bookworm_${KASMVNC_VERSION}_${deb_arch}.deb"
    apt-get install -y --no-install-recommends /tmp/kasmvnc.deb
    rm -rf /var/lib/apt/lists/* /tmp/kasmvnc.deb
}

case "${1:-}" in
    chrome-headless-shell) install_chrome_headless_shell ;;
    lightpanda)            install_lightpanda ;;
    fingerprint-chromium)  install_fingerprint_chromium ;;
    camoufox)              install_camoufox ;;
    chrome)                install_chrome ;;
    brave)                 install_brave ;;
    kasmvnc)               install_kasmvnc ;;
    *) echo "usage: $0 {chrome-headless-shell|lightpanda|fingerprint-chromium|camoufox|chrome|brave|kasmvnc}" >&2; exit 2 ;;
esac
