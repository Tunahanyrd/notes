#!/bin/sh
# Fetch the GPL-3.0 sdocx renderer source at the pinned commit into
# references/upstream/sdocx (tools/ink-renderer/build.rs verifies that this
# is a clean git checkout of exactly this commit). Idempotent: an existing
# checkout at the right commit is left alone; a checkout at another commit is
# moved to it only if its tree is clean.
#
#   tools/fetch_sdocx.sh            # default location
#   SDOCX_DIR=/path tools/fetch_sdocx.sh
set -eu

SDOCX_URL="${SDOCX_URL:-https://github.com/twangodev/sdocx}"
SDOCX_COMMIT=41e9c1099b29a48dc7320501573d7c4858f09303
root="$(cd "$(dirname "$0")/.." && pwd)"
dest="${SDOCX_DIR:-$root/references/upstream/sdocx}"

if [ -d "$dest/.git" ]; then
    head="$(git -C "$dest" rev-parse HEAD)"
    if [ "$head" = "$SDOCX_COMMIT" ]; then
        echo "sdocx already at $SDOCX_COMMIT ($dest)"
        exit 0
    fi
    if ! git -C "$dest" diff --quiet HEAD; then
        echo "error: $dest has local changes and is at $head, not $SDOCX_COMMIT" >&2
        exit 1
    fi
else
    if [ -e "$dest" ]; then
        echo "error: $dest exists but is not a git checkout" >&2
        exit 1
    fi
    mkdir -p "$(dirname "$dest")"
    git init -q "$dest"
    git -C "$dest" remote add origin "$SDOCX_URL"
fi

# Fetch just the pinned commit (GitHub allows fetching reachable SHAs).
git -C "$dest" fetch -q --depth 1 origin "$SDOCX_COMMIT"
git -C "$dest" -c advice.detachedHead=false checkout -q --detach "$SDOCX_COMMIT"
head="$(git -C "$dest" rev-parse HEAD)"
[ "$head" = "$SDOCX_COMMIT" ] || { echo "error: checkout ended at $head" >&2; exit 1; }
echo "sdocx at $SDOCX_COMMIT ($dest)"
