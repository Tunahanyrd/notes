# notes-engine — the Python-free engine

> Historical port log, written while the Python reference implementation was the test oracle (`NOTES_IMPL`, `tests/…`, `samsung_notes/…`). That reference has been retired and is not part of this repository; the engine is now gated by `cargo test` and the golden replay in `crates/notes-engine/conformance/golden`.

`notes-engine` (lib `notes_engine` + bin `notes-engine`) serves the same
loopback HTTP API as `python -m samsung_notes` over the same SQLite library,
using only the pure-Rust cores of the area crates. The binary does not link
libpython and never starts `python3` for engine work (`ldd`: libc, libgcc_s,
libm, libz, libsqlite3, libssl/libcrypto; `nm -u`: no `Py*`). External tools
stay as today: pdfinfo/pdftoppm (bwrap), rsvg-convert, ImageMagick, the GPL
ink-renderer CLI. Canvas-cache (SPI) pages are decoded in-process by
notes-preview's own decoder (`spi`, docs/CANVAS_CACHE.md, since 2026-10-27);
the former optional emulator path (a research venv running the user's native
library under Unicorn) is gone.

## Crate layout

| crate | role |
|---|---|
| `crates/notes-host` | `Fail`/`Kind` error (Python exception-class facts), `DocumentSnapshot` dict form, pure `rs_ops::deps::Deps` (`PureDeps`), pure `rs_store::host::Host` (`PureHost`), area facades (`editor_projection`, `project_ink` v5, `project_page_geometry`, ...) |
| `crates/notes-cloud` | pure cloud_api / cloud_metadata / cloud_ink_erase / cloud_page_ops incl. token renewal (`Transport`, `Renewal`, `Random` seams) |
| `crates/notes-net` | pure blocking HTTP/1.1 + TLS (openssl): `send_once`, previews `_http`, `fetch_pdf` download — urllib/http.client wire-identical |
| `crates/notes-login` | pure sign-in: `notes_login start/dispatch/install_handler`, callback capture/decrypt, `notes_auth_bridge` steps, registration, `SessionManager._login` |
| `crates/notes-preview` | pure PreviewManager / ThumbnailManager (+ PDF signed-link refresh) |
| `crates/notes-engine` | `Engine` (engine.py), `Sessions` (session.py), `RustBackend` (server.py dispatch over `rs_engine::server_core`), accept loop, launcher (`__main__.py`), adapters (`cloud_impl`, `net_impl`, `preview_impl`, `login_impl`), in-process `launcher::start` for Tauri |

## Oracle results (2026-10-27, historical)

Recorded while the Python reference engine still existed; it has since been
retired and is not part of the public repository. Today the gate is the
frozen golden replay described under "Conformance gate without Python".

- HTTP conformance harness (`conformance/run.py` against the former Python
  reference over a synthetic library): **1838/1838 requests equal (1750 byte-equal + 88
  masked-equal for generated ids/timestamps), 0 SQLite-table / data-file
  mismatches**, stable over repeated runs. Covers every GET route, static/font,
  security, malformed wire/body, and offline writes (notes, title, editor, ink,
  ink-erase, ink-move, page style/ops, PDF, templates incl. `?note=`, export,
  folders, organize, resolve, sync/login/disconnect, preferences, shortcuts v1/v2).
- Rust tests: notes-engine 17, notes-host 6 (incl. 1152-case Python
  differential fixture, recorded from the former reference),
  notes-cloud 14 (626 recorded oracle steps replayed), notes-net 33,
  notes-login 60, notes-preview 35 — 165 passing.

## Needs live validation by the user

The pure engine is opt-in until these pass on the real account:
1. First real HTTPS through `notes-net` (read-only `changes`/`get_note`), then
   one preview/PDF fetch (broker 302 → signed file URL, `is_global` check).
2. A full browser sign-in through the pure engine (ms-app handler switches from
   python to `notes-engine __notes-login-callback`), registration POST.
3. Token renewal (`notes-cloud` `PortedRenewal`), only mock/oracle-tested.
4. One controlled write round (same as the Rust-extension live round).

## Running without Python

    CARGO_BUILD_JOBS=4 cargo build -p notes-engine          # or --release
    target/debug/notes-engine [--offline] [--no-auto-sync] --data-dir DIR \
        --port 0 --runtime-file RT.json --no-browser [--frontend-dir DIST]

Desktop shell (desktop/src-tauri): since 2026-10-27 the Linux default spawns
`notes-engine` (LINUX_NOTES_ENGINE_BIN, next to the executable, `<root>/bin`,
`<root>/target/{release,debug}`, PATH); build with `--features
embedded-engine` and set `LINUX_NOTES_ENGINE=embedded` to run it in-process.
The Python child mode is gone; the former Python engine is frozen under
`reference/python/` (not product).

## Conformance gate without Python (2026-10-27)

`conformance/golden/` holds the responses and final state of the Python
reference over the committed synthetic library, recorded after a live
1968/1968 PASS (plus a `no-magick` variant). Product gate:

    cargo build -p notes-engine
    python3 crates/notes-engine/conformance/run.py --replay --candidate target/debug/notes-engine
    # or: cargo test -p notes-engine --test golden_conformance -- --ignored

Result 2026-10-27: 1968/1968 (1874 byte-equal + 94 masked-equal), 0 state
mismatches. The canvas-cache (SPI) decoder (`codec.rs`) no longer starts the
Python worker.

Intended divergence (2026-10-27, built-in SPI decoder): `GET /api/notes/:id/pages`
now reports `providerAvailable: true` where the recording (made without the
historical optional decoder) has `false` — for notes with canvas-cache pages or
with no preview candidates, exactly as the Python app behaved with its decoder
installed. Replay after the change: 1919/1968 equal (1825 byte-equal + 94
masked-equal); the 49 differences are all this one field. Re-bless with
`--record --reference target/debug/notes-engine` once reviewed. (A separate
state mismatch on the exported `Downloads/Synthetic ink.pdf` was already
present before this change and is unrelated to the decoder.)

## Platforms

OS-specific behaviour (data/state/downloads directories, opener, PDEATHSIG,
rlimits, tool lookup, loopback socket, ms-app registration, the preview
sandbox fallback) goes through `crates/notes-platform`; Linux results are the
ones documented above. macOS: the desktop shell always links this engine
in-process and receives the sign-in callback as an Apple Event
(`deliver_login_callback`); see docs/MACOS_PORT.md.
