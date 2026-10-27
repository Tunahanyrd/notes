# Packaging verification

Local verification of the Linux packages, 2026-10-27. Nothing was published
(no GitHub release, no Flathub PR, no AUR push). All builds ran in throwaway
containers from the **public source export** (`tools/public_export.py`,
0 blocking findings, 727 files): that is the tree CI and users get, with
the free icon overlay instead of the private Samsung UI icons. Tests ran
offline (`docker --network none`) on a synthetic library
(`crates/notes-engine/conformance/gen_library.py`, 43 notes, two PDF-backed
pages); no Samsung account, network call or personal data was involved.

## What the packages contain

Every package ships the Tauri shell (`notes`) plus the **Python-free
`notes-engine` binary** next to it (the shell's Cargo feature `packaged`
makes that binary the default engine), the GPL ink renderer
(`ink-render`, sdocx pinned at 41e9c1099b29a48dc7320501573d7c4858f09303),
the built frontend including the live pen module `ink-live.wasm`
(crates/notes-ink-live, GPL-3.0-only, rebuilt from source by every package
build), LICENSE/NOTICE/THIRD_PARTY_LICENSES and AppStream metadata. No `python3`, no `samsung_notes/`, no PyO3 module, no Samsung
icons or fonts (checked by `packaging/check-package-contents.py`, which
compares every `windows-assets/` icon with the free set by hash and flags
known Samsung hashes and Segoe/Samsung fonts).

| format | bundled | required from the system |
|---|---|---|
| .deb (Ubuntu 22.04+, Debian 12+) | shell, notes-engine (SQLite static), ink-render, frontend | libwebkit2gtk-4.1-0, libgtk-3-0, libssl3, poppler-utils, bubblewrap, xdg-utils; Recommends imagemagick |
| .rpm (Fedora) | same | webkit2gtk4.1, gtk3, openssl-libs, poppler-utils, bubblewrap, xdg-utils; Recommends ImageMagick |
| AppImage (glibc >= 2.35) | same + WebKitGTK/GTK/libssl (linuxdeploy) | host Mesa `libgbm1`/`libEGL` (never bundled by linuxdeploy; present on every desktop), `pdftoppm` (PDF pages), `bwrap` (sandbox; falls back to rlimits), `magick` (Save as PDF), `xdg-open` |
| Arch (`notes`, `notes-git`) | shell, notes-engine, ink-render, frontend under /usr/lib/notes | webkit2gtk-4.1 gtk3 libsoup3 openssl sqlite poppler bubblewrap xdg-utils; optdepends imagemagick, librsvg |
| Flatpak (GNOME 50) | shell, notes-engine, ink-render, Poppler 26.10.0 (pdftoppm/pdfinfo), ImageMagick 7.1.2-34 | GNOME 50 runtime (WebKitGTK 4.1, SQLite, librsvg) |

The Flatpak's rust-stable SDK extension (rustc 1.98.1) has no wasm32
standard library, so the manifest adds the official
`rust-std-1.98.1-wasm32-unknown-unknown` tarball (sha256-pinned) in a
private sysroot used only for the wasm32 compile; the build stops with a
clear message when the extension moves to another rustc (bump the URL and
hash then).

External helpers are looked up at fixed paths (`/usr/bin/pdftoppm`,
`/usr/bin/bwrap`; Flatpak `/app/bin`), so the AppImage cannot carry its own
Poppler: it is a host requirement. Inside Flatpak (and in containers where
user namespaces are blocked) the engine reports
`renderer sandbox: bubblewrap disabled ...` and runs the renderers with
rlimits only (`NOTES_SANDBOX=direct`), as designed.

On Ubuntu, `python3` is pulled in only by WebKitGTK's *Recommends*
(`--no-install-recommends` installs none); the package itself has no
python dependency. Fedora's base image ships python3 for dnf. Neither is
used by the app.

## Results

Package sizes (version 0.1.0, x86_64; sha256 prefix):

| file | size | sha256 |
|---|---|---|
| Notes_0.1.0_amd64.AppImage | 95.9 MB (299 MB unpacked, 423 files) | 4c90e373e4c940de |
| Notes_0.1.0_amd64.deb | 16.8 MB (39.7 MB unpacked, 68 files) | 260a8003934e2963 |
| Notes-0.1.0-1.x86_64.rpm | 16.8 MB (39.7 MB unpacked, 68 files) | bd95217d71790f8f |
| notes-0.1.0-1-x86_64.pkg.tar.zst | 14.1 MB (33.4 MB unpacked, 72 files) | a0a533b98f8ddc96 |
| io.github.tunahanyrd.Notes-0.1.0.flatpak | 14.0 MB | b19ae968740e1d4f |

Build times on this machine (CARGO_BUILD_JOBS=4, warm cargo registry):
AppImage+deb+rpm in one Ubuntu 22.04 container 294 s; Arch makepkg 780 s
(clean target); Flatpak 669 s with warm flatpak-builder cache (first run
2805 s including Poppler/ImageMagick and SDK extension downloads).

Content check (`check-package-contents.py`, all five): python none, Samsung
assets none (49 UI icons, all byte-identical to the free set; a run against
the private tree's packages, with the provenance hash list, correctly
flagged the 49 Samsung icons).

Smoke tests (clean container per format, `--network none` during tests,
synthetic library):

| check | deb 22.04 | deb 24.04 | AppImage 22.04 | rpm Fedora 44 | Arch | Flatpak GNOME 50 |
|---|---|---|---|---|---|---|
| engine starts offline, runtime file | PASS | PASS | PASS | PASS | PASS | PASS |
| /api/status 200, /api/notes (43 ids), frontend | PASS | PASS | PASS | PASS | PASS | PASS |
| /ink-live.wasm served (valid module) | PASS | PASS | PASS | PASS | PASS | PASS |
| PDF page preview via Poppler | PASS | PASS | PASS | PASS | PASS | PASS (bundled pdftoppm) |
| ink page preview via ink-render | PASS | PASS | PASS | PASS | PASS | PASS |
| no python in engine tree / container | PASS | PASS | PASS | PASS | PASS | PASS (engine tree) |
| `<app> __notes-login-callback <synthetic>` forwarded to notes-engine, rejected cleanly (exit 1, fixed message, no window) | PASS | PASS | PASS (AppRun) | PASS | PASS | n/a (exported LoginCallback.desktop runs notes-engine directly) |
| GUI under Xvfb spawns notes-engine, listens on 127.0.0.1 | PASS | PASS | PASS (extracted AppRun; no FUSE in containers) | PASS | PASS | not run |

In the containers the engine logs `renderer sandbox: bubblewrap disabled`
(Docker blocks the user namespaces bwrap needs; inside Flatpak nested bwrap
is unavailable by design) and renders with rlimits only. The same packaged
`notes-engine` run directly on an Arch host with working bubblewrap used
the bwrap sandbox (no such line) and rendered the PDF and ink previews.


## Validation

- `appstreamcli validate --no-net packaging/linux/io.github.tunahanyrd.Notes.metainfo.xml`:
  passes (1 pedantic note: uppercase letters in the reverse-DNS id, which
  Flathub accepts).
- `desktop-file-validate`: `packaging/linux/desktop.hbs` rendered (the
  deb/rpm/AppImage `Notes.desktop`), `packaging/flatpak/io.github.tunahanyrd.Notes.desktop`,
  `packaging/flatpak/io.github.tunahanyrd.Notes.LoginCallback.desktop`: no
  errors or hints.
- `flatpak-builder-lint` (from the gnome-50 builder image): manifest and
  build output have no errors other than `appid-url-not-reachable` (the
  GitHub repository does not exist yet) and, for builddir/repo,
  `metainfo-missing-screenshots`; warning `runtime-update-available` (GNOME
  51 is out; 50 is what was built and tested).
- `makepkg --printsrcinfo` regenerated `packaging/arch/.SRCINFO` and
  `packaging/arch/notes-git/.SRCINFO`; no local paths in either.

## Commands

Build (CI runs the same scripts; see `.github/workflows/release.yml`):

    # AppImage / .deb / .rpm on the oldest supported base (Ubuntu 22.04)
    docker build -t notes-build:ubuntu-22.04 -f packaging/containers/build-ubuntu-22.04.Dockerfile .
    docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp -v "$PWD:/work" -w /work \
        notes-build:ubuntu-22.04 packaging/build-linux-bundles.sh appimage,deb,rpm dist
    # or one format: packaging/build-deb.sh dist   (build-appimage.sh, build-rpm.sh)

    # Flatpak (needs --privileged for flatpak-builder's bubblewrap)
    docker run --rm --privileged -v "$PWD:/work" -w /work \
        ghcr.io/flathub-infra/flatpak-github-actions:gnome-50 packaging/build-flatpak.sh dist

    # Arch, from this tree instead of the release tarball
    packaging/build-arch.sh dist          # on Arch; see the script for a container recipe

Check and smoke-test a built package:

    packaging/check-package-contents.py dist/*.deb dist/*.rpm dist/*.AppImage dist/*.pkg.tar.zst
    python3 crates/notes-engine/conformance/gen_library.py --out /tmp/synthlib
    packaging/smoke/container-smoke.sh deb      dist/Notes_*_amd64.deb      /tmp/synthlib ubuntu:24.04
    packaging/smoke/container-smoke.sh appimage dist/Notes_*_amd64.AppImage /tmp/synthlib ubuntu:22.04
    packaging/smoke/container-smoke.sh rpm      dist/Notes-*.x86_64.rpm     /tmp/synthlib fedora:latest
    packaging/smoke/container-smoke.sh arch     dist/notes-*.pkg.tar.zst    /tmp/synthlib archlinux:latest
    # Flatpak, inside the builder container after FLATPAK_INSTALL=1 packaging/build-flatpak.sh:
    packaging/smoke/flatpak-smoke.sh io.github.tunahanyrd.Notes /tmp/synthlib

Regenerate the Flatpak offline source lists after lockfile changes:

    tools/flatpak_sources.sh     # cargo-sources.json (3 lockfiles) + node-sources.json

## Open items

- Flathub submission needs: the public repository online (the app-id URL
  check), screenshots in the metainfo (hosted images), and optionally a move
  to GNOME 51 (rebuild + retest; WebKitGTK 4.1 availability must be
  checked first).
- `packaging/arch/PKGBUILD` uses the GitHub tag tarball with `SKIP`
  checksums; set the sha256 once tag v0.1.0 exists. `notes-git` builds from
  the default branch and was not built here (no repository yet); the
  `notes` package was built from this tree with `packaging/build-arch.sh`.
  The AUR name `notes` may already be taken; check before the first push.
- The deb/rpm/AppImage desktop file is named `Notes.desktop` (Tauri uses the
  product name), the Flatpak one `io.github.tunahanyrd.Notes.desktop`;
  the AppStream `launchable` follows each.
- Tauri installs the 256 px icon under `hicolor/256x256@2/` (its naming);
  the 512 px and scalable (Arch/Flatpak) icons cover large sizes.
- Not verified here: a real sign-in, Samsung Cloud sync and token renewal
  through any package (no network, by design of these tests); the AppImage
  through FUSE on a desktop; the Flatpak GUI window.
