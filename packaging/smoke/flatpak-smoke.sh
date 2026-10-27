#!/usr/bin/env bash
# Smoke test of an INSTALLED Flatpak (run where `flatpak run` works, e.g.
# inside the flatpak-builder container right after build-flatpak.sh with
# FLATPAK_INSTALL=1, as a non-root user). Offline engine mode; the engine inside the sandbox runs
# its renderers in rlimit-only "direct" mode (no nested bubblewrap).
#
#   packaging/smoke/flatpak-smoke.sh APP_ID LIBRARY [ROOT]   # ROOT default /app/lib/notes
set -euo pipefail
app="$1"; library="$2"; root="${3:-/app/lib/notes}"
# As root the sandboxed engine cannot use files the host side created
# (observed: "Permission denied"); run as an ordinary user.
[ "$(id -u)" != 0 ] || { echo "run flatpak-smoke.sh as a non-root user" >&2; exit 2; }
here="$(cd "$(dirname "$0")" && pwd)"
base="$HOME/.var/app/$app/smoke"
rm -rf "$base" && mkdir -p "$base"
cp -a "$library" "$base/lib" && chmod -R go-rwx "$base/lib"
wrapper="$base/engine.sh"
cat > "$wrapper" <<SH
#!/bin/sh
exec flatpak run --env=LINUX_NOTES_PROJECT_ROOT="\$LINUX_NOTES_PROJECT_ROOT" --command=notes-engine "$app" "\$@"
SH
chmod +x "$wrapper"
echo "== flatpak $app ($(flatpak info "$app" | sed -n 's/^ *Runtime: //p'))"
rc=0
SMOKE_WORK="$base/work" "$here/smoke-engine.sh" "$wrapper" "$root" \
    "$base/lib/main" 6dfd4bbf-e334-4e80-b37b-93f461f09aa3 0e34b3bb-2a10-4ed8-a414-a3b820aa2c55 || rc=$?
for t in pdftoppm magick notes-engine notes; do
    flatpak run --command=sh "$app" -c "command -v $t >/dev/null" && echo "PASS /app has $t" || { echo "FAIL $t missing in sandbox"; rc=1; }
done
flatpak run --command=sh "$app" -c 'command -v python3 >/dev/null' \
    && echo "INFO the GNOME runtime ships python3 (not used by the app)" || echo "INFO no python3 in runtime"
exit "$rc"
