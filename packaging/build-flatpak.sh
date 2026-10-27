#!/usr/bin/env bash
# Build the Flatpak bundle (single-file .flatpak) from
# packaging/flatpak/<APP_ID>.yml, offline from the generated source lists.
#
#   packaging/build-flatpak.sh [OUT_DIR]      # default <root>/dist
#
# Needs flatpak + flatpak-builder. The GNOME runtime/SDK and the rust-stable
# and node22 SDK extensions named in the manifest are installed from Flathub
# when missing (FLATPAK_INSTALL_DEPS=0 disables that; the release job's
# ghcr.io/flathub-infra/flatpak-github-actions:gnome-<N> image ships the
# runtime/SDK but not the extensions). As root the system installation is
# used, otherwise the user installation (FLATPAK_USER_DIR honoured, so a
# throwaway installation works). Environment: VERSION (bundle file name),
# APP_ID (checked against branding.json), FLATPAK_INSTALL=1 also installs
# the result into that installation.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$root/dist}"
case "$out" in /*) ;; *) out="$PWD/$out";; esac
cd "$root"

want_id="$(sed -n '1,/^app-id:/s/^app-id: *//p' packaging/flatpak/*.yml | sed -n 1p)"
if command -v python3 >/dev/null; then
    python3 tools/branding.py sync --check
    want_id="$(python3 tools/branding.py get appId)"
fi
APP_ID="${APP_ID:-$want_id}"
[ "$APP_ID" = "$want_id" ] || { echo "APP_ID=$APP_ID but branding says $want_id" >&2; exit 2; }
manifest="packaging/flatpak/$APP_ID.yml"
for f in "$manifest" packaging/flatpak/cargo-sources.json packaging/flatpak/node-sources.json; do
    [ -f "$f" ] || { echo "missing $f (tools/branding.py sync / tools/flatpak_sources.sh)" >&2; exit 1; }
done
VERSION="${VERSION:-$(grep -o '"version": *"[^"]*"' desktop/src-tauri/tauri.conf.json | sed -n '1s/.*"\([^"]*\)"$/\1/p')}"
VERSION="${VERSION:-dev}"

# Work tree inside the checkout but excluded from the manifest's dir source.
work="$root/.flatpak-builder"
mkdir -p "$work" "$out"
if [ "$(id -u)" = 0 ]; then where=--system; else where=--user; fi
opts=("$where" --force-clean)
if [ "${FLATPAK_INSTALL_DEPS:-1}" = 1 ]; then
    flatpak remote-add "$where" --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
    opts+=(--install-deps-from=flathub)
fi
if [ "${FLATPAK_INSTALL:-0}" = 1 ]; then opts+=(--install); fi

flatpak-builder "${opts[@]}" --disable-rofiles-fuse \
    --state-dir="$work/state" --repo="$work/repo" \
    --default-branch=stable "$work/build" "$manifest"
flatpak build-bundle "$work/repo" "$out/$APP_ID-$VERSION.flatpak" "$APP_ID" stable
if command -v python3 >/dev/null; then
    python3 packaging/check-package-contents.py "$work/build/files" || { rc=$?; [ "$rc" -eq 3 ] || exit "$rc"; }
fi
ls -l "$out/$APP_ID-$VERSION.flatpak"
