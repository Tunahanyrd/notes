#!/bin/sh
# Build the live pen preview core (crates/notes-ink-live, GPL-3.0, pinned
# sdocx) as WebAssembly and place it where Vite copies it into the desktop
# build: desktop/public/ink-live.wasm -> dist/ink-live.wasm. Run before
# `npm run build`. Without the file the UI silently keeps its simple preview.
#
#   tools/build_ink_wasm.sh              # release build into desktop/public
#   INK_WASM_OUT=/path/x.wasm tools/build_ink_wasm.sh
#
# Needs the wasm32-unknown-unknown standard library. Distribution Rust
# packages often lack it; use rustup (`rustup target add
# wasm32-unknown-unknown`), or point RUSTUP_HOME/CARGO_HOME at an isolated
# rustup installation that has it. The build is offline-capable once the
# crates in crates/notes-ink-live/Cargo.lock are fetched (`--locked`).
set -eu

root="$(cd "$(dirname "$0")/.." && pwd)"
crate="$root/crates/notes-ink-live"
out="${INK_WASM_OUT:-$root/desktop/public/ink-live.wasm}"
target=wasm32-unknown-unknown

if [ ! -d "$root/references/upstream/sdocx/.git" ]; then
    echo "error: pinned sdocx source missing; run tools/fetch_sdocx.sh" >&2
    exit 1
fi
libdir="$(rustc --print target-libdir --target "$target" 2>/dev/null || true)"
if [ -z "$libdir" ] || ! ls "$libdir"/libstd-*.rlib >/dev/null 2>&1; then
    echo "error: Rust standard library for $target is not installed" >&2
    echo "       (rustup target add $target, or set RUSTUP_HOME/CARGO_HOME to a rustup that has it)" >&2
    exit 1
fi

# Reproducible paths in the module: no absolute build directories.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$root=. --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=cargo"
cargo build --release --locked --target "$target" --manifest-path "$crate/Cargo.toml" \
    --target-dir "$crate/target"
wasm="$crate/target/$target/release/notes_ink_live.wasm"
mkdir -p "$(dirname "$out")"
cp "$wasm" "$out.tmp"
mv "$out.tmp" "$out"
size="$(wc -c < "$out" | tr -d ' ')"
echo "ink-live.wasm: $out ($size bytes)"
