# Development / CI container

`Dockerfile` at the repository root builds a **toolchain-only dev/CI** image:
Rust (rustup, exact version, with the `wasm32-unknown-unknown` target),
Node.js (exact version), Python 3 (standard library only, for helper scripts:
the conformance harness and the branding/license checks — the app itself is
pure Rust), the Tauri/WebKitGTK build dependencies, Poppler (`pdftoppm`,
`pdfinfo`), librsvg (`rsvg-convert`), ImageMagick (`magick`), bubblewrap,
xdg-utils and Playwright's Chromium. It contains no project sources; the checkout is
bind-mounted at `/work`. `.dockerignore` excludes everything, so no private
file can end up in an image layer.

Pinned versions are build arguments (`DEBIAN_TAG`, `RUST_VERSION`,
`NODE_VERSION`, `PLAYWRIGHT_VERSION`). `DEBIAN_TAG` may carry a digest
(`trixie-20261020-slim@sha256:<digest>`) to pin the base image by content;
see the comment in the `Dockerfile` for how to resolve it. `PLAYWRIGHT_VERSION` must equal the
`@playwright/test` version in `desktop/package-lock.json`.

## Build

```sh
docker build -t notes-dev .
# match your uid/gid so files written into the bind mount stay yours
docker build -t notes-dev --build-arg USER_UID="$(id -u)" --build-arg USER_GID="$(id -g)" .
```

## Run

```sh
alias notes-dev='docker run --rm -it -v "$PWD":/work -w /work \
  -e NOTES_SANDBOX=direct \
  -e CARGO_TARGET_DIR=/work/target/docker notes-dev'
```

* The container keeps Docker's default seccomp and AppArmor profiles.
  Those profiles block the user namespaces bubblewrap needs, so the default
  run sets `NOTES_SANDBOX=direct`: the PDF/ink renderers run with resource
  limits (rlimits, timeouts, parent-death signal) but without namespace,
  filesystem or network isolation, and print one stderr line saying so (see
  `crates/rs-render/src/sandbox.rs`; `direct` and `none` are accepted, unset
  means auto-detect). The container itself is the isolation boundary, and the
  tests only feed synthetic fixtures. Without the variable the engine probes
  bwrap, finds it unusable and falls back to the same mode on its own.
* `CARGO_TARGET_DIR=/work/target/docker` keeps container build products
  apart from host builds (different glibc/toolchain).

### Opt-in: exercise the bubblewrap sandbox

Only when you need to test the bwrap code path itself (sandbox arguments,
mount layout), and only with trusted inputs:

```sh
# WARNING: disables the container's seccomp filter and AppArmor profile.
# Any process in the container (build scripts, npm/cargo dependencies, the
# code under test) then runs with a much larger kernel attack surface. Do not
# use this for everyday builds or with untrusted checkouts or notes.
docker run --rm -it -v "$PWD":/work -w /work \
  --security-opt seccomp=unconfined --security-opt apparmor=unconfined \
  -e NOTES_SANDBOX=bwrap \
  -e CARGO_TARGET_DIR=/work/target/docker notes-dev
```

`NOTES_SANDBOX=bwrap` forces bubblewrap, so a sandbox failure is an error
instead of a silent fallback to `direct`.

## Prerequisite: sdocx (GPL-3.0 renderer source)

The ink renderer CLI (`tools/ink-renderer`) builds against
`references/upstream/sdocx` at commit
`41e9c1099b29a48dc7320501573d7c4858f09303`; its `build.rs` verifies a clean
git checkout of exactly that commit.

```sh
notes-dev tools/fetch_sdocx.sh
notes-dev cargo build --release --manifest-path tools/ink-renderer/Cargo.toml
```

(`tools/ink-renderer` uses its own target dir by default; with
`CARGO_TARGET_DIR` set, point the engine at the binary with the usual
`tools/ink-renderer/target/release/ink-render` path by building without the
override: `notes-dev env -u CARGO_TARGET_DIR cargo build --release --manifest-path tools/ink-renderer/Cargo.toml`.)

## Run every test suite

```sh
# Rust crates (default features: no PyO3)
notes-dev cargo test --locked --workspace

# notes-engine HTTP conformance: replay the frozen golden fixtures
# (crates/notes-engine/conformance/golden) against the engine; must be 100 %
notes-dev sh -c 'cargo build -p notes-engine &&
  python3 crates/notes-engine/conformance/run.py --replay \
    --candidate "$CARGO_TARGET_DIR/debug/notes-engine"'

# Live pen preview (separate GPL-3.0 workspace): native tests + WebAssembly
notes-dev sh -c 'cargo test --locked --manifest-path crates/notes-ink-live/Cargo.toml &&
  cargo build --locked --release --target wasm32-unknown-unknown \
    --manifest-path crates/notes-ink-live/Cargo.toml'

# Desktop UI: type check, build, Playwright (mocked API, vite preview on 4178)
notes-dev sh -c 'cd desktop && npm ci && npx tsc --noEmit && npm run build &&
  npx playwright test'

# Branding and third-party license list
notes-dev python3 tools/branding.py sync --check
notes-dev python3 tools/third_party_licenses.py --check
```

Notes:

* The replay compares against recordings made with every optional tool
  present. Full coverage needs `tools/ink-renderer/target/{release,debug}/ink-render`
  (build it as above). When a renderer's version differs from the recording
  (see `golden.json`), PNG/PDF outputs are compared loosely (status, headers,
  image geometry) and reported as `PASS~`; everything else stays exact. If
  `magick` is missing, the recorded `no-magick` variant is used.
* `desktop/playwright.config.ts` uses `PLAYWRIGHT_CHROMIUM_PATH`, else
  `/usr/bin/chromium` when present, else Playwright's own Chromium; the image
  links Playwright's Chromium to `/usr/bin/chromium`.
* The image runs as the unprivileged `dev` user on purpose: several tests
  check file modes and permission errors that root would bypass.
* Nothing in these commands talks to Samsung: the conformance and UI tests
  run offline against synthetic data.
