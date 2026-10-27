#!/bin/sh
# Build the appimage bundle into OUT_DIR (default <root>/dist).
# See packaging/build-linux-bundles.sh for the environment contract.
set -eu
script="$(dirname "$0")/build-linux-bundles.sh"
if [ "$#" -gt 0 ]; then exec "$script" appimage "$1"; fi
exec "$script" appimage
