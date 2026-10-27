#!/usr/bin/env bash
# Build the Tauri bundles (AppImage / .deb / .rpm). They ship the
# Python-free `notes-engine` binary next to the shell executable; the shell
# is built with Cargo feature `packaged` (set by
# packaging/linux/tauri.release.conf.json), which makes that binary the
# default engine. The packages never need python3.
#
#   packaging/build-linux-bundles.sh [FORMATS [OUT_DIR]]
#   FORMATS: comma list of appimage,deb,rpm (default all three)
#   OUT_DIR: where only the finished bundles are written (default <root>/dist)
#
# Environment (release.yml contract, all optional locally):
#   VERSION             version stamped into the bundles (default: tauri.conf.json)
#   APP_ID, BINARY_NAME checked against branding.json (the source of truth)
#   CARGO_BUILD_JOBS    default 4
#   NOTES_ALLOW_SAMSUNG_ASSETS=1  local test builds only: report, do not fail,
#                       when packaging/check-package-contents.py finds
#                       non-redistributable Samsung UI assets.
#
# Build on the oldest distribution you support (release: ubuntu-22.04); see
# packaging/containers/build-ubuntu-22.04.Dockerfile for a matching image.
set -euo pipefail

bundles="${1:-appimage,deb,rpm}"
root="$(cd "$(dirname "$0")/.." && pwd)"
dist="${2:-$root/dist}"
case "$dist" in /*) ;; *) dist="$PWD/$dist";; esac
VERSION="${VERSION:-${NOTES_VERSION:-}}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"

case ",$bundles," in *[!a-z,]*) echo "bad bundle list: $bundles" >&2; exit 2;; esac

cd "$root"
python3 tools/branding.py sync --check
check_brand() {
    local want; want="$(python3 tools/branding.py get "$2")"
    [ -z "$1" ] || [ "$1" = "$want" ] || { echo "$3=$1 but branding.json says $want" >&2; exit 2; }
}
check_brand "${APP_ID:-}" appId APP_ID
check_brand "${BINARY_NAME:-}" binaryName BINARY_NAME
tools/fetch_sdocx.sh

# The Python-free engine, shipped next to the shell executable (Tauri
# externalBin wants a target-triple suffixed copy). SQLite is linked
# statically so the AppImage does not depend on the host libsqlite3.
cargo build --locked --release -p notes-engine --features bundled-sqlite
host="$(rustc -vV | sed -n 's/^host: //p')"
cp -f target/release/notes-engine "target/release/notes-engine-$host"
# GPL ink renderer CLI, shipped as a resource next to the frontend.
cargo build --locked --release --manifest-path tools/ink-renderer/Cargo.toml
# Live pen preview (GPL, WebAssembly) -> desktop/public/ink-live.wasm ->
# dist/ink-live.wasm; always rebuilt from source for a release.
if command -v rustup >/dev/null; then rustup target add wasm32-unknown-unknown; fi
tools/build_ink_wasm.sh

[ -d desktop/node_modules ] || npm --prefix desktop ci --no-audit --no-fund

configs=(--config ../packaging/linux/tauri.release.conf.json)
if [ -n "$VERSION" ]; then
    case "$VERSION" in *[!0-9A-Za-z.+-]*) echo "bad VERSION: $VERSION" >&2; exit 2;; esac
    overlay="$(mktemp)"
    trap 'rm -f "$overlay"' EXIT INT TERM
    printf '{"version": "%s"}\n' "$VERSION" > "$overlay"
    configs+=(--config "$overlay")
fi

# linuxdeploy's bundled strip cannot read .relr.dyn sections; the release
# profile strips our own binary already.
export NO_STRIP=true APPIMAGE_EXTRACT_AND_RUN=1
(
    cd desktop
    npx tauri build --bundles "$bundles" "${configs[@]}" -- --locked
)

mkdir -p "$dist"
out="desktop/src-tauri/target/release/bundle"
found=0
for kind in appimage deb rpm; do
    case ",$bundles," in *",$kind,"*) ;; *) continue;; esac
    for file in "$out/$kind"/*.AppImage "$out/$kind"/*.deb "$out/$kind"/*.rpm; do
        [ -f "$file" ] || continue
        cp -f "$file" "$dist/"
        found=$((found + 1))
        python3 packaging/check-package-contents.py "$dist/$(basename "$file")" || [ $? -eq 3 ]
    done
done
[ "$found" -gt 0 ] || { echo "no bundles produced" >&2; exit 1; }
ls -l "$dist"
