#!/usr/bin/env bash
# `cargo check` the engine workspace (and optionally the desktop shell) for
# aarch64-apple-darwin from a Linux machine. Catches Linux-only code (libc
# symbols such as prctl/SOCK_CLOEXEC, missing cfg gates) without a Mac.
# Nothing is linked and no macOS SDK is needed: C build scripts get a
# wrapper compiler that preprocesses with clang and emits empty objects.
#
#   tools/macos/cross_check.sh [--desktop] [--no-tests] [extra cargo args...]
#   CROSS_TARGET=x86_64-apple-darwin tools/macos/cross_check.sh --desktop   # Intel
#
# Needs: a rustup toolchain with `rustup target add aarch64-apple-darwin`
# (override with CARGO=/path/to/cargo RUSTC=/path/to/rustc) and clang. No
# OpenSSL for the Apple target is needed (stub version headers, see below).
# This is a compile check only; real behaviour is tested on macOS
# (.github/workflows/macos.yml, docs/MACOS_PORT.md).
set -euo pipefail
target=${CROSS_TARGET:-aarch64-apple-darwin}   # or x86_64-apple-darwin
case "$target" in aarch64-apple-darwin) clang_target=arm64-apple-macos11 ;; x86_64-apple-darwin) clang_target=x86_64-apple-macos10.15 ;; *) echo "error: unsupported CROSS_TARGET $target" >&2; exit 2 ;; esac
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
desktop=0; tests=1; extra=()
for arg in "$@"; do
  case "$arg" in
    --desktop) desktop=1 ;;
    --no-tests) tests=0 ;;
    *) extra+=("$arg") ;;
  esac
done
cargo=${CARGO:-cargo}
rustc=${RUSTC:-rustc}
sysroot=$("$rustc" --print sysroot)
if [ ! -d "$sysroot/lib/rustlib/$target" ]; then
  echo "error: the $target standard library is not installed for $rustc." >&2
  echo "       rustup target add $target   (distribution rust packages usually lack it)" >&2
  exit 2
fi
command -v clang >/dev/null || { echo "error: clang is required" >&2; exit 2; }
work=$(mktemp -d "${TMPDIR:-/tmp}/notes-xcheck.XXXXXX")
trap 'rm -rf "$work"' EXIT
cc="$work/$target-clang"
cat > "$cc" <<WRAP
#!/bin/sh
# Preprocess for the Apple target; emit an empty Mach-O object for compiles.
for a in "\$@"; do
  case "\$a" in -E) exec clang --target=$clang_target "\$@";; esac
done
out=""; prev=""
for a in "\$@"; do [ "\$prev" = "-o" ] && out="\$a"; prev="\$a"; done
[ -n "\$out" ] && exec clang --target=$clang_target -c -x c /dev/null -o "\$out"
exec clang --target=$clang_target "\$@"
WRAP
chmod +x "$cc"
# openssl-sys only preprocesses <openssl/opensslv.h> + <openssl/opensslconf.h>
# to learn the version (nothing is linked by `cargo check`). Host headers are
# not usable for an Apple target (Debian/Ubuntu keep opensslconf.h in the
# multiarch dir), so describe the OpenSSL 3.x a macOS build links (Homebrew
# openssl@3) with two stub headers. Override with OPENSSL_INCLUDE_DIR.
stub="$work/openssl-stub"
mkdir -p "$stub/include/openssl" "$stub/lib"
cat > "$stub/include/openssl/opensslv.h" <<'H'
#define OPENSSL_VERSION_MAJOR 3
#define OPENSSL_VERSION_MINOR 0
#define OPENSSL_VERSION_PATCH 0
#define OPENSSL_VERSION_NUMBER 0x30000000L
H
: > "$stub/include/openssl/opensslconf.h"
# openssl-sys also requires the library files to exist (never linked here).
for lib in libssl libcrypto; do : > "$stub/lib/$lib.dylib"; : > "$stub/lib/$lib.a"; done
openssl_inc=${OPENSSL_INCLUDE_DIR:-$stub/include}
export RUSTC="$rustc"
triple_env=$(printf %s "$target" | tr - _)
export "CC_$triple_env=$cc" "AR_$triple_env=ar"
export OPENSSL_DIR=${OPENSSL_DIR:-$stub} OPENSSL_INCLUDE_DIR="$openssl_inc" OPENSSL_LIB_DIR=${OPENSSL_LIB_DIR:-$stub/lib}
export PYO3_CROSS=1 PYO3_CROSS_PYTHON_VERSION=${PYO3_CROSS_PYTHON_VERSION:-3.12}
export PKG_CONFIG_ALLOW_CROSS=1
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$root/target/xcheck-darwin}
test_args=(); [ "$tests" = 1 ] && test_args=(--tests)
echo "==> engine workspace ($target)"
"$cargo" check --target "$target" --workspace ${test_args[@]+"${test_args[@]}"} --manifest-path "$root/Cargo.toml" ${extra[@]+"${extra[@]}"}
if [ "$desktop" = 1 ]; then
  echo "==> desktop shell ($target)"
  "$cargo" check --target "$target" --manifest-path "$root/desktop/src-tauri/Cargo.toml" ${extra[@]+"${extra[@]}"}
fi
echo "macOS cross-check passed (compile only)."
