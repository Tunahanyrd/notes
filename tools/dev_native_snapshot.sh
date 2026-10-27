#!/bin/sh
# Run the native app from a frozen copy of the pure-Rust engine + UI, so
# parallel work in the source tree (half-edited files) cannot break it.
#   tools/dev_native_snapshot.sh refresh   # build + copy notes-engine and desktop/dist, then restart
#   tools/dev_native_snapshot.sh restart   # restart on the existing copy
# LINUX_NOTES_ENGINE=embedded tools/dev_native_snapshot.sh restart  # in-process engine (embedded-engine build)
# The snapshot holds only bin/notes-engine and desktop/dist (plus symlinks to
# tools/ink-renderer, references, target, artifacts); there is no Python engine.
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
# XDG cache dir (spec: unset, empty or relative -> ~/.cache).
case ${XDG_CACHE_HOME:-} in /*) cache_home=$XDG_CACHE_HOME ;; *) cache_home=$HOME/.cache ;; esac
SNAP=${LINUX_NOTES_SNAPSHOT:-$cache_home/linux-notes-dev/snapshot}
BIN=$ROOT/desktop/src-tauri/target/debug/linux-notes-desktop
ENGINE=${LINUX_NOTES_ENGINE:-native}
mode=${1:-restart}

if [ "$mode" = refresh ]; then
    (cd "$ROOT/desktop" && npm run build >/dev/null)
    (cd "$ROOT" && cargo build -p notes-engine >/dev/null)
    next=$SNAP.next
    rm -rf "$next"; mkdir -p "$next/desktop" "$next/tools" "$next/bin"; chmod 700 "$next"
    cp -a "$ROOT/desktop/dist" "$next/desktop/"
    # Engine binary at a stable path inside the copy (<root>/bin is searched first).
    cp -a "$ROOT/target/debug/notes-engine" "$next/bin/"
    for link in tools/ink-renderer references target artifacts; do ln -s "$ROOT/$link" "$next/$link"; done
    "$next/bin/notes-engine" --help >/dev/null 2>&1 || { echo "notes-engine in $next does not run" >&2; exit 1; }
    rm -rf "$SNAP.old"; [ -d "$SNAP" ] && mv "$SNAP" "$SNAP.old"; mv "$next" "$SNAP"
fi
[ -x "$SNAP/bin/notes-engine" ] && [ -f "$SNAP/desktop/dist/index.html" ] || { echo "snapshot missing: run '$0 refresh'" >&2; exit 1; }

P=$(pgrep -f "target/debug/linux-notes-desktop$" || true)
[ -n "$P" ] && kill -TERM $P
for i in $(seq 40); do pgrep -f "notes-engine --port 0" >/dev/null || break; sleep 0.25; done
OLD=$(hyprctl activewindow -j | python3 -c "import json,sys;print(json.load(sys.stdin).get('address',''))")
# LINUX_NOTES_ENGINE is passed explicitly so an older desktop build (whose
# default was a Python child) still starts the pure engine.
env LINUX_NOTES_ENGINE=$ENGINE LINUX_NOTES_PROJECT_ROOT=$SNAP setsid -f "$BIN" >/dev/null 2>&1 </dev/null
for i in $(seq 80); do
    A=$(hyprctl clients -j | python3 -c "import json,sys;print(next((c['address'] for c in json.load(sys.stdin) if c['class']=='linux-notes-desktop'),''))")
    [ -n "$A" ] && break; sleep 0.25
done
[ -n "${A:-}" ] && hyprctl dispatch "hl.dsp.window.move({ workspace = \"8\", follow = false, window = \"address:$A\" })" >/dev/null
[ -n "$OLD" ] && hyprctl dispatch "hl.dsp.focus({ window = \"address:$OLD\" })" >/dev/null
echo "native app started from $SNAP (engine: $ENGINE)"
