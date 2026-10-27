#!/usr/bin/env bash
# Headless smoke test of an INSTALLED package (run inside a clean container
# or sandbox; needs only sh, curl, ps and grep - deliberately no python3).
#
#   smoke-engine.sh ENGINE ROOT LIBRARY [PDF_NOTE_ID [INK_NOTE_ID]]
#
#   ENGINE   the shipped notes-engine executable
#   ROOT     the resource root the shell would pass (dir with desktop/dist)
#   LIBRARY  a WRITABLE copy of a synthetic library directory
#            (crates/notes-engine/conformance/gen_library.py --out X; use X/main)
#   PDF_NOTE_ID  note with PDF background pages (gen_library: pdf_template;
#            page 1 is rendered - page 0 of that note has no preview)
#   INK_NOTE_ID  handwritten note whose page 0 is drawn by the bundled GPL
#            ink-render CLI (gen_library: ink_render)
#
# Starts the engine offline (--offline --no-auto-sync: no network at all),
# checks /api/status, the notes list, one PDF page preview (Poppler), and that
# no python process exists. Prints one line per check; exit 1 on failure.
set -uo pipefail
engine="$1"; root="$2"; lib="$3"; pdf="${4:-}"; ink="${5:-}"
# SMOKE_WORK: a directory visible to the engine (e.g. inside a Flatpak's
# ~/.var/app/<id>, whose /tmp is private).
work="${SMOKE_WORK:-$(mktemp -d)}"
mkdir -p "$work"
fail=0
ok() { echo "PASS $*"; }
bad() { echo "FAIL $*"; fail=1; }

LINUX_NOTES_PROJECT_ROOT="$root" "$engine" --offline --no-auto-sync --data-dir "$lib" \
    --port 0 --runtime-file "$work/rt.json" --no-browser >"$work/engine.log" 2>&1 &
pid=$!
i=0
while [ ! -s "$work/rt.json" ] && [ $i -lt 150 ]; do sleep 0.2; i=$((i + 1)); done
if [ ! -s "$work/rt.json" ]; then
    bad "engine did not write its runtime file"; cat "$work/engine.log"; kill "$pid" 2>/dev/null; exit 1
fi
port="$(grep -o '"port": *[0-9]*' "$work/rt.json" | grep -o '[0-9]*$')"
token="$(grep -o '"token": *"[^"]*"' "$work/rt.json" | sed 's/.*"\([^"]*\)"$/\1/')"
ok "engine started (pid $pid, 127.0.0.1:$port)"
api() { curl -s -o "$work/body" -w '%{http_code} %{content_type} %{size_download}' \
          -H "Host: 127.0.0.1:$port" -H "Authorization: Bearer $token" "http://127.0.0.1:$port$1"; }

r="$(api /api/status)"
case "$r" in 200*) ok "/api/status $r";; *) bad "/api/status $r";; esac
r="$(api /api/notes)"
n="$(grep -o '"id": *"[^"]*"' "$work/body" | wc -l)"
case "$r" in 200*) [ "$n" -gt 0 ] && ok "/api/notes $r ($n ids)" || bad "/api/notes empty";; *) bad "/api/notes $r";; esac
r="$(api /)"
case "$r" in 200*text/html*) ok "frontend / $r";; *) bad "frontend / $r";; esac
r="$(api /ink-live.wasm)"
if [ "${r%% *}" = 200 ] && [ "$(head -c 4 "$work/body" | od -An -c | tr -d ' ')" = '\0asm' ]; then ok "live pen module /ink-live.wasm $r"; else bad "live pen module /ink-live.wasm $r"; fi

preview() {  # NOTE PAGE LABEL
    local i=0 r
    while :; do
        r="$(api "/api/notes/$1/preview?page=$2")"
        case "$r" in 202*) [ $i -lt 60 ] || break; sleep 0.5; i=$((i + 1));; *) break;; esac
    done
    if [ "$(head -c 4 "$work/body" | tail -c 3)" = PNG ]; then ok "$3 $r"; else bad "$3 $r: $(head -c 300 "$work/body")"; fi
}
[ -z "$pdf" ] || preview "$pdf" 1 "PDF page preview (Poppler)"
[ -z "$ink" ] || preview "$ink" 0 "ink page preview (ink-render)"

# The engine and every process it started (renderer helpers are short-lived).
tree() { echo "$1"; for c in $(ps -eo pid=,ppid= | awk -v p="$1" '$2 == p {print $1}'); do tree "$c"; done; }
names="$(for p in $(tree "$pid"); do ps -o comm= -p "$p"; done | sort | uniq -c | tr -s ' \n' ' ')"
if [ -r "/proc/$pid/maps" ] && grep -q 'libpython' "/proc/$pid/maps"; then bad "engine maps libpython"; fi
if [ -n "$names" ] && echo "$names" | grep -i python >/dev/null; then bad "python in engine process tree: $names"; else ok "no python in engine process tree ($names)"; fi
if [ "${SMOKE_IN_CONTAINER:-0}" = 1 ] && ps -eo comm= | grep -i python >/dev/null; then
    bad "python process running in the container: $(ps -eo comm= | grep -i python | tr '\n' ' ')"
fi
command -v python3 >/dev/null && echo "INFO python3 exists on this system (not used)" || echo "INFO python3 not installed on this system"
if grep -qi 'bubblewrap disabled' "$work/engine.log"; then echo "INFO $(grep -i -m1 'bubblewrap disabled' "$work/engine.log")"; fi
kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
[ -n "${SMOKE_WORK:-}" ] || rm -rf "$work"
exit "$fail"
