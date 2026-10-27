# syntax=docker/dockerfile:1.7
#
# Development / CI toolchain image. It contains NO project sources: the
# checkout is bind-mounted at /work (see docs/DOCKER.md). Everything a full
# test run needs is here: Rust (+ wasm32-unknown-unknown), Node, Python 3, the
# Tauri (WebKitGTK) build dependencies, Poppler, librsvg, ImageMagick,
# bubblewrap, xdg-utils and Playwright's Chromium.
#
# Python 3 (standard library only) is here only for helper scripts: the
# golden conformance harness, branding and license checks. The app is pure
# Rust (notes-engine) and needs no Python. This is a dev/CI image, not a
# runtime image.
#
# Versions are pinned via build args; bump them deliberately.
#
# Base image: the dated Debian tag below is immutable in practice but not by
# content. To pin by content, resolve the digest of exactly this tag on a
# networked machine and append it to the tag, e.g.
#   docker buildx imagetools inspect debian:trixie-20261020-slim \
#     --format '{{json .Manifest.Digest}}'
#   docker build --build-arg DEBIAN_TAG=trixie-20261020-slim@sha256:<digest> -t notes-dev .
# The digest is not committed here because it could not be verified offline
# when this file was written; an unverified digest would be worse than none.
ARG DEBIAN_TAG=trixie-20261020-slim
FROM debian:${DEBIAN_TAG}

# Fail a RUN line when any command in a pipeline fails, not only the last.
SHELL ["/bin/bash", "-o", "pipefail", "-c"]

ARG RUST_VERSION=1.99.1
ARG NODE_VERSION=22.21.1
ARG PLAYWRIGHT_VERSION=1.64.1
ARG USER_UID=1000
ARG USER_GID=1000

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright \
    CARGO_BUILD_JOBS=4 \
    PATH=/opt/cargo/bin:/opt/node/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

# System packages: compilers, Tauri/WebKitGTK headers, SQLite/OpenSSL, the
# external helpers the engine runs (pdftoppm/pdfinfo, rsvg-convert, magick,
# bwrap, xdg-user-dir/xdg-open), fonts for deterministic rendering, git (the
# ink renderer's build.rs verifies the sdocx checkout with git).
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl xz-utils git build-essential pkg-config file patchelf \
        libssl-dev libsqlite3-dev zlib1g-dev \
        libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev \
        librsvg2-dev libxdo-dev \
        python3 \
        poppler-utils librsvg2-bin imagemagick bubblewrap xdg-utils xdg-user-dirs \
        fonts-dejavu-core fonts-noto-core fontconfig \
        desktop-file-utils appstream \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

# Debian names ImageMagick 7's CLI "convert"/"magick" depending on version;
# the engine calls `magick`.
RUN command -v magick >/dev/null || ln -s "$(command -v convert)" /usr/local/bin/magick

# Rust (rustup, minimal profile) at an exact version.
RUN curl -fsSL https://sh.rustup.rs -o /tmp/rustup-init.sh \
    && sh /tmp/rustup-init.sh -y --no-modify-path --profile minimal --default-toolchain "${RUST_VERSION}" \
        --target wasm32-unknown-unknown \
    && rm /tmp/rustup-init.sh \
    && rustc --version && cargo --version

# Node.js (official tarball, exact version; vite 8 needs >= 20.19 / 22.12).
RUN arch="$(dpkg --print-architecture)"; case "$arch" in amd64) n=x64;; arm64) n=arm64;; *) echo "unsupported $arch"; exit 1;; esac; \
    curl -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-${n}.tar.xz" -o /tmp/node.tar.xz \
    && curl -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/SHASUMS256.txt" -o /tmp/SHASUMS256.txt \
    && (cd /tmp && grep " node-v${NODE_VERSION}-linux-${n}.tar.xz\$" SHASUMS256.txt | sed "s#node-v.*#node.tar.xz#" | sha256sum -c -) \
    && mkdir -p /opt/node && tar -xJf /tmp/node.tar.xz -C /opt/node --strip-components=1 \
    && rm /tmp/node.tar.xz /tmp/SHASUMS256.txt && node --version

# Playwright Chromium matching desktop/package-lock.json (@playwright/test).
# The spec files fall back to Playwright's Chromium when /usr/bin/chromium is
# absent; the two .mjs integration scripts expect /usr/bin/chromium, so link it.
# `--with-deps` runs apt-get itself; the lists it fetches and apt's archive
# cache are removed in the same layer.
RUN npx -y "playwright@${PLAYWRIGHT_VERSION}" install --with-deps chromium \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/* /var/cache/apt/archives/*.deb /root/.npm \
    && ln -s "$(find "$PLAYWRIGHT_BROWSERS_PATH" -path '*chrome-linux*/chrome' -type f | head -n1)" /usr/bin/chromium \
    && /usr/bin/chromium --version

# Unprivileged user (tests check file modes; root would bypass them). Tool
# homes stay writable so cargo/npm caches work under any --user.
RUN groupadd -g "${USER_GID}" dev && useradd -m -u "${USER_UID}" -g "${USER_GID}" -s /bin/bash dev \
    && chmod -R a+rwX /opt/cargo /opt/rustup /opt/ms-playwright \
    && git config --system --add safe.directory '*'

USER dev
WORKDIR /work
CMD ["bash"]
