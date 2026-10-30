#!/usr/bin/env bash
# Build the Arch package (packaging/arch/PKGBUILD) from THIS source tree
# instead of the GitHub tag tarball: a tarball of the tree is placed under
# the PKGBUILD's source file name and makepkg runs with --skipchecksums
# for it. Needs makepkg and the PKGBUILD's makedepends; installs nothing.
#
#   packaging/build-arch.sh [OUT_DIR]        # default <root>/dist
#
# Clean-room build in a throwaway Arch container (no host packages touched):
#   docker run --rm -v "$PWD:/src:ro" -v "$PWD/dist:/out" archlinux:latest sh -c '
#     pacman -Syu --noconfirm --needed base-devel git rust rust-wasm nodejs npm python \
#       webkit2gtk-4.1 gtk3 libsoup3 openssl sqlite poppler bubblewrap xdg-utils hicolor-icon-theme &&
#     useradd -m -d /build b && cp -a /src /build/src && chown -R b /build/src /out &&
#     su b -c "/build/src/packaging/build-arch.sh /out"'
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$root/dist}"
case "$out" in /*) ;; *) out="$PWD/$out";; esac
# VERSION (release tag without v, e.g. 0.0.2-alpha) overrides the PKGBUILD's
# pkgver; pacman versions cannot contain '-', and "0.0.2alpha" sorts before
# the final 0.0.2 (vercmp), like rpm's 0.alpha release.
if [ -n "${VERSION:-}" ]; then
    case "$VERSION" in *[!0-9A-Za-z.+-]*) echo "bad VERSION: $VERSION" >&2; exit 2;; esac
    pkgver="${VERSION//-/}"
else
    pkgver="$(sed -n 's/^pkgver=//p' "$root/packaging/arch/PKGBUILD")"
fi
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT INT TERM

python3 "$root/tools/branding.py" sync --check
mkdir -p "$work/build" "$out"
sed "s/^pkgver=.*/pkgver=$pkgver/" "$root/packaging/arch/PKGBUILD" > "$work/build/PKGBUILD"
tar -C "$root/.." --transform "s|^$(basename "$root")|notes-$pkgver|" \
    --exclude-vcs --exclude='*/target' --exclude='*/target-*' --exclude='*/node_modules' \
    --exclude="$(basename "$root")/artifacts" --exclude="$(basename "$root")/references" --exclude="$(basename "$root")/reference" \
    --exclude="$(basename "$root")/tools/vendor" --exclude="$(basename "$root")/dist" \
    --exclude="$(basename "$root")/desktop/dist" --exclude="$(basename "$root")/desktop/dist-*" \
    --exclude='__pycache__' --exclude='*.sqlite3*' \
    -czf "$work/build/notes-$pkgver.tar.gz" "$(basename "$root")"
(
    cd "$work/build"
    export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" PKGDEST="$out"
    makepkg -f --noconfirm --skipchecksums
)
for pkg in "$out"/notes-"$pkgver"-*.pkg.tar.*; do
    python3 "$root/packaging/check-package-contents.py" "$pkg" || [ $? -eq 3 ]
done
