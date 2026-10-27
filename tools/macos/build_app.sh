#!/usr/bin/env bash
# Build the macOS app bundle and, with --dmg, an unsigned disk image
# `<AppName>-<version>-<arch>.dmg` (+ `.sha256`) from this checkout.
# Not signed, not notarized; see docs/MACOS_PORT.md ("Gatekeeper").
#
#   tools/macos/build_app.sh [--release] [--dmg] [--version X.Y.Z] [--out DIR]
#                            [--with-ink-renderer] [--no-wasm] [--keep-private-icons]
#
#   --release              optimized build (default: debug, faster to compile)
#   --dmg                  also produce the .dmg in --out (default:
#                          desktop/src-tauri/target/<profile>/bundle/dmg)
#   --version X.Y.Z        app/dmg version (default: tauri.conf.json "version")
#   --with-ink-renderer    build and bundle the GPL ink-render CLI
#   --no-wasm              skip the live pen WASM (dist/ink-live.wasm); not
#                          allowed together with --dmg
#   --keep-private-icons   keep desktop/public/windows-assets as-is instead of
#                          the free Lucide set (Samsung icons must never leave
#                          your machine; refused together with --dmg)
#
# The architecture is the host's (arm64 on Apple Silicon, x86_64 on Intel):
# OpenSSL is linked statically from the host's Homebrew prefix, which holds
# one architecture only, so a universal binary would need a second OpenSSL
# (CI builds arm64 and x86_64 on separate runners instead).
# The pure engine is linked into the app (no Python, no sidecar); SQLite and
# OpenSSL are static. Poppler/librsvg/ImageMagick stay external (brew).
set -euo pipefail
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
profile=debug; dmg=0; ink=0; wasm=1; private_icons=0; version=""; out=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --release) profile=release ;;
    --dmg) dmg=1 ;;
    --version) version=${2:?--version needs a value}; shift ;;
    --out) out=${2:?--out needs a directory}; shift ;;
    --with-ink-renderer) ink=1 ;;
    --no-wasm) wasm=0 ;;
    --keep-private-icons) private_icons=1 ;;
    -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

# --- preconditions: stop before touching anything if the host is wrong.
[ "$(uname -s)" = Darwin ] || { echo "error: macOS only (this is $(uname -s))" >&2; exit 2; }
if [ "$dmg" = 1 ] && [ "$wasm" = 0 ]; then echo "error: a .dmg must contain ink-live.wasm (drop --no-wasm)" >&2; exit 2; fi
if [ "$dmg" = 1 ] && [ "$private_icons" = 1 ]; then echo "error: refusing to put Samsung UI icons into a .dmg" >&2; exit 2; fi
xcode-select -p >/dev/null 2>&1 || { echo "error: Xcode Command Line Tools missing: xcode-select --install" >&2; exit 2; }
for tool in cargo rustc npm node python3 hdiutil plutil otool lipo shasum; do
  command -v "$tool" >/dev/null || { echo "error: $tool not found (see docs/MACOS_PORT.md prerequisites)" >&2; exit 2; }
done
if ! brew --prefix openssl@3 >/dev/null 2>&1 && [ -z "${OPENSSL_DIR:-}" ]; then
  echo "error: OpenSSL 3 not found: brew install openssl@3 (or set OPENSSL_DIR)" >&2; exit 2
fi
if [ "$wasm" = 1 ] && ! ls "$(rustc --print target-libdir --target wasm32-unknown-unknown 2>/dev/null)"/libstd-*.rlib >/dev/null 2>&1; then
  echo "error: wasm32-unknown-unknown std missing: rustup target add wasm32-unknown-unknown" >&2; exit 2
fi
for pair in pdftoppm:poppler rsvg-convert:librsvg magick:imagemagick; do
  bin=${pair%%:*}; formula=${pair#*:}
  if ! PATH="/opt/homebrew/bin:/usr/local/bin:/opt/local/bin:$PATH" command -v "$bin" >/dev/null; then
    echo "warning: $bin missing (runtime only): brew install $formula" >&2
  fi
done
case "$(uname -m)" in arm64) arch=arm64 ;; x86_64) arch=x86_64 ;; *) echo "error: unsupported arch $(uname -m)" >&2; exit 2 ;; esac
[ -n "$version" ] || version=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$root/desktop/src-tauri/tauri.conf.json")
case "$version" in *[!0-9A-Za-z.+-]*|'') echo "error: bad version: $version" >&2; exit 2 ;; esac
app_name=$(python3 "$root/tools/branding.py" get appName 2>/dev/null || echo Notes)
dmg_base=$(printf '%s' "$app_name" | tr -c 'A-Za-z0-9.-' '-' | sed 's/--*/-/g; s/^-//; s/-$//')
[ -n "$dmg_base" ] || dmg_base=Notes

export OPENSSL_STATIC=${OPENSSL_STATIC:-1}
[ -n "${OPENSSL_DIR:-}" ] || export OPENSSL_DIR="$(brew --prefix openssl@3)"

# --- live pen preview core (GPL sdocx) -> desktop/public/ink-live.wasm
if [ "$wasm" = 1 ]; then
  "$root/tools/fetch_sdocx.sh"
  "$root/tools/build_ink_wasm.sh"
fi

# --- frontend
cd "$root/desktop"
[ -d node_modules ] || npm ci
npm run build
if [ "$private_icons" = 0 ]; then
  if [ -d assets-free/windows-assets ]; then
    rm -rf dist/windows-assets
    cp -R assets-free/windows-assets dist/windows-assets
    echo "==> UI icons: free Lucide set (desktop/assets-free)"
  else
    echo "warning: desktop/assets-free/windows-assets missing; UI icons will be blank" >&2
    rm -rf dist/windows-assets
  fi
else
  echo "warning: --keep-private-icons: this bundle contains Samsung UI icons; do NOT share it" >&2
fi
if [ "$wasm" = 1 ] && [ ! -s dist/ink-live.wasm ]; then echo "error: dist/ink-live.wasm missing after the frontend build" >&2; exit 1; fi

# --- optional GPL ink renderer
extra_resources=""
if [ "$ink" = 1 ]; then
  [ "$wasm" = 1 ] || "$root/tools/fetch_sdocx.sh"
  cargo build --locked --release --manifest-path "$root/tools/ink-renderer/Cargo.toml"
  extra_resources=',"../../tools/ink-renderer/target/release/ink-render":"tools/ink-renderer/target/release/ink-render","../../tools/ink-renderer/LICENSE":"licenses/ink-render-LICENSE"'
fi

# --- bundle the .app (frontend already built: beforeBuildCommand disabled)
config=$(mktemp "${TMPDIR:-/tmp}/notes-tauri-config.XXXXXX")
stage=""; mnt=""
cleanup() { [ -n "$mnt" ] && hdiutil detach -quiet "$mnt" 2>/dev/null || true; rm -rf "$config" ${stage:+"$stage"}; }
trap cleanup EXIT
cat > "$config" <<JSON
{"version":"$version","build":{"beforeBuildCommand":""},"bundle":{"resources":{"../dist/":"desktop/dist/","../../LICENSE":"licenses/LICENSE","../../NOTICE":"licenses/NOTICE"${extra_resources}}}}
JSON
debug_flag=(); [ "$profile" = debug ] && debug_flag=(--debug)
npx tauri build ${debug_flag[@]+"${debug_flag[@]}"} --bundles app --config "$config"

# --- verification (applied to the built .app and to the copy inside the .dmg)
verify_app() {
  local app=$1 res exe
  res="$app/Contents/Resources"
  exe=$(plutil -extract CFBundleExecutable raw -o - "$app/Contents/Info.plist")
  [ -f "$res/desktop/dist/index.html" ] || { echo "error: $res/desktop/dist/index.html missing" >&2; return 1; }
  if [ "$wasm" = 1 ]; then
    [ -s "$res/desktop/dist/ink-live.wasm" ] || { echo "error: ink-live.wasm missing from the bundle" >&2; return 1; }
    [ "$(head -c 4 "$res/desktop/dist/ink-live.wasm" | od -An -tx1 | tr -d ' ')" = 0061736d ] \
      || { echo "error: ink-live.wasm is not a WebAssembly module" >&2; return 1; }
  fi
  plutil -extract CFBundleURLTypes xml1 -o - "$app/Contents/Info.plist" | grep -q '<string>ms-app</string>' \
    || { echo "error: ms-app URL scheme missing from Info.plist" >&2; return 1; }
  [ "$(plutil -extract CFBundleShortVersionString raw -o - "$app/Contents/Info.plist")" = "$version" ] \
    || { echo "error: bundle version is not $version" >&2; return 1; }
  if [ "$private_icons" = 0 ] && [ -f "$root/desktop/windows-assets-provenance.json" ]; then
    python3 - "$root/desktop/windows-assets-provenance.json" "$res/desktop/dist" <<'PY' || return 1
import hashlib, json, pathlib, sys
samsung = {e["sha256"] for e in json.load(open(sys.argv[1])) if isinstance(e, dict) and "sha256" in e}
hits = [p for p in pathlib.Path(sys.argv[2]).rglob("*") if p.is_file() and hashlib.sha256(p.read_bytes()).hexdigest() in samsung]
if hits:
    sys.exit(f"error: Samsung UI icons found in the bundle: {len(hits)} files")
PY
  fi
  if otool -L "$app/Contents/MacOS/$exe" | grep -E '/opt/homebrew|/usr/local/(opt|Cellar)|/opt/local/' >&2; then
    echo "error: the executable links package-manager libraries (not portable)" >&2; return 1
  fi
  # macOS always embeds the pure engine (no notes-engine sidecar, no Python).
  grep -aq 'Bu yerel arşiv için Notlar hizmeti zaten açık.' "$app/Contents/MacOS/$exe" \
    || { echo "error: the embedded notes-engine is missing from $exe" >&2; return 1; }
  if [ -e "$app/Contents/MacOS/notes-engine" ]; then echo "error: unexpected notes-engine sidecar" >&2; return 1; fi
  lipo -archs "$app/Contents/MacOS/$exe" | tr ' ' '\n' | grep -qx "$arch" \
    || { echo "error: executable is not $arch ($(lipo -archs "$app/Contents/MacOS/$exe"))" >&2; return 1; }
}

app=$(ls -d "$root/desktop/src-tauri/target/$profile/bundle/macos/"*.app | head -1)
[ -d "$app" ] || { echo "error: no .app produced" >&2; exit 1; }
verify_app "$app"
echo "==> built: $app ($arch, $version)"

if [ "$dmg" = 1 ]; then
  out=${out:-$root/desktop/src-tauri/target/$profile/bundle/dmg}
  mkdir -p "$out"
  dmg_path="$out/$dmg_base-$version-$arch.dmg"
  stage=$(mktemp -d "${TMPDIR:-/tmp}/notes-dmg.XXXXXX")
  ditto "$app" "$stage/$(basename "$app")"
  ln -s /Applications "$stage/Applications"
  rm -f "$dmg_path"
  hdiutil create -quiet -volname "$app_name $version" -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$dmg_path"
  # Re-verify what users will actually get: the .app inside the image.
  mnt=$(mktemp -d "${TMPDIR:-/tmp}/notes-dmg-mnt.XXXXXX")
  hdiutil attach -quiet -nobrowse -readonly -mountpoint "$mnt" "$dmg_path"
  [ -L "$mnt/Applications" ] || { echo "error: /Applications link missing in the dmg" >&2; exit 1; }
  verify_app "$mnt/$(basename "$app")"
  hdiutil detach -quiet "$mnt"; mnt=""
  (cd "$out" && shasum -a 256 "$(basename "$dmg_path")" > "$(basename "$dmg_path").sha256")
  echo "==> dmg: $dmg_path"
  cat "$dmg_path.sha256"
fi
echo "Unsigned, not notarized: first launch with right-click > Open (or: xattr -dr com.apple.quarantine <app>)."
