# Build environment for the AppImage / .deb / .rpm bundles, matching the
# release workflow runner (ubuntu-22.04: the oldest supported glibc with
# webkit2gtk-4.1, so the AppImage and .deb run on that release or newer).
# Contains no project sources; the checkout is bind-mounted at /work.
#
#   docker build -t notes-build:ubuntu-22.04 -f packaging/containers/build-ubuntu-22.04.Dockerfile .
#   docker run --rm -u "$(id -u):$(id -g)" -v "$PWD:/work" -w /work \
#       notes-build:ubuntu-22.04 packaging/build-linux-bundles.sh appimage,deb,rpm
# Pinned by digest (ubuntu:22.04 as of 2026-10-27); bump deliberately.
FROM ubuntu:22.04@sha256:e0f80c829509ef4df9e76bd8944eb7b8ec25cbb379b112a7f9448b2c77a77c9b

ARG RUST_VERSION=stable
ARG NODE_VERSION=22.21.1

ENV DEBIAN_FRONTEND=noninteractive LANG=C.UTF-8 \
    RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo CARGO_BUILD_JOBS=4 \
    PATH=/opt/cargo/bin:/opt/node/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl xz-utils git build-essential pkg-config file patchelf \
        libssl-dev libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev \
        libjavascriptcoregtk-4.1-dev librsvg2-dev libfuse2 squashfs-tools \
        xdg-utils python3 desktop-file-utils \
    && apt-get clean && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL https://sh.rustup.rs -o /tmp/rustup-init.sh \
    && sh /tmp/rustup-init.sh -y --no-modify-path --profile minimal --default-toolchain "${RUST_VERSION}" \
    && rm /tmp/rustup-init.sh && rustup target add wasm32-unknown-unknown && rustc --version

RUN curl -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-x64.tar.xz" -o /tmp/node.tar.xz \
    && curl -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/SHASUMS256.txt" -o /tmp/SHASUMS256.txt \
    && (cd /tmp && grep " node-v${NODE_VERSION}-linux-x64.tar.xz\$" SHASUMS256.txt | sed "s#node-v.*#node.tar.xz#" | sha256sum -c -) \
    && mkdir -p /opt/node && tar -xJf /tmp/node.tar.xz -C /opt/node --strip-components=1 \
    && rm /tmp/node.tar.xz /tmp/SHASUMS256.txt && node --version

# Any --user can use the toolchain and its caches.
RUN chmod -R a+rwX /opt/cargo /opt/rustup && git config --system --add safe.directory '*'
WORKDIR /work
