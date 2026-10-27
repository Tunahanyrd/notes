# Notes

Samsung Notes compatible · unofficial

An unofficial desktop client for Linux, compatible with Samsung Notes. It
signs in with a Samsung account, syncs notes with Samsung Cloud and lets you
read and edit them in a native window — no Wine, Windows VM or Android
emulator involved. A macOS port is in progress.

_Screenshots: to be added before the first release (synthetic demo notes only)._

## Disclaimer

This is an independent, unofficial project. It is not affiliated with,
endorsed by, or sponsored by Samsung Electronics Co., Ltd. "Samsung",
"Samsung Notes" and "Galaxy" are trademarks of Samsung Electronics Co., Ltd.,
used here only to describe compatibility.

This client was implemented independently to interoperate with Samsung Notes. The file formats and protocol were documented through interoperability analysis (network observation, file inspection and static analysis of the official clients); the project contains no Samsung code, binaries or assets. Third-party open-source code that the project
uses — for example the GPL-3.0 [sdocx](https://github.com/twangodev/sdocx)
renderer library — is not Samsung code and is attributed in [NOTICE](NOTICE).

The client talks to Samsung's private, undocumented services. They can change
at any time and break the client without notice, and using a third-party
client may not be permitted by the terms of the service you connect to. You
are solely responsible for how you use this software and for complying with
the terms of any third-party service, including Samsung's. Every Cloud write
is verified by reading the whole note back, but data loss is still possible;
keep backups of notes that matter.

The software is provided "as is", without warranty of any kind, as stated in
sections 15 and 16 of the GNU General Public License v3. The authors are not
liable for account actions, data loss or any other consequences of its use.
Details: [docs/LEGAL.md](docs/LEGAL.md).

## Status

Features are listed with the strongest evidence that exists for them.
"Cloud-verified" means a controlled write to a dedicated test note was
accepted by Samsung Cloud and confirmed by a full readback; "seen on device"
means the result was also checked in the official app on a phone or tablet.

| Feature | Evidence |
|---|---|
| Browser sign-in (Samsung account) and session reuse | Live, including the pure Rust engine |
| Download sync into a local SQLite library, offline reading | Live |
| Plain-text notes: create, edit, upload | Cloud-verified, seen on device |
| Rich text (bold, italic, colour, alignment) | Cloud-verified, seen on device (limited set of styles) |
| Title edits that preserve ink and body | Cloud-verified |
| Favorites, trash/restore, folders (create, rename, move, trash empty folder) | Cloud-verified |
| Ink: add strokes, first stroke on an empty page, eraser, lasso move | Cloud-verified, seen on device |
| Ink on PDF pages | Cloud-verified, seen on device |
| Page background colour, add / move / delete pages, insert PDF, templates | Cloud-verified, seen on device |
| Page previews and thumbnails (sandboxed renderers) | Works offline |
| Token renewal | Implemented and tested against recorded exchanges; not yet observed live |
| Highlighter, shapes | In progress |

Not supported (by design, for now): permanent delete, locked/encrypted notes,
notes with embedded images for ink editing, creating new pen presets. The
browser ink preview is drawn by the client and is not pixel-identical to the
official renderer.

## Install

Release builds (AppImage, .deb, .rpm and a Flatpak bundle, each with a
`SHA256SUMS` entry) will be attached to
[GitHub Releases](https://github.com/Tunahanyrd/notes/releases) —
coming with the first tagged release.

**macOS** (`<AppName>-<version>-arm64.dmg`, plus `-x86_64.dmg` when built):
the disk image is **unsigned and not notarized**, so Gatekeeper blocks the
first launch. Check it against `SHA256SUMS`, drag the app to Applications,
then right-click it → **Open** (or run
`xattr -dr com.apple.quarantine "/Applications/<AppName>.app"`). Poppler,
librsvg and ImageMagick are optional runtime tools:
`brew install poppler librsvg imagemagick`. Details: docs/MACOS_PORT.md.

## Build from source

Requirements: Rust (stable, 1.92+), Node.js 22+, Python 3 (standard library
only, for helper scripts such as the conformance harness and branding/license
checks — the application itself contains no Python), and the Tauri 2 system
libraries (webkit2gtk-4.1, gtk3,
libsoup3). Runtime tools used by previews and export: `pdfinfo`/`pdftoppm`
(poppler-utils), `rsvg-convert` (librsvg), `magick` (ImageMagick) and
`bwrap` (bubblewrap; optional inside Flatpak/macOS, where a reduced sandbox is
used).

```sh
# 1. Fetch the GPL-3.0 renderer library at the pinned commit
tools/fetch_sdocx.sh

# 2. Engine and renderer
cargo build --release -p notes-engine
cargo build --release --manifest-path tools/ink-renderer/Cargo.toml

# 3. Frontend
(cd desktop && npm ci && npm run build)

# 4. Desktop shell; on Linux it starts the notes-engine binary from step 2
#    (LINUX_NOTES_ENGINE=embedded with --features embedded-engine runs it in-process)
cargo run --release --manifest-path desktop/src-tauri/Cargo.toml
```

The engine is pure Rust (`crates/notes-*` on top of the plain-Rust area
cores in `crates/rs-*`). An earlier implementation served as the behavioural
reference while the engine was written; its responses are frozen as golden
fixtures in `crates/notes-engine/conformance/golden/`, which every change is
checked against.

The engine can also run on its own and serve the UI in a browser:

```sh
target/release/notes-engine --data-dir /tmp/notes-demo --offline --port 0 \
    --runtime-file /tmp/notes-demo/runtime.json --frontend-dir desktop/dist
```

A container with every build and test dependency is described in
[docs/DOCKER.md](docs/DOCKER.md). Packages are built by the scripts in
[`packaging/`](packaging/): [`build-linux-bundles.sh`](packaging/build-linux-bundles.sh)
(AppImage, .deb, .rpm via the Tauri bundler) and
[`build-flatpak.sh`](packaging/build-flatpak.sh) (Flatpak bundle from
`packaging/flatpak/`).

## Tests

See [CONTRIBUTING.md](CONTRIBUTING.md). In short: `cargo test --workspace`,
the notes-engine HTTP conformance replay against frozen golden fixtures (must
stay at 100 %) and the desktop type check + Playwright suite. CI runs all of
them on every pull request (`.github/workflows/ci.yml`).

```sh
cargo test --workspace
cargo build -p notes-engine && python3 crates/notes-engine/conformance/run.py --replay --candidate target/debug/notes-engine
(cd desktop && npx tsc --noEmit && npm run build && npx playwright test)
```

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — components, crates, data flow
- [CONTRIBUTING.md](CONTRIBUTING.md) — workflow, write-safety and privacy rules
- [docs/EDITOR_FORMAT.md](docs/EDITOR_FORMAT.md), `docs/INK_*.md` — format notes
- [SECURITY.md](SECURITY.md) — reporting vulnerabilities

## Branding

The product name ("Notes"), application id (`io.github.tunahanyrd.Notes`),
binary name and repository URL live only in [`branding.json`](branding.json).
After changing them run

```sh
python3 tools/branding.py sync
```

which regenerates the packaging files (Flatpak manifest, desktop entry,
metainfo). CI fails if they are out of sync.

## License

Our code is GPL-3.0-or-later (see [LICENSE](LICENSE)). The ink renderer
(`ink-render`) and the live pen preview (`notes-ink-live`) link the
GPL-3.0-only sdocx library, so those two binaries are GPL-3.0-only as a
whole; attribution and third-party licenses are listed in
[NOTICE](NOTICE) and the generated third-party license file.
