#!/bin/sh
# Regenerate the offline source lists the Flatpak manifest consumes:
#
#   packaging/flatpak/cargo-sources.json  <- Cargo.lock (notes-engine binary)
#                                            + desktop/src-tauri/Cargo.lock
#                                            + tools/ink-renderer/Cargo.lock
#                                            + crates/notes-ink-live/Cargo.lock
#   packaging/flatpak/node-sources.json   <- desktop/package-lock.json
#
# flatpak-builder runs without network, so every crate and npm tarball must
# be listed with its checksum. Run this after any change to those lockfiles
# and commit the two JSON files. The generators come from
# flatpak/flatpak-builder-tools at a pinned commit and run in a throwaway
# virtualenv (needs network for git/PyPI only; nothing is installed system-wide).
#
#   tools/flatpak_sources.sh
set -eu

TOOLS_URL=https://github.com/flatpak/flatpak-builder-tools
TOOLS_COMMIT=9d86d336a39b9af38b86f114b6ce5820fe260f91
root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/packaging/flatpak"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT INT TERM

git init -q "$work/fbt"
git -C "$work/fbt" remote add origin "$TOOLS_URL"
git -C "$work/fbt" fetch -q --depth 1 origin "$TOOLS_COMMIT"
git -C "$work/fbt" -c advice.detachedHead=false checkout -q --detach "$TOOLS_COMMIT"

python3 -m venv "$work/venv"
"$work/venv/bin/pip" install -q aiohttp tomlkit

"$work/venv/bin/python" "$work/fbt/cargo/flatpak-cargo-generator.py" \
    "$root/Cargo.lock" -o "$work/cargo-engine.json"
"$work/venv/bin/python" "$work/fbt/cargo/flatpak-cargo-generator.py" \
    "$root/desktop/src-tauri/Cargo.lock" -o "$work/cargo-tauri.json"
"$work/venv/bin/python" "$work/fbt/cargo/flatpak-cargo-generator.py" \
    "$root/tools/ink-renderer/Cargo.lock" -o "$work/cargo-ink.json"
"$work/venv/bin/python" "$work/fbt/cargo/flatpak-cargo-generator.py" \
    "$root/crates/notes-ink-live/Cargo.lock" -o "$work/cargo-ink-live.json"

# One list for all lockfiles: identical crate entries appear once.
"$work/venv/bin/python" - "$work/cargo-engine.json" "$work/cargo-tauri.json" "$work/cargo-ink.json" "$work/cargo-ink-live.json" "$out/cargo-sources.json" <<'PY'
import json, sys
merged, seen = [], set()
for path in sys.argv[1:-1]:
    for source in json.load(open(path)):
        key = json.dumps(source, sort_keys=True)
        if key not in seen:
            seen.add(key)
            merged.append(source)
json.dump(merged, open(sys.argv[-1], 'w'), indent=4)
open(sys.argv[-1], 'a').write('\n')
print(f'cargo-sources.json: {len(merged)} entries')
PY

# npm: plain tarball list from the lockfile (no registry metadata needed; see
# packaging/flatpak/npm_sources.py - flatpak-node-generator fetched every
# packument and was unreliable on slow links).
python3 "$root/packaging/flatpak/npm_sources.py" "$root/desktop/package-lock.json" -o "$out/node-sources.json"
echo "wrote $out/cargo-sources.json and $out/node-sources.json"
