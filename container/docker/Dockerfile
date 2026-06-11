# Runtime image for the afhttp *host* (the browser side).
#
# The driver (`afhttp fetch`/`cdp`/`upload`/…) is a thin client and runs directly
# wherever the agent is — it does NOT need this image. The host runs a real
# browser with Chromium's own sandbox disabled (the image sets AFHTTP_NO_SANDBOX,
# so the container itself is the isolation boundary; afhttp keeps the sandbox ON
# when run natively), holds browser profile state, and exposes a CDP endpoint;
# that is what belongs in a container. See docs/deployment.md.
#
# The afhttp binary comes from ONE of two source stages, picked by the
# AFHTTP_BIN_FROM build-arg:
#   builder    — compile from a source checkout (default; dev / `--from-source`).
#                Build context is the spore root:
#                  docker build -t afhttp-host -f container/docker/Dockerfile .
#   downloader — download the matching prebuilt release; what `afhttp container
#                install` uses. Needs --build-arg AFHTTP_VERSION + AFHTTP_TARGET.
# BuildKit only builds the selected stage, so the download path pulls no Rust
# toolchain and needs no source tree, while the from-source path makes no network
# call for a release. Optional backends are opt-in (chromium is always present):
#   docker build --build-arg WITH_BRAVE=1 --build-arg WITH_KASMVNC=1 ...
#
# This is NOT the test image — tests/Dockerfile.test (rust toolchain + all backends
# + coverage tooling, source mounted as a volume) is for build/test/clippy/coverage.

# Selects which source stage below provides the binary (see FROM ${AFHTTP_BIN_FROM}).
# Global (before the first FROM) so it is usable on a FROM line.
ARG AFHTTP_BIN_FROM=builder

# ── Source stage A: compile from a checkout (default) ──
FROM rust:bookworm AS builder
WORKDIR /src
# Only the crate inputs — NOT .cargo/ (its dev path override to
# ../agent-first-data/rust is absent here, so agent-first-data resolves from
# crates.io per Cargo.lock).
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY assets/ assets/
COPY tests/ tests/
COPY examples/ examples/
# Embedded at compile time via include_str!: the skill (skill.rs) and the
# container build recipe + shared scripts (container.rs).
COPY skills/ skills/
COPY container/ container/
RUN cargo build --release --bin afhttp \
    && cp target/release/afhttp /usr/local/bin/afhttp

# ── Source stage B: download the matching prebuilt release ──
FROM debian:bookworm-slim AS downloader
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
# Hard-pinned by the driver: its own version + the linux triple for the image
# arch. A 404 here means no release asset exists for this version/arch — the
# driver surfaces that and points at the from-source path.
ARG AFHTTP_VERSION
ARG AFHTTP_TARGET
RUN set -eu; \
    base="https://github.com/agentfirstkit/agent-first-http/releases/download/v${AFHTTP_VERSION}"; \
    archive="afhttp-v${AFHTTP_VERSION}-${AFHTTP_TARGET}.tar.gz"; \
    cd /tmp; \
    curl -fsSL -o "$archive" "${base}/${archive}"; \
    curl -fsSL -o "${archive}.sha256" "${base}/${archive}.sha256"; \
    sha256sum -c "${archive}.sha256"; \
    tar -xzf "$archive" -C /usr/local/bin afhttp; \
    chmod +x /usr/local/bin/afhttp

# ── Pick the source stage that provides /usr/local/bin/afhttp ──
FROM ${AFHTTP_BIN_FROM} AS afhttp_bin

# ── Runtime image (single source of truth for the host) ──
FROM debian:bookworm-slim

ENV DEBIAN_FRONTEND=noninteractive

# chromium (the always-on backend) + the libraries it needs to start, plus the
# small toolset install-backends.sh uses for the optional backends, plus tini to
# reap the browser's child processes (afhttp host is PID 1).
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        chromium \
        curl \
        fonts-liberation \
        fonts-noto-cjk \
        libatk-bridge2.0-0 \
        libatk1.0-0 \
        libdrm2 \
        libgbm1 \
        libnss3 \
        libxcomposite1 \
        libxdamage1 \
        libxfixes3 \
        libxkbcommon0 \
        libxrandr2 \
        procps \
        tini \
        unzip \
        xdg-utils \
        xz-utils \
    && rm -rf /var/lib/apt/lists/*

# Optional backends — each opt-in, arch-guarded inside the shared installer that
# tests/Dockerfile.test also uses (single source of truth for pinned versions).
COPY container/docker/install-backends.sh /usr/local/bin/install-backends.sh
RUN chmod +x /usr/local/bin/install-backends.sh

ARG WITH_CHROME_HEADLESS_SHELL=0
ARG WITH_LIGHTPANDA=0
ARG WITH_FINGERPRINT_CHROMIUM=0
ARG WITH_CAMOUFOX=0
ARG WITH_BRAVE=0
ARG WITH_KASMVNC=0

RUN if [ "$WITH_CHROME_HEADLESS_SHELL" = "1" ]; then install-backends.sh chrome-headless-shell; fi
RUN if [ "$WITH_LIGHTPANDA" = "1" ]; then install-backends.sh lightpanda; fi
RUN if [ "$WITH_FINGERPRINT_CHROMIUM" = "1" ]; then install-backends.sh fingerprint-chromium; fi
RUN if [ "$WITH_CAMOUFOX" = "1" ]; then install-backends.sh camoufox; fi
RUN if [ "$WITH_BRAVE" = "1" ]; then install-backends.sh brave; fi
RUN if [ "$WITH_KASMVNC" = "1" ]; then install-backends.sh kasmvnc; fi

COPY --from=afhttp_bin /usr/local/bin/afhttp /usr/local/bin/afhttp
COPY container/docker/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# Non-root: AFHTTP_NO_SANDBOX makes chromium run with its sandbox off (the
# container is the boundary), so it does not need root. Profiles live under
# $XDG_DATA_HOME (a volume). KasmVNC env is harmless when Xvnc isn't built in —
# the host only reads it for `--takeover-provider kasmvnc`.
RUN useradd --create-home --uid 10001 afhttp \
    && mkdir -p /data \
    && chown -R afhttp:afhttp /data
ENV XDG_DATA_HOME=/data \
    AFHTTP_NO_SANDBOX=1 \
    AFHTTP_KASMVNC_BIN=/usr/bin/Xvnc \
    AFHTTP_KASMVNC_WEB_ROOT=/usr/share/kasmvnc/www

USER afhttp
VOLUME ["/data"]
EXPOSE 9222

ENTRYPOINT ["/usr/bin/tini", "--", "/entrypoint.sh"]
